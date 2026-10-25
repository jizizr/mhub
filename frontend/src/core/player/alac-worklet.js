import processorUrl from "./alac-processor.js?url";
import initAlacWasm, { AlacDecoder } from "../../wasm/alac-decoder/alac_decoder.js";
import { createAlacOutput, getOutputContext, prepareOutput } from "./audio-output.js";
import { canRequestAlacWorklet, canUseAlacWorklet, isAlacWorkletSupported, isLosslessTrack } from "./alac-support.js";

const PCM_QUEUE_SECONDS = 2;
const BACKGROUND_PCM_QUEUE_SECONDS = 15;
const MAX_PCM_QUEUE_BYTES = 24 * 1024 * 1024;
const DECODE_SLICE_MS = 8;
const DECODE_SLICE_PACKETS = 64;
// After EOF packets are decoded from the cached Blob. Read it in windows: one
// async Blob read per packet (20-90 ms of audio) could not keep a Hi-Res
// queue filled while the page was busy.
const BLOB_READ_WINDOW_BYTES = 2 * 1024 * 1024;
const STARTUP_QUEUE_SECONDS = 0.5;
const RESUME_TIMEOUT_MS = 1500;
const RANGE_HANDOFF_TIMEOUT_MS = 8000;
const HEADER_BYTES = 24;
const STREAM_MAGIC = "SPLALAC1";
const STREAM_VERSION = 2;
let wasmReady = null;
const contextModules = new WeakMap();

function prepareModule(context) {
  let pending = contextModules.get(context);
  if (!pending) {
    pending = context.audioWorklet.addModule(processorUrl).catch((error) => {
      contextModules.delete(context);
      throw error;
    });
    contextModules.set(context, pending);
  }
  return pending;
}

function toError(error, fallback) {
  return error instanceof Error ? error : new Error(error?.message || fallback);
}

function readU16(bytes, offset) {
  return (bytes[offset] << 8) | bytes[offset + 1];
}

function readU32(bytes, offset) {
  return ((bytes[offset] * 0x1000000) + (bytes[offset + 1] << 16) + (bytes[offset + 2] << 8) + bytes[offset + 3]) >>> 0;
}

function concatBytes(left, right) {
  if (!left?.byteLength) return right.slice();
  const result = new Uint8Array(left.byteLength + right.byteLength);
  result.set(left, 0);
  result.set(right, left.byteLength);
  return result;
}

function yieldDecodeTask() {
  // A timer can be heavily throttled in background tabs. A posted task gives
  // controls and visibility events a turn without adding that timer delay to
  // every slice of the bounded background refill.
  if (typeof MessageChannel === "function") {
    return new Promise((resolve) => {
      const channel = new MessageChannel();
      channel.port1.onmessage = () => {
        channel.port1.close(); channel.port2.close(); resolve();
      };
      channel.port2.postMessage(null);
    });
  }
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function monotonicNow() { return globalThis.performance?.now?.() ?? Date.now(); }

function packetSource(value, base) {
  try {
    const url = new URL(value, base || globalThis.location?.href);
    if (!/^https?:$/.test(url.protocol) || url.username || url.password || url.hash) return null;
    const match = /^\/api\/v1\/playbacks\/([^/]+)\/audio\/stream$/.exec(url.pathname);
    if (!match) return null;
    const task = decodeURIComponent(match[1]);
    if (!task || /[/?#\s\x00-\x1f]/.test(task)) return null;
    return { url, task };
  } catch { return null; }
}

function packetHandoffPolicy(source, trustedMediaOrigins, streamIdentity) {
  const initial = packetSource(source);
  if (!initial || !streamIdentity) return null;
  const origins = new Set([initial.url.origin]);
  for (const value of trustedMediaOrigins) {
    try {
      const url = new URL(value);
      if (/^https?:$/.test(url.protocol) && !url.username && !url.password && url.pathname === "/" && !url.search && !url.hash) origins.add(url.origin);
    } catch { /* Invalid configuration must not widen the trusted origins. */ }
  }
  return { initial, origins, streamIdentity };
}

function trustedCompletedPacketSource(value, policy) {
  if (!policy) return null;
  const { initial, origins } = policy;
  const candidate = packetSource(value, initial.url);
  if (!candidate || !origins.has(candidate.url.origin) || candidate.url.pathname !== initial.url.pathname) return null;
  // A task ID is not a content hash. Both endpoints must describe the same
  // generation and packet representation; the response's stream ID below
  // additionally proves they reference the exact file already being read.
  const identityQuery = (source) => {
    const params = new URLSearchParams(source.url.search);
    if (params.getAll("v").length > 1 || (params.has("v") && params.get("v") !== source.task)) return null;
    if (params.getAll("rev").length > 1 || (params.has("rev") && !/^[a-f0-9]{32}$/.test(params.get("rev")))) return null;
    params.delete("v"); params.sort();
    return params.toString();
  };
  const initialQuery = identityQuery(initial);
  if (initialQuery === null || identityQuery(candidate) !== initialQuery) return null;
  // Existing identity parameters cannot be removed by a completion response.
  for (const key of ["v", "rev"]) {
    if (initial.url.searchParams.has(key) && candidate.url.searchParams.get(key) !== initial.url.searchParams.get(key)) return null;
  }
  return candidate.url.href;
}

/**
 * Streaming sample-rate converter (Catmull-Rom cubic) for interleaved PCM.
 * Phase and the last input frames carry over between calls. Converting each
 * packet on its own pinned both of its ends to the packet edges, which jumped
 * time at every boundary: a 10-50 Hz flutter on 44.1 kHz -> 48 kHz music and
 * a half-sample jump per packet on 96 kHz -> 192 kHz Hi-Res.
 */
export function createStreamResampler(channels, sourceRate, targetRate) {
  if (!channels || !sourceRate || !targetRate || Math.abs(sourceRate - targetRate) <= 1) {
    return {
      process: (input) => input.slice(0, Math.floor(input.length / (channels || 1)) * (channels || 1)),
      flush: () => new Float32Array(0),
    };
  }
  const step = sourceRate / targetRate;
  let history = null;
  let position = 1;
  // Output frames for source positions [position, end) of `source`, where
  // frame i reads frames i-1..i+2.
  const interpolate = (source, end) => {
    const count = Math.max(0, Math.ceil((end - position) / step));
    const output = new Float32Array(count * channels);
    let time = position;
    for (let index = 0; index < count; index += 1, time += step) {
      const frame = Math.floor(time);
      const fraction = time - frame;
      const base = frame * channels;
      for (let channel = 0; channel < channels; channel += 1) {
        const p0 = source[base - channels + channel];
        const p1 = source[base + channel];
        const p2 = source[base + channels + channel];
        const p3 = source[base + 2 * channels + channel];
        output[index * channels + channel] = p1 + 0.5 * fraction * (p2 - p0 +
          fraction * (2 * p0 - 5 * p1 + 4 * p2 - p3 + fraction * (3 * (p1 - p2) + p3 - p0)));
      }
    }
    position = time;
    return output;
  };
  // A virtual frame continuing the slope of its neighbours, so the stream
  // edges interpolate like the interior instead of flattening.
  const extrapolate = (target, at, from, toward) => {
    for (let channel = 0; channel < channels; channel += 1) {
      target[at * channels + channel] = 2 * target[from * channels + channel] - target[toward * channels + channel];
    }
  };
  return {
    process(input) {
      const inputFrames = Math.floor(input.length / channels);
      if (!inputFrames) return new Float32Array(0);
      const fresh = input.subarray(0, inputFrames * channels);
      let source;
      if (history) {
        source = new Float32Array(history.length + fresh.length);
        source.set(history);
        source.set(fresh, history.length);
      } else {
        // Frame 0 is virtual; interpolation starts exactly on the first sample.
        source = new Float32Array(fresh.length + channels);
        source.set(fresh, channels);
        if (inputFrames > 1) extrapolate(source, 0, 1, 2);
        else source.set(fresh.subarray(0, channels));
        position = 1;
      }
      const frames = source.length / channels;
      if (frames < 4) {
        history = source;
        return new Float32Array(0);
      }
      // Frame i needs i-1..i+2, so positions stay below frames - 2.
      const output = interpolate(source, frames - 2);
      history = source.slice((frames - 3) * channels);
      position -= frames - 3;
      return output;
    },
    /** Emit the frames still held back at the end of the stream. */
    flush() {
      if (!history) return new Float32Array(0);
      const real = history.length / channels;
      const source = new Float32Array((real + 2) * channels);
      source.set(history);
      for (let pad = real; pad < real + 2; pad += 1) {
        if (pad >= 2) extrapolate(source, pad, pad - 1, pad - 2);
        else source.set(source.subarray((pad - 1) * channels, pad * channels), pad * channels);
      }
      // Positions up to one source period past the last real frame.
      const output = interpolate(source, real);
      history = null;
      position = 1;
      return output;
    },
  };
}

class AlacPacketStreamParser {
  constructor() {
    this.buffer = new Uint8Array(0);
    this.offset = 0;
    this.header = null;
  }

  // Parse with a read offset. Re-slicing the remaining buffer after every
  // packet copied each network chunk once per packet it contained. Packets
  // are read-only views; only an incomplete tail is copied into the next push.
  push(chunk) {
    if (chunk?.byteLength) {
      this.buffer = concatBytes(this.buffer.subarray(this.offset), chunk);
      this.offset = 0;
    }
    const packets = [];
    if (!this.header && this.buffer.byteLength - this.offset >= HEADER_BYTES) {
      const start = this.offset;
      const magic = new TextDecoder().decode(this.buffer.subarray(start, start + 8));
      const version = readU32(this.buffer, start + 8);
      const cookieLength = readU16(this.buffer, start + 18);
      if (magic !== STREAM_MAGIC || version !== STREAM_VERSION || cookieLength < 24 || cookieLength > 64) {
        throw new Error("ALAC 流协议版本或配置无效");
      }
      const headerLength = HEADER_BYTES + cookieLength;
      if (this.buffer.byteLength - start < headerLength) return packets;
      this.headerBytes = this.buffer.slice(start, start + headerLength);
      this.header = {
        sampleRate: readU32(this.buffer, start + 12),
        channels: this.buffer[start + 16],
        bits: this.buffer[start + 17],
        maxSamples: readU32(this.buffer, start + 20),
        cookie: this.buffer.slice(start + HEADER_BYTES, start + headerLength),
      };
      this.offset += headerLength;
    }
    if (!this.header) return packets;
    while (this.buffer.byteLength - this.offset >= 8) {
      const start = this.offset;
      const packetLength = readU32(this.buffer, start);
      const duration = readU32(this.buffer, start + 4);
      if (!packetLength || packetLength > 16 * 1024 * 1024) throw new Error("ALAC 流包含非法 packet 长度");
      if (this.buffer.byteLength - start < 8 + packetLength) break;
      packets.push({ duration, bytes: this.buffer.subarray(start + 8, start + 8 + packetLength) });
      this.offset = start + 8 + packetLength;
    }
    return packets;
  }

  finish() {
    if (!this.header || this.buffer.byteLength > this.offset) throw new Error("ALAC 流在完整 packet 边界前结束");
  }
}

async function ensureWasm() {
  if (!wasmReady) wasmReady = initAlacWasm().catch((error) => { wasmReady = null; throw error; });
  await wasmReady;
}

export class AlacWorkletPlayer {
  constructor({ onState, onTime, onEnded, onError, onProgress, onStreamComplete } = {}) {
    Object.assign(this, {
      onState: onState || (() => {}), onTime: onTime || (() => {}),
      onEnded: onEnded || (() => {}), onError: onError || (() => {}),
      onProgress: onProgress || (() => {}), onStreamComplete: onStreamComplete || (() => {}),
      context: null, node: null, gain: null, output: null, decoder: null, track: null,
      reader: null, parser: null, abortController: null, firstPacket: null,
      generation: 0, epoch: 0, playRequest: 0, loaded: false, playing: false, eof: false,
      volume: 1, loudnessGain: 1, fadeGain: 1, outputSampleRate: 0, totalBytes: 0, readBytes: 0,
      packets: [], decodeIndex: 0, totalSourceFrames: 0, skipSourceFrames: 0,
      pendingSeekFrame: null,
      sentFrames: 0, consumedFrames: 0, ended: false, decoding: null,
      completeBlob: null, streamCompleteNotified: false, renderEofSent: false,
      removeAbortListener: null,
      preparedContext: null, preparedSampleRate: 0,
      completionController: null, rangeController: null, completedSource: null,
      handoffPolicy: null,
      rangeAttempted: false, expectedTotalBytes: 0,
      streamIdentity: null, removeRangeAbortListener: null,
      rangeCandidate: null, transportWaiter: null,
      removeVisibilityListener: null,
    });
  }

  get queuedFrames() { return Math.max(0, this.sentFrames - this.consumedFrames); }
  get duration() { return this.track ? this.totalSourceFrames / this.track.sampleRate * 1000 : 0; }
  get canSeek() { return Boolean(this.loaded && this.track && this.node); }
  get seekableDuration() { return this.duration; }
  get seekPending() { return this.pendingSeekFrame !== null; }

  get pcmQueueTargetFrames() {
    const backgroundPlaying = this.playing && typeof document !== "undefined" && document.visibilityState === "hidden";
    const seconds = backgroundPlaying ? BACKGROUND_PCM_QUEUE_SECONDS : PCM_QUEUE_SECONDS;
    if (!this.track || !this.outputSampleRate) return 0;
    const channels = this.track.channels;
    const maxFrames = Math.floor(MAX_PCM_QUEUE_BYTES / (Float32Array.BYTES_PER_ELEMENT * channels));
    // The decoder transfers complete packets. Reserve the final refill packet
    // and the consumed prefix still held by the renderer's first packet, so
    // the PCM allocations (not just unplayed frames) stay within the ceiling.
    const packetFrames = Math.ceil(this.track.maxSamples * this.outputSampleRate / this.track.sampleRate);
    return Math.max(0, Math.min(this.outputSampleRate * seconds, maxFrames - packetFrames * 2));
  }

  watchVisibility(generation) {
    this.removeVisibilityListener?.();
    this.removeVisibilityListener = null;
    if (typeof document === "undefined" || !document.addEventListener) return;
    const owner = document;
    const changed = () => {
      if (generation === this.generation && this.loaded && this.playing) void this.drainPackets(generation);
    };
    owner.addEventListener("visibilitychange", changed);
    this.removeVisibilityListener = () => owner.removeEventListener("visibilitychange", changed);
  }

  static isSupported() { return isAlacWorkletSupported(); }

  static canHandle(track) { return canUseAlacWorklet(track); }

  static canRequest(track) {
    return isAlacWorkletSupported() && track?.platform === "applemusic" && isLosslessTrack(track);
  }

  // Must be called synchronously by a trusted click/change/key handler. WebKit
  // permissions belong to this context: unlocking a throwaway context does
  // not authorize a new one after a cold provider request finishes.
  prepare(sampleRate = 192000) {
    if (!AlacWorkletPlayer.isSupported()) return;
    const active = this.context;
    if (active && active.state !== "closed") void active.resume().catch(() => {});
    const context = prepareOutput(sampleRate);
    this.preparedContext = context;
    this.preparedSampleRate = sampleRate;
    // Download the codec and processor in parallel with the provider task.
    void ensureWasm().catch(() => {});
    void prepareModule(context).catch(() => {});
  }

  async ensureContext(sampleRate, generation) {
    const requested = Number.isFinite(sampleRate) && sampleRate > 0 ? Math.round(sampleRate) : undefined;
    let context = this.preparedContext;
    if (!context || context.state === "closed") {
      context = getOutputContext(requested);
      this.preparedContext = context;
      this.preparedSampleRate = requested;
    }
    this.context = context;
    let output;
    try {
      await prepareModule(context);
      if (generation !== this.generation) throw new DOMException("ALAC 加载已取消", "AbortError");
      output = createAlacOutput(context);
      output.setVolume(this.volume);
      output.setLoudnessGain(this.loudnessGain);
      output.cancelFade(this.fadeGain);
      const node = new AudioWorkletNode(context, "splayer-alac-pcm", { numberOfInputs: 0, numberOfOutputs: 1, outputChannelCount: [2] });
      node.connect(output.input);
      node.port.onmessage = (event) => {
        if (generation === this.generation) this.handleWorkletMessage(event.data || {});
      };
      this.context = context;
      this.output = output;
      this.gain = output.volumeGain;
      this.node = node;
      this.outputSampleRate = context.sampleRate;
    } catch (error) {
      output?.dispose();
      throw error;
    }
  }

  async load(url, { signal, waitForCompletedSource, completed = false, trustedMediaOrigins = [] } = {}) {
    if (!url) throw new Error("ALAC 音频地址为空");
    if (!AlacWorkletPlayer.isSupported()) throw new Error("当前浏览器不支持 AudioWorklet");
    // stop() invalidates and detaches synchronously; an old reader cancellation
    // must never tear down a newer track after its promise eventually resolves.
    void this.stop();
    const generation = ++this.generation;
    const controller = new AbortController();
    this.abortController = controller;
    if (signal) {
      const abort = () => controller.abort();
      if (signal.aborted) controller.abort();
      else signal.addEventListener("abort", abort, { once: true });
      this.removeAbortListener = () => signal.removeEventListener("abort", abort);
    }
    try {
      const response = await fetch(url, { cache: completed ? "default" : "no-store", credentials: "omit", signal: controller.signal });
      if (generation !== this.generation) throw new DOMException("ALAC 加载已取消", "AbortError");
      if (!response.ok || !response.body?.getReader) throw new Error(`ALAC 流下载失败：HTTP ${response.status}`);
      const encoding = response.headers.get("content-encoding")?.trim().toLowerCase();
      const identityEncoding = !encoding || encoding === "identity";
      // Fetch exposes decoded bytes. A compressed HTTP representation cannot
      // share wire offsets (or Content-Length) with our packet byte counter.
      this.totalBytes = identityEncoding ? Number(response.headers.get("content-length")) || 0 : 0;
      this.expectedTotalBytes = this.totalBytes;
      this.streamIdentity = response.headers.get("x-audio-stream-id");
      this.handoffPolicy = packetHandoffPolicy(url, trustedMediaOrigins, this.streamIdentity);
      if (response.url && !trustedCompletedPacketSource(response.url, this.handoffPolicy)) this.handoffPolicy = null;
      this.reader = response.body.getReader();
      this.parser = new AlacPacketStreamParser();
      const firstPacket = {};
      firstPacket.promise = new Promise((resolve, reject) => { firstPacket.resolve = resolve; firstPacket.reject = reject; });
      this.firstPacket = firstPacket;
      if (!completed && this.handoffPolicy && identityEncoding && !this.totalBytes && response.headers.get("accept-ranges") !== "bytes" && waitForCompletedSource) {
        const completionController = new AbortController();
        this.completionController = completionController;
        void Promise.resolve().then(() => waitForCompletedSource({ signal: completionController.signal })).then((completedSource) => {
          const trustedSource = trustedCompletedPacketSource(completedSource, this.handoffPolicy);
          if (generation === this.generation && !this.eof && !completionController.signal.aborted && trustedSource) {
            this.completedSource = trustedSource;
            this.startCompletedStream(generation);
          }
        }).catch(() => {}); // Completion monitoring is optional; retain the original stream on failure.
      }
      void this.consumeStream(generation);
      const metadata = await firstPacket.promise;
      if (generation !== this.generation) throw new DOMException("ALAC 加载已取消", "AbortError");
      this.loaded = true;
      this.watchVisibility(generation);
      return metadata;
    } catch (error) {
      if (generation === this.generation) void this.stop();
      throw toError(error, "ALAC 解码失败");
    }
  }

  resolveStartup() {
    if (!this.firstPacket || !this.track) return;
    if (this.queuedFrames >= this.outputSampleRate * STARTUP_QUEUE_SECONDS || (this.eof && this.decodeIndex === this.packets.length)) {
      if (!this.totalSourceFrames) {
        this.firstPacket.reject(new Error("ALAC 流没有音频 packet"));
      } else {
        this.firstPacket.resolve({ duration: this.eof ? this.duration : 0, sampleRate: this.track.sampleRate, channels: this.track.channels, bits: this.track.bits });
      }
    }
  }

  async consumeStream(generation) {
    try {
      while (generation === this.generation && this.reader) {
        // Compressed transport is independent from the bounded PCM queue.
        // Pausing audio must not prevent EOF, persistence, or cached seeking.
        const { done, value } = await this.readStreamChunk(generation);
        if (generation !== this.generation) return;
        if (done) {
          if (this.expectedTotalBytes && this.readBytes !== this.expectedTotalBytes) throw new Error("ALAC 完整流字节数不符，已停止缓存");
          this.parser.finish();
          if (!this.packets.length) throw new Error("ALAC 流没有音频 packet");
          this.eof = true;
          this.completionController?.abort();
          this.completionController = null;
          this.discardRangeCandidate(this.rangeCandidate);
          this.removeRangeAbortListener?.();
          this.removeRangeAbortListener = null;
          this.rangeController = null;
          const parts = [this.parser.headerBytes];
          let offset = this.parser.headerBytes.byteLength;
          for (const packet of this.packets) {
            const framing = new Uint8Array(8);
            const view = new DataView(framing.buffer);
            view.setUint32(0, packet.bytes.byteLength);
            view.setUint32(4, packet.duration);
            packet.offset = offset + 8;
            packet.length = packet.bytes.byteLength;
            parts.push(framing, packet.bytes);
            offset += 8 + packet.length;
          }
          this.completeBlob = new Blob(parts, { type: "application/x-splayer-alac" });
          // Keep a single immutable compressed Blob plus small packet offsets.
          // PCM is decoded on demand, never retained for the entire song.
          for (const packet of this.packets) packet.bytes = null;
          this.reader = null;
          if (!this.streamCompleteNotified) {
            this.streamCompleteNotified = true;
            this.onStreamComplete({ blob: this.completeBlob, bytes: this.readBytes, duration: this.duration });
          }
          void this.drainPackets(generation);
          this.resolveStartup();
          return;
        }
        if (!value?.byteLength) continue;
        this.readBytes += value.byteLength;
        this.onProgress(this.readBytes, this.totalBytes || this.readBytes);
        const packets = this.parser.push(value);
        if (!this.track && this.parser.header) {
          const header = this.parser.header;
          if (!header.sampleRate || !header.channels || header.channels > 2) throw new Error("当前 AudioWorklet 仅支持单声道或立体声 ALAC");
          // Codec and worklet module are independent; fetch them together.
          await Promise.all([ensureWasm(), this.ensureContext(header.sampleRate, generation)]);
          if (generation !== this.generation) return;
          this.decoder = new AlacDecoder(header.cookie);
          this.track = header;
          this.configureRenderer(0);
        }
        for (const packet of packets) {
          if (!packet.duration || packet.duration > this.track.maxSamples) throw new Error("ALAC packet 时长无效");
          this.packets.push({ ...packet, startFrame: this.totalSourceFrames });
          this.totalSourceFrames += packet.duration;
        }
        void this.drainPackets(generation);
      }
    } catch (error) {
      if (generation === this.generation) this.fail(error);
    }
  }

  async readStreamChunk(generation) {
    const reader = this.reader;
    // There is exactly one outstanding read on the active reader. A candidate
    // can wake this consumer even when the old connection has stopped sending.
    const pending = reader.read().then((result) => ({ result }), (error) => ({ error }));
    while (generation === this.generation) {
      const candidate = this.rangeCandidate;
      if (candidate?.chunk) {
        const { start, value } = candidate.chunk;
        const skip = this.readBytes - start;
        if (skip < 0) this.discardRangeCandidate(candidate);
        else if (skip >= value.byteLength) {
          // The old stream won this race. Ask the candidate to catch up again
          // without buffering a growing duplicate prefix in memory.
          candidate.chunk = null;
          candidate.continue?.();
          candidate.continue = null;
        } else {
          // Only this consumer commits bytes. Any old read already in flight
          // is discarded, so neither parser nor progress sees duplicates.
          candidate.adopted = true;
          clearTimeout(candidate.timer);
          this.rangeCandidate = null;
          this.reader = candidate.reader;
          this.totalBytes = this.expectedTotalBytes = candidate.total;
          this.removeRangeAbortListener = candidate.removeAbortListener;
          candidate.chunk = null;
          candidate.continue?.();
          candidate.continue = null;
          void reader.cancel().catch(() => {});
          return { done: false, value: value.subarray(skip) };
        }
      }
      let wake;
      const changed = new Promise((resolve) => { wake = () => resolve({ changed: true }); });
      this.transportWaiter = wake;
      const next = await Promise.race([pending, changed]);
      if (this.transportWaiter === wake) this.transportWaiter = null;
      if (generation !== this.generation) break;
      if (next.changed) continue;
      if (next.error) throw next.error;
      return next.result;
    }
    return { done: true };
  }

  startCompletedStream(generation) {
    if (this.rangeAttempted || generation !== this.generation || this.eof) return;
    this.rangeAttempted = true;
    const controller = new AbortController();
    const candidate = {
      controller, offset: this.readBytes, cursor: this.readBytes,
      reader: null, chunk: null, continue: null, adopted: false,
      cancelled: false, timer: null, removeAbortListener: null,
    };
    this.rangeCandidate = candidate;
    this.rangeController = controller;
    const lifecycleSignal = this.abortController?.signal;
    const abort = () => candidate.adopted ? controller.abort() : this.discardRangeCandidate(candidate);
    candidate.removeAbortListener = () => lifecycleSignal?.removeEventListener("abort", abort);
    if (lifecycleSignal?.aborted) { abort(); return; }
    lifecycleSignal?.addEventListener("abort", abort, { once: true });
    // This deadline covers headers and catching up. It never suspends the old
    // transport, and is removed once the candidate has actually been adopted.
    candidate.timer = setTimeout(() => this.discardRangeCandidate(candidate), RANGE_HANDOFF_TIMEOUT_MS);
    void this.prepareCompletedStream(candidate, generation);
  }

  discardRangeCandidate(candidate) {
    if (!candidate || candidate.adopted || candidate.cancelled) return;
    candidate.cancelled = true;
    clearTimeout(candidate.timer);
    candidate.removeAbortListener?.();
    candidate.controller.abort();
    candidate.chunk = null;
    candidate.continue?.();
    candidate.continue = null;
    if (candidate.reader) void candidate.reader.cancel().catch(() => {});
    if (this.rangeController === candidate.controller) this.rangeController = null;
    if (this.rangeCandidate === candidate) {
      this.rangeCandidate = null;
      this.transportWaiter?.();
    }
  }

  async prepareCompletedStream(candidate, generation) {
    const { controller, offset } = candidate;
    let response;
    try {
      const source = trustedCompletedPacketSource(this.completedSource, this.handoffPolicy);
      if (!source) return;
      response = await fetch(source, { cache: "default", credentials: "omit", redirect: "error", signal: controller.signal, headers: { Range: `bytes=${offset}-` } });
      if (generation !== this.generation || controller.signal.aborted) return;
      if (response.url && !trustedCompletedPacketSource(response.url, this.handoffPolicy)) return;
      const range = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(response.headers.get("content-range") || "");
      const [start, end, total] = range ? range.slice(1).map(Number) : [];
      const length = response.headers.get("content-length");
      const type = response.headers.get("content-type")?.split(";")[0].trim();
      const encoding = response.headers.get("content-encoding")?.trim().toLowerCase();
      if (response.status !== 206 || !response.body?.getReader || !range ||
          (encoding && encoding !== "identity") ||
          !this.streamIdentity || response.headers.get("x-audio-stream-id") !== this.streamIdentity ||
          ![start, end, total].every(Number.isSafeInteger) || start !== offset || end !== total - 1 || total <= offset ||
          (length != null && Number(length) !== total - offset) || (type && type !== "application/x-splayer-alac")) return;
      candidate.total = total;
      candidate.reader = response.body.getReader();
      while (generation === this.generation && !candidate.cancelled && !candidate.adopted) {
        const { done, value } = await candidate.reader.read();
        if (generation !== this.generation || candidate.cancelled || done) return;
        if (!value?.byteLength) continue;
        const start = candidate.cursor;
        candidate.cursor += value.byteLength;
        if (candidate.cursor > total) return;
        if (candidate.cursor <= this.readBytes) continue;
        // Keep at most one candidate chunk. Duplicate bytes are discarded
        // without touching parser/decoder; the consumer trims again at commit.
        candidate.chunk = { start, value };
        const consumed = new Promise((resolve) => { candidate.continue = resolve; });
        this.transportWaiter?.();
        await consumed;
      }
    } catch {
      // Optional transport improvement: all candidate failures retain the
      // continuously consumed original stream.
    } finally {
      if (!candidate.adopted) {
        this.discardRangeCandidate(candidate);
        if (!candidate.reader && response?.body) void response.body.cancel().catch(() => {});
      }
    }
  }

  fail(error) {
    this.firstPacket?.reject(error);
    void this.stop();
    this.onState("paused");
    this.onError(error);
  }

  async drainPackets(generation) {
    if (generation !== this.generation || !this.decoder || !this.node || !this.track) return;
    if (this.pendingSeekFrame !== null) {
      // Packet framing indexes the compressed prefix as it arrives. A forward
      // seek must not decode the old prefix or synthesize progress while its
      // target is still downloading; retain only the latest selected frame.
      if (!this.eof && this.pendingSeekFrame >= this.totalSourceFrames) return;
      const target = Math.min(this.pendingSeekFrame, this.totalSourceFrames);
      if (target !== this.pendingSeekFrame) {
        this.configureRenderer(Math.round(target / this.track.sampleRate * this.outputSampleRate));
        this.onTime(target / this.track.sampleRate * 1000);
        if (this.playing) this.node.port.postMessage({ type: "play", epoch: this.epoch });
      }
      this.selectPacketAtFrame(target);
      this.pendingSeekFrame = null;
    }
    const epoch = this.epoch;
    if (this.decoding === epoch) return;
    this.decoding = epoch;
    try {
      let sliceStart = monotonicNow();
      let slicePackets = 0;
      while (generation === this.generation && epoch === this.epoch && this.decodeIndex < this.packets.length && this.queuedFrames < this.pcmQueueTargetFrames) {
        const packet = this.packets[this.decodeIndex];
        const bytes = packet.bytes || await this.readCompletedPacket(packet);
        if (generation !== this.generation || epoch !== this.epoch) return;
        // Reused: the resampler below always returns a new transferable array.
        const capacity = Math.max(this.decoder.maxSamplesPerPacket(), 1024);
        if (this.decodeBuffer?.length !== capacity) this.decodeBuffer = new Float32Array(capacity);
        const output = this.decodeBuffer;
        const count = this.decoder.decodePacketF32(bytes, output);
        const decodedFrames = Math.floor(count / this.track.channels);
        if (decodedFrames !== packet.duration) throw new Error("ALAC packet 解码长度与时间索引不一致");
        this.decodeIndex += 1;
        const skip = Math.min(this.skipSourceFrames, decodedFrames);
        this.skipSourceFrames -= skip;
        const decoded = output.subarray(skip * this.track.channels, count);
        // One converter per renderer epoch: a seek or new track starts fresh.
        if (this.resamplerEpoch !== epoch) {
          this.resampler = createStreamResampler(this.track.channels, this.track.sampleRate, this.outputSampleRate);
          this.resamplerEpoch = epoch;
        }
        const pcm = this.resampler.process(decoded);
        const frames = Math.floor(pcm.length / this.track.channels);
        if (frames) {
          this.sentFrames += frames;
          this.node.port.postMessage({ type: "data", epoch, format: "f32", data: pcm.buffer, frames }, [pcm.buffer]);
        }
        this.resolveStartup();
        slicePackets += 1;
        if (slicePackets >= DECODE_SLICE_PACKETS || monotonicNow() - sliceStart >= DECODE_SLICE_MS) {
          await yieldDecodeTask();
          sliceStart = monotonicNow();
          slicePackets = 0;
        }
      }
      if (generation === this.generation && epoch === this.epoch && this.eof && this.decodeIndex === this.packets.length && !this.renderEofSent) {
        const tail = this.resamplerEpoch === epoch ? this.resampler.flush() : null;
        const tailFrames = tail ? Math.floor(tail.length / this.track.channels) : 0;
        if (tailFrames) {
          this.sentFrames += tailFrames;
          this.node.port.postMessage({ type: "data", epoch, format: "f32", data: tail.buffer, frames: tailFrames }, [tail.buffer]);
        }
        this.renderEofSent = true;
        this.node.port.postMessage({ type: "eof", epoch });
        this.resolveStartup();
      }
    } catch (error) {
      if (generation === this.generation && epoch === this.epoch) this.fail(error);
    } finally {
      if (this.decoding === epoch) this.decoding = null;
    }
  }

  async readCompletedPacket(packet) {
    const blob = this.completeBlob;
    const window = this.blobWindow;
    if (window?.blob === blob && packet.offset >= window.start && packet.offset + packet.length <= window.end) {
      return window.bytes.subarray(packet.offset - window.start, packet.offset - window.start + packet.length);
    }
    const start = packet.offset;
    const end = Math.min(blob.size, Math.max(start + packet.length, start + BLOB_READ_WINDOW_BYTES));
    const bytes = new Uint8Array(await blob.slice(start, end).arrayBuffer());
    this.blobWindow = { blob, start, end, bytes };
    return bytes.subarray(0, packet.length);
  }

  handleWorkletMessage(message) {
    if (message.epoch !== this.epoch) return;
    this.consumedFrames = Math.max(this.consumedFrames, Number(message.consumedFrames) || 0);
    if (message.type === "queued" || message.type === "need-data" || message.type === "position") {
      if (message.type === "position" && this.track) this.onTime(Number(message.position) / this.outputSampleRate * 1000);
      void this.drainPackets(this.generation);
    } else if (message.type === "state") {
      if (this.playing) this.onState(message.state);
    } else if (message.type === "ended") {
      this.ended = true;
      this.playing = false;
      this.onTime(this.duration);
      this.onState("paused");
      this.onEnded();
    }
  }

  async play() {
    if (!this.loaded || !this.node || !this.context) throw new Error("ALAC 音频尚未加载");
    const request = ++this.playRequest;
    if (this.ended) await this.seek(0);
    const generation = this.generation;
    const node = this.node;
    let timer;
    try {
      await Promise.race([
        this.context.resume(),
        new Promise((_, reject) => {
          timer = setTimeout(() => reject(new DOMException("音频已就绪，点击播放以允许浏览器播放声音", "NotAllowedError")), RESUME_TIMEOUT_MS);
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
    if (generation !== this.generation || node !== this.node || request !== this.playRequest) throw new DOMException("ALAC 播放已取消", "AbortError");
    this.playing = true;
    node.port.postMessage({ type: "play", epoch: this.epoch });
    this.onState("buffering");
    // A track can be loaded as a paused candidate while the document is
    // already hidden. Only actual playback opts into the larger PCM buffer.
    void this.drainPackets(generation);
    return true;
  }

  pause() {
    this.playRequest += 1;
    if (!this.node) return;
    this.playing = false;
    this.node.port.postMessage({ type: "pause", epoch: this.epoch });
    this.onState("paused");
  }

  configureRenderer(position) {
    this.node.port.postMessage({
      type: "configure", epoch: ++this.epoch, channels: this.track.channels, bits: this.track.bits, position,
      resumeFrames: Math.ceil(this.outputSampleRate * STARTUP_QUEUE_SECONDS),
      rebufferFrames: Math.ceil(this.outputSampleRate * PCM_QUEUE_SECONDS),
      lowWaterFrames: Math.ceil(this.outputSampleRate * 0.5),
      reportFrames: Math.ceil(this.outputSampleRate * 0.1),
    });
  }

  selectPacketAtFrame(requested) {
    let lo = 0;
    let hi = this.packets.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (this.packets[mid].startFrame + this.packets[mid].duration <= requested) lo = mid + 1;
      else hi = mid;
    }
    this.decodeIndex = lo;
    this.skipSourceFrames = requested - (this.packets[lo]?.startFrame ?? requested);
  }

  async seek(positionMs) {
    if (!this.canSeek) throw new Error("ALAC 音频尚未加载");
    const generation = this.generation;
    const position = Number(positionMs);
    const desired = Math.max(0, Math.floor((Number.isFinite(position) ? position : 0) * this.track.sampleRate / 1000));
    const requested = this.eof ? Math.min(this.totalSourceFrames, desired) : desired;
    const wasPlaying = this.playing;
    this.decoder.free?.();
    this.decoder = new AlacDecoder(this.track.cookie);
    this.selectPacketAtFrame(requested);
    this.pendingSeekFrame = !this.eof && requested >= this.totalSourceFrames ? requested : null;
    this.sentFrames = 0;
    this.consumedFrames = 0;
    this.ended = false;
    this.renderEofSent = false;
    this.configureRenderer(Math.round(requested / this.track.sampleRate * this.outputSampleRate));
    const epoch = this.epoch;
    const actualMs = requested / this.track.sampleRate * 1000;
    this.onTime(actualMs);
    if (wasPlaying) this.onState("buffering");
    await this.drainPackets(generation);
    if (generation !== this.generation || epoch !== this.epoch) throw new DOMException("ALAC 跳转已取消", "AbortError");
    // A pause during the asynchronous Blob read wins over the earlier seek.
    if (wasPlaying && this.playing) this.node.port.postMessage({ type: "play", epoch });
    return actualMs;
  }

  setVolume(volume) {
    this.volume = Math.max(0, Math.min(1, Number(volume) || 0));
    this.output?.setVolume(this.volume);
  }

  setLoudnessGain(gain) {
    this.loudnessGain = Number.isFinite(Number(gain)) ? Math.max(0, Number(gain)) : 1;
    this.output?.setLoudnessGain(this.loudnessGain);
  }

  fadeTo(gain, durationSeconds = 0, options) {
    this.fadeGain = Number.isFinite(Number(gain)) ? Math.max(0, Math.min(1, Number(gain))) : 1;
    this.output?.fadeTo(this.fadeGain, durationSeconds, options);
  }

  cancelFade(gain) {
    this.fadeGain = this.output
      ? this.output.cancelFade(gain)
      : gain === undefined ? this.fadeGain : Math.max(0, Math.min(1, Number(gain) || 0));
    return this.fadeGain;
  }

  getFadeLevel(at) {
    return this.output?.getFadeLevel(at) ?? this.fadeGain;
  }

  stop() {
    this.playRequest += 1;
    this.generation += 1;
    const renderEpoch = this.epoch;
    this.epoch += 1;
    this.removeVisibilityListener?.();
    this.removeVisibilityListener = null;
    this.abortController?.abort();
    this.abortController = null;
    this.discardRangeCandidate(this.rangeCandidate);
    this.transportWaiter?.();
    this.transportWaiter = null;
    this.completionController?.abort();
    this.completionController = null;
    this.rangeController?.abort();
    this.rangeController = null;
    this.removeRangeAbortListener?.();
    this.removeRangeAbortListener = null;
    this.completedSource = null;
    this.handoffPolicy = null;
    this.streamIdentity = null;
    this.rangeAttempted = false;
    this.removeAbortListener?.();
    this.removeAbortListener = null;
    const reader = this.reader;
    this.reader = null;
    this.context = null;
    if (this.node) {
      this.node.port.onmessage = null;
      // The reusable AudioContext outlives this node. Retire the processor
      // with its own epoch before closing the port, so its PCM is released
      // and process() stops keeping an unreachable worklet alive.
      this.node.port.postMessage({ type: "dispose", epoch: renderEpoch });
      this.node.disconnect();
      this.node.port.close?.();
    }
    this.node = null;
    // Retiring one deck must never close the context used by its successor.
    // Preserve requested gains across load(); callers set the next track's
    // normalization and initial fade before preparing a muted candidate.
    this.output?.dispose();
    this.output = null;
    this.gain = null;
    this.decoder?.free?.();
    this.decoder = null;
    this.firstPacket?.reject(new DOMException("ALAC 加载已取消", "AbortError"));
    this.firstPacket = null;
    this.track = null;
    this.parser = null;
    this.packets = [];
    this.completeBlob = null;
    this.blobWindow = null;
    this.resampler = null;
    this.resamplerEpoch = null;
    this.loaded = this.playing = this.eof = this.ended = this.streamCompleteNotified = this.renderEofSent = false;
    this.decodeIndex = this.totalSourceFrames = this.skipSourceFrames = this.sentFrames = this.consumedFrames = this.totalBytes = this.readBytes = this.expectedTotalBytes = 0;
    this.pendingSeekFrame = null;
    this.decoding = null;
    return Promise.allSettled([reader?.cancel()]);
  }

  async destroy() {
    await this.stop();
    this.preparedContext = null;
    this.preparedSampleRate = 0;
  }
}

export function createAlacWorkletPlayer(options) { return new AlacWorkletPlayer(options); }
export { canRequestAlacWorklet, canUseAlacWorklet };
