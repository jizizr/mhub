import initAlacWasm, { AlacDecoder } from "../../wasm/alac-decoder/alac_decoder.js";
import { acquireOutputContext, createAlacOutput, getOutputContext, prepareOutput, suspendIdleOutputs } from "./audio-output.js";
import { canRequestAlacPlayback, canUseAlacPlayback, isAlacPlaybackSupported, isLosslessTrack } from "./alac-support.js";
import { PcmScheduler } from "./pcm-scheduler.js";
import { Mp4AlacStreamParser, looksLikeMp4 } from "./mp4-alac.js";

const PCM_QUEUE_SECONDS = 2;
const BACKGROUND_PCM_QUEUE_SECONDS = 15;
// Refill once a quarter of the queue has played, so each refill becomes one
// sizeable AudioBuffer instead of a source node per pump tick.
const REFILL_FRACTION = 0.75;
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
// A seek this close ahead of a stored M4A's download point waits for it;
// anything further (or behind it) restarts the download there with Range.
const SEEK_WAIT_BYTES = 2 * 1024 * 1024;
const STREAM_MAGIC = "SPLALAC1";
const STREAM_VERSION = 2;
let wasmReady = null;

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
 * Band-limited streaming sample-rate converter (polyphase windowed sinc).
 *
 * Cubic interpolation was continuous across packets but a poor filter: at
 * 44.1 -> 48 kHz (most Apple lossless tracks on a 48 kHz context) a 10 kHz
 * tone came out with distortion only 26 dB below it, heard as a gritty hiss
 * on cymbals and sibilants. Here every output frame is an exact rational
 * phase (48000/44100 = 160/147) of a Kaiser-windowed sinc low-pass at the
 * lower Nyquist rate, giving distortion below -90 dB across the audio band.
 * State carries across packets, so packet boundaries are seamless.
 */
const RESAMPLER_ZERO_CROSSINGS = 16;
const RESAMPLER_KAISER_BETA = 9;
const RESAMPLER_PASSBAND = 0.945;
const resamplerTables = new Map();

function besselI0(x) {
  let sum = 1;
  let term = 1;
  for (let k = 1; k < 40; k += 1) {
    term *= (x / (2 * k)) ** 2;
    sum += term;
    if (term < sum * 1e-12) break;
  }
  return sum;
}

function gcd(a, b) {
  while (b) [a, b] = [b, a % b];
  return a;
}

// taps[phase * width + j] weights input frame (base - half + 1 + j) for an
// output at base + phase / up. Each phase is normalised to unity DC gain.
function resamplerTable(sourceRate, targetRate) {
  const key = `${sourceRate}:${targetRate}`;
  if (resamplerTables.has(key)) return resamplerTables.get(key);
  const divisor = gcd(sourceRate, targetRate);
  const up = targetRate / divisor;
  const down = sourceRate / divisor;
  // Cut-off in cycles per input sample, just below the lower Nyquist rate.
  const cutoff = 0.5 * Math.min(1, targetRate / sourceRate) * RESAMPLER_PASSBAND;
  const reach = RESAMPLER_ZERO_CROSSINGS / (2 * cutoff);
  const half = Math.ceil(reach);
  const width = half * 2;
  const taps = new Float32Array(up * width);
  const norm = besselI0(RESAMPLER_KAISER_BETA);
  for (let phase = 0; phase < up; phase += 1) {
    const fraction = phase / up;
    let sum = 0;
    for (let j = 0; j < width; j += 1) {
      const distance = fraction - (j - half + 1);
      const ratio = distance / reach;
      if (Math.abs(ratio) >= 1) continue;
      const angle = 2 * Math.PI * cutoff * distance;
      const sinc = distance === 0 ? 2 * cutoff : Math.sin(angle) / (Math.PI * distance);
      const value = sinc * besselI0(RESAMPLER_KAISER_BETA * Math.sqrt(1 - ratio * ratio)) / norm;
      taps[phase * width + j] = value;
      sum += value;
    }
    for (let j = 0; j < width; j += 1) taps[phase * width + j] /= sum;
  }
  const table = { up, down, half, width, taps };
  resamplerTables.set(key, table);
  return table;
}

export function createStreamResampler(channels, sourceRate, targetRate) {
  if (!channels || !sourceRate || !targetRate || Math.abs(sourceRate - targetRate) <= 1) {
    return {
      process: (input) => input.slice(0, Math.floor(input.length / (channels || 1)) * (channels || 1)),
      flush: () => new Float32Array(0),
    };
  }
  const { up, down, half, width, taps } = resamplerTable(Math.round(sourceRate), Math.round(targetRate));
  // Input frames not yet fully consumed; `start` is the absolute index of
  // buffer frame 0. Stream edges are extended by point reflection, which
  // keeps both the level and the slope continuous (no edge click).
  let buffer = null;
  let start = 0;
  let received = 0;
  let produced = 0;
  let finished = false;

  const append = (frames) => {
    if (!buffer) { buffer = frames; return; }
    const joined = new Float32Array(buffer.length + frames.length);
    joined.set(buffer);
    joined.set(frames, buffer.length);
    buffer = joined;
  };
  // `count` virtual frames beyond edge frame `at`, stepping by `step` (-1
  // before the stream, +1 after it): x[at + step*k] = 2*x[at] - x[at - step*k].
  // Returned in time order.
  const extend = (source, at, count, step) => {
    const output = new Float32Array(count * channels);
    const total = source.length / channels;
    for (let k = 1; k <= count; k += 1) {
      const mirror = Math.min(total - 1, Math.max(0, at - step * k));
      const slot = step < 0 ? count - k : k - 1;
      for (let channel = 0; channel < channels; channel += 1) {
        output[slot * channels + channel] = 2 * source[at * channels + channel] - source[mirror * channels + channel];
      }
    }
    return output;
  };
  // Emit outputs whose whole filter window lies inside the buffer.
  const render = (limit) => {
    const frames = buffer ? buffer.length / channels : 0;
    // Upper bound of outputs this call can produce.
    const capacity = Math.max(0, Math.ceil((frames + start + half) * up / down) - produced + 1);
    const outputs = new Float32Array(capacity * channels);
    let count = 0;
    for (;;) {
      if (limit !== undefined && produced >= limit) break;
      const position = produced * down;
      const base = Math.floor(position / up);
      const phase = position - base * up;
      const first = base - half + 1 - start;
      if (first < 0) throw new Error("resampler window underflow");
      if (first + width > frames) break;
      const row = phase * width;
      for (let channel = 0; channel < channels; channel += 1) {
        let sum = 0;
        let index = first * channels + channel;
        for (let j = 0; j < width; j += 1, index += channels) sum += taps[row + j] * buffer[index];
        outputs[count * channels + channel] = sum;
      }
      count += 1;
      produced += 1;
    }
    // Drop frames no future output needs.
    const nextBase = Math.floor(produced * down / up);
    const keepFrom = Math.max(0, nextBase - half + 1 - start);
    if (keepFrom > 0 && buffer) {
      buffer = buffer.slice(keepFrom * channels);
      start += keepFrom;
    }
    return outputs.slice(0, count * channels);
  };

  return {
    process(input) {
      const frames = Math.floor(input.length / channels);
      if (!frames || finished) return new Float32Array(0);
      const fresh = input.slice(0, frames * channels);
      if (!buffer) {
        // Virtual history before the first frame.
        buffer = extend(fresh, 0, half - 1, -1);
        start = -(half - 1);
      }
      append(fresh);
      received += frames;
      return render();
    },
    /** Emit the frames still held back at the end of the stream. */
    flush() {
      if (!buffer || finished) return new Float32Array(0);
      finished = true;
      const frames = buffer.length / channels;
      append(extend(buffer, frames - 1, half, 1));
      const total = Math.ceil(received * up / down);
      return render(total);
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

// Growing songs arrive as the SPLALAC1 packet transport; completed songs are
// the stored ALAC M4A itself. Both yield the same packets and header, so the
// player and its cached Blob do not depend on which one the server sent.
class AlacSourceParser {
  constructor() {
    this.prefix = new Uint8Array(0);
    this.inner = null;
  }

  get header() { return this.inner?.header ?? null; }
  get headerBytes() { return this.inner?.headerBytes ?? null; }

  push(chunk) {
    if (this.inner) return this.inner.push(chunk);
    this.prefix = concatBytes(this.prefix, chunk || new Uint8Array(0));
    if (this.prefix.byteLength < 8) return [];
    this.inner = looksLikeMp4(this.prefix) ? new Mp4AlacStreamParser() : new AlacPacketStreamParser();
    const bytes = this.prefix;
    this.prefix = null;
    return this.inner.push(bytes);
  }

  finish() {
    if (!this.inner) throw new Error("ALAC 流在完整 packet 边界前结束");
    this.inner.finish();
  }
}

async function ensureWasm() {
  if (!wasmReady) wasmReady = initAlacWasm().catch((error) => { wasmReady = null; throw error; });
  await wasmReady;
}

export class AlacPlayer {
  constructor({ onState, onTime, onEnded, onError, onProgress, onStreamComplete } = {}) {
    Object.assign(this, {
      onState: onState || (() => {}), onTime: onTime || (() => {}),
      onEnded: onEnded || (() => {}), onError: onError || (() => {}),
      onProgress: onProgress || (() => {}), onStreamComplete: onStreamComplete || (() => {}),
      context: null, scheduler: null, gain: null, output: null, decoder: null, track: null,
      reader: null, parser: null, abortController: null, firstPacket: null,
      generation: 0, epoch: 0, playRequest: 0, loaded: false, playing: false, eof: false,
      volume: 1, loudnessGain: 1, fadeGain: 1, outputSampleRate: 0, totalBytes: 0, readBytes: 0,
      packets: [], decodeIndex: 0, totalSourceFrames: 0, skipSourceFrames: 0,
      pendingSeekFrame: null,
      ended: false, decoding: null,
      completeBlob: null, streamCompleteNotified: false, renderEofSent: false,
      removeAbortListener: null,
      preparedContext: null, preparedSampleRate: 0,
      completionController: null, rangeController: null, completedSource: null,
      handoffPolicy: null,
      rangeAttempted: false, expectedTotalBytes: 0,
      streamIdentity: null, removeRangeAbortListener: null,
      rangeCandidate: null, transportWaiter: null,
      removeVisibilityListener: null,
      // Stored M4A source: the full sample table is known up front, so any
      // packet can be fetched with Range (`{ url, rangeable, missing }`).
      mp4: null, seekWaitBytes: SEEK_WAIT_BYTES,
    });
  }

  get queuedFrames() { return this.scheduler?.queuedFrames ?? 0; }
  get duration() { return this.track ? this.totalSourceFrames / this.track.sampleRate * 1000 : 0; }
  get canSeek() { return Boolean(this.loaded && this.track && this.scheduler); }
  get seekableDuration() { return this.duration; }
  get seekPending() { return this.pendingSeekFrame !== null; }

  get pcmQueueTargetFrames() {
    const backgroundPlaying = this.playing && typeof document !== "undefined" && document.visibilityState === "hidden";
    const seconds = backgroundPlaying ? BACKGROUND_PCM_QUEUE_SECONDS : PCM_QUEUE_SECONDS;
    if (!this.track || !this.outputSampleRate) return 0;
    const channels = this.track.channels;
    const maxFrames = Math.floor(MAX_PCM_QUEUE_BYTES / (Float32Array.BYTES_PER_ELEMENT * channels));
    // The decoder emits complete packets. Reserve the final refill packet and
    // the played prefix still held by the first scheduled AudioBuffer, so the
    // PCM allocations (not just unplayed frames) stay within the ceiling.
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

  static isSupported() { return isAlacPlaybackSupported(); }

  static canHandle(track) { return canUseAlacPlayback(track); }

  static canRequest(track) {
    return isAlacPlaybackSupported() && track?.platform === "applemusic" && isLosslessTrack(track);
  }

  // Must be called synchronously by a trusted click/change/key handler. WebKit
  // permissions belong to this context: unlocking a throwaway context does
  // not authorize a new one after a cold provider request finishes.
  prepare(sampleRate = 192000) {
    if (!AlacPlayer.isSupported()) return;
    const active = this.context;
    if (active && active.state !== "closed") void active.resume().catch(() => {});
    const context = prepareOutput(sampleRate);
    this.preparedContext = context;
    this.preparedSampleRate = sampleRate;
    // Download the codec in parallel with the provider task.
    void ensureWasm().catch(() => {});
  }

  async ensureContext(sampleRate, channels, generation) {
    const requested = Number.isFinite(sampleRate) && sampleRate > 0 ? Math.round(sampleRate) : undefined;
    let prepared = this.preparedContext;
    if (!prepared || prepared.state === "closed") {
      prepared = getOutputContext(requested);
      this.preparedContext = prepared;
      this.preparedSampleRate = requested;
    }
    // Render at the source's own class of rate (48 kHz for 44.1/48 kHz songs
    // even in Hi-Res mode), falling back to a context that is running.
    const context = await acquireOutputContext(requested, [this.context, prepared]);
    if (generation !== this.generation) throw new DOMException("ALAC 加载已取消", "AbortError");
    this.context = context;
    // Never load an AudioWorklet module into this context: Chromium then
    // renders it through the two-thread path this player exists to avoid.
    const output = createAlacOutput(context);
    output.setVolume(this.volume);
    output.setLoudnessGain(this.loudnessGain);
    output.cancelFade(this.fadeGain);
    const current = () => generation === this.generation;
    this.scheduler = new PcmScheduler(context, output.input, {
      channels,
      onState: (state) => { if (current() && this.playing) this.onState(state); },
      onTime: (frame) => { if (current()) this.onTime(frame / this.outputSampleRate * 1000); },
      onEnded: () => { if (current()) this.handleEnded(); },
      onNeedData: () => { if (current()) this.refill(generation); },
    });
    suspendIdleOutputs(context);
    this.context = context;
    this.output = output;
    this.gain = output.volumeGain;
    this.outputSampleRate = context.sampleRate;
  }

  async load(url, { signal, waitForCompletedSource, completed = false, trustedMediaOrigins = [] } = {}) {
    if (!url) throw new Error("ALAC 音频地址为空");
    if (!AlacPlayer.isSupported()) throw new Error("当前浏览器不支持 Web Audio");
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
      this.parser = new AlacSourceParser();
      // Only an identity response with byte ranges can be read out of order.
      const rangeable = identityEncoding && response.headers.get("accept-ranges")?.trim().toLowerCase() === "bytes" &&
        /^https?:/.test(new URL(url, globalThis.location?.href || "http://local.invalid").protocol);
      this.mp4Candidate = { url: response.url || url, rangeable, missing: 0 };
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
        this.firstPacket.resolve({ duration: this.eof || this.mp4 ? this.duration : 0, sampleRate: this.track.sampleRate, channels: this.track.channels, bits: this.track.bits });
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
          // A stored M4A may have been read in pieces after seeks; fetch
          // whatever is still missing before it counts as complete.
          if (this.mp4 && await this.nextMp4Segment(generation)) continue;
          if (generation !== this.generation) return;
          if (!this.mp4 && this.expectedTotalBytes && this.readBytes !== this.expectedTotalBytes) throw new Error("ALAC 完整流字节数不符，已停止缓存");
          if (!this.mp4) this.parser.finish();
          if (!this.packets.length) throw new Error("ALAC 流没有音频 packet");
          this.completeStream(generation);
          return;
        }
        if (!value?.byteLength) continue;
        this.readBytes += value.byteLength;
        this.onProgress(this.readBytes, this.totalBytes || this.readBytes);
        const packets = this.parser.push(value);
        if (!this.track && this.parser.header) {
          const header = this.parser.header;
          if (!header.sampleRate || !header.channels || header.channels > 2) throw new Error("当前播放器仅支持单声道或立体声 ALAC");
          await Promise.all([ensureWasm(), this.ensureContext(header.sampleRate, header.channels, generation)]);
          if (generation !== this.generation) return;
          this.decoder = new AlacDecoder(header.cookie);
          this.track = header;
          this.configureRenderer(0);
          this.prepareMp4Source();
        }
        for (const packet of packets) {
          if (!packet.duration || packet.duration > this.track.maxSamples) throw new Error("ALAC packet 时长无效");
          if (this.mp4) {
            const slot = this.packets[packet.index];
            if (!slot.bytes) this.mp4.missing -= 1;
            slot.bytes = packet.bytes;
            continue;
          }
          this.packets.push({ ...packet, startFrame: this.totalSourceFrames });
          this.totalSourceFrames += packet.duration;
        }
        void this.drainPackets(generation);
      }
    } catch (error) {
      if (generation === this.generation) this.fail(error);
    }
  }

  completeStream(generation) {
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
  }

  // A stored M4A carries its whole sample table, so every packet slot (and
  // the duration) is known before any audio arrives.
  prepareMp4Source() {
    if (!(this.parser.inner instanceof Mp4AlacStreamParser)) return;
    const table = this.parser.inner.table;
    this.mp4 = this.mp4Candidate;
    this.packets = [];
    let startFrame = 0;
    for (let index = 0; index < table.durations.length; index += 1) {
      this.packets.push({ duration: table.durations[index], startFrame, bytes: null });
      startFrame += table.durations[index];
    }
    this.totalSourceFrames = startFrame;
    this.mp4.missing = this.packets.length;
  }

  /**
   * Start fetching the next missing run of packets, preferring the playback
   * position. Returns false when every packet is present.
   */
  async nextMp4Segment(generation) {
    const mp4 = this.mp4;
    if (!mp4.missing || !this.track) return false;
    const count = this.packets.length;
    const from = Math.min(this.decodeIndex, count - 1);
    let first = -1;
    for (let index = from; index < count && first < 0; index += 1) if (!this.packets[index].bytes) first = index;
    for (let index = 0; index < from && first < 0; index += 1) if (!this.packets[index].bytes) first = index;
    if (first < 0) return false;
    if (!mp4.rangeable) throw new Error("音频源不支持分段读取，无法补齐缺失部分");
    let last = first + 1;
    while (last < count && !this.packets[last].bytes) last += 1;
    const { start, end } = this.parser.inner.byteRange(first, last);
    const response = await fetch(mp4.url, {
      cache: "default", credentials: "omit", redirect: "error",
      signal: this.abortController?.signal, headers: { Range: `bytes=${start}-${end - 1}` },
    });
    if (generation !== this.generation) return false;
    const range = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(response.headers.get("content-range") || "");
    const encoding = response.headers.get("content-encoding")?.trim().toLowerCase();
    if (response.status !== 206 || !range || Number(range[1]) !== start || !response.body?.getReader ||
        (encoding && encoding !== "identity")) {
      void response.body?.cancel().catch(() => {});
      throw new Error(`音频分段读取失败：HTTP ${response.status}`);
    }
    this.parser.inner.seekTo(first);
    this.reader = response.body.getReader();
    mp4.segmentFirst = first;
    return true;
  }

  /** After a seek into a missing part, move the download there at once. */
  followMp4Seek() {
    const mp4 = this.mp4;
    const packet = this.packets[this.decodeIndex];
    if (!mp4?.rangeable || this.eof || !packet || packet.bytes || !this.reader) return;
    const next = this.parser.inner.next;
    const { offsets } = this.parser.inner.table;
    // Already on its way: the current download reaches it shortly.
    if (next <= this.decodeIndex && offsets[this.decodeIndex] - offsets[next] <= this.seekWaitBytes) return;
    // Ending this read makes the transport pick the run at the playhead.
    void this.reader.cancel().catch(() => {});
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
    if (generation !== this.generation || !this.decoder || !this.scheduler || !this.track) return;
    if (this.pendingSeekFrame !== null) {
      // Packet framing indexes the compressed prefix as it arrives. A forward
      // seek must not decode the old prefix or synthesize progress while its
      // target is still downloading; retain only the latest selected frame.
      if (!this.eof && this.pendingSeekFrame >= this.totalSourceFrames) return;
      const target = Math.min(this.pendingSeekFrame, this.totalSourceFrames);
      if (target !== this.pendingSeekFrame) {
        this.configureRenderer(Math.round(target / this.track.sampleRate * this.outputSampleRate));
        this.onTime(target / this.track.sampleRate * 1000);
        if (this.playing) this.scheduler.play();
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
        // A stored M4A's packet not downloaded yet: its bytes wake us.
        if (!packet.bytes && !this.completeBlob) break;
        const bytes = packet.bytes || await this.readCompletedPacket(packet);
        if (generation !== this.generation || epoch !== this.epoch) return;
        // Reused: the resampler below always returns a new array.
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
        this.scheduler.push(this.resampler.process(decoded));
        this.resolveStartup();
        slicePackets += 1;
        if (slicePackets >= DECODE_SLICE_PACKETS || monotonicNow() - sliceStart >= DECODE_SLICE_MS) {
          // One AudioBuffer per decode slice keeps source nodes few and large.
          this.scheduler.commit();
          await yieldDecodeTask();
          sliceStart = monotonicNow();
          slicePackets = 0;
        }
      }
      if (generation !== this.generation || epoch !== this.epoch) return;
      if (this.eof && this.decodeIndex === this.packets.length && !this.renderEofSent) {
        if (this.resamplerEpoch === epoch) this.scheduler.push(this.resampler.flush());
        this.renderEofSent = true;
        this.scheduler.finish();
        this.resolveStartup();
      } else this.scheduler.commit();
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

  refill(generation) {
    if (this.queuedFrames <= this.pcmQueueTargetFrames * REFILL_FRACTION) void this.drainPackets(generation);
  }

  handleEnded() {
    this.ended = true;
    this.playing = false;
    this.onTime(this.duration);
    this.onState("paused");
    this.onEnded();
  }

  async play() {
    if (!this.loaded || !this.scheduler || !this.context) throw new Error("ALAC 音频尚未加载");
    const request = ++this.playRequest;
    if (this.ended) await this.seek(0);
    const generation = this.generation;
    const scheduler = this.scheduler;
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
    if (generation !== this.generation || scheduler !== this.scheduler || request !== this.playRequest) throw new DOMException("ALAC 播放已取消", "AbortError");
    this.playing = true;
    this.onState("buffering");
    scheduler.play();
    // A track can be loaded as a paused candidate while the document is
    // already hidden. Only actual playback opts into the larger PCM buffer.
    void this.drainPackets(generation);
    return true;
  }

  pause() {
    this.playRequest += 1;
    if (!this.scheduler) return;
    this.playing = false;
    this.scheduler.pause();
    this.onState("paused");
  }

  configureRenderer(position) {
    this.epoch += 1;
    const scheduler = this.scheduler;
    scheduler.startupFrames = Math.ceil(this.outputSampleRate * STARTUP_QUEUE_SECONDS);
    // After an underrun, wait for a real cushion rather than alternating
    // short bursts and silence; never more than the queue can hold.
    const foreground = Math.min(this.outputSampleRate * PCM_QUEUE_SECONDS, this.pcmQueueTargetFrames || Infinity);
    scheduler.rebufferFrames = Math.max(scheduler.startupFrames, Math.floor(foreground));
    scheduler.reset(position);
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
    const requested = this.eof || this.mp4 ? Math.min(this.totalSourceFrames, desired) : desired;
    const wasPlaying = this.playing;
    this.decoder.free?.();
    this.decoder = new AlacDecoder(this.track.cookie);
    this.selectPacketAtFrame(requested);
    this.pendingSeekFrame = !this.eof && requested >= this.totalSourceFrames ? requested : null;
    this.followMp4Seek();
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
    if (wasPlaying && this.playing) this.scheduler.play();
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
    // Stops every scheduled source and releases its PCM. The shared
    // AudioContext outlives this deck.
    this.scheduler?.dispose();
    this.scheduler = null;
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
    this.mp4 = null;
    this.mp4Candidate = null;
    this.completeBlob = null;
    this.blobWindow = null;
    this.resampler = null;
    this.resamplerEpoch = null;
    this.loaded = this.playing = this.eof = this.ended = this.streamCompleteNotified = this.renderEofSent = false;
    this.decodeIndex = this.totalSourceFrames = this.skipSourceFrames = this.totalBytes = this.readBytes = this.expectedTotalBytes = 0;
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

export function createAlacPlayer(options) { return new AlacPlayer(options); }
export { canRequestAlacPlayback, canUseAlacPlayback };
