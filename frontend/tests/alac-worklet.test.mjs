import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

// Exercise the real transport and real render processor. Only the codec is
// stubbed here; the separate ALAC fixture test validates real WASM samples.
let Processor;
class ProcessorBase {
  constructor() { this.port = { postMessage: (message) => this.emit?.(message), close() { this.closed = true; } }; }
}
vm.runInNewContext(await readFile(new URL("../src/core/player/alac-processor.js", import.meta.url), "utf8"), {
  AudioWorkletProcessor: ProcessorBase, ArrayBuffer, Float32Array, Uint8Array,
  registerProcessor(_name, constructor) { Processor = constructor; },
});
class Context {
  constructor({ sampleRate = 48000 } = {}) { this.sampleRate = sampleRate; this.destination = {}; this.currentTime = 0; }
  get audioWorklet() {
    if (this === Context.prototype) throw new TypeError("Illegal invocation");
    return { addModule: async () => {} };
  }
  createGain() {
    return { gain: { value: 1, cancelScheduledValues() {}, setValueAtTime(value) { this.value = value; }, setTargetAtTime(value) { this.value = value; }, linearRampToValueAtTime(value) { this.value = value; } }, connect() {}, disconnect() {} };
  }
  async resume() {}
  async close() { this.closed = true; }
}
class WorkletNode {
  constructor() {
    this.processor = new Processor();
    this.port = { onmessage: null, postMessage: (message) => queueMicrotask(() => this.processor.handleMessage(message)), close() { this.closed = true; } };
    this.processor.emit = (data) => queueMicrotask(() => this.port.onmessage?.({ data }));
  }
  connect(gain) { return gain; }
  disconnect() {}
  render(frames = 128) {
    const output = [new Float32Array(frames), new Float32Array(frames)];
    this.processor.process([], [output]);
    return output;
  }
}
class Decoder {
  constructor() {}
  maxSamplesPerPacket() { return 8192; }
  decodePacketF32(bytes, output) {
    const frames = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(0);
    output.fill(bytes[4] / 100, 0, frames * 2);
    return frames * 2;
  }
  free() {}
}
globalThis.window = { AudioContext: Context };
globalThis.AudioWorkletNode = WorkletNode;
globalThis.__alacTestDecoder = Decoder;
const source = (await readFile(new URL("../src/core/player/alac-worklet.js", import.meta.url), "utf8"))
  .replace('import processorUrl from "./alac-processor.js?url";', 'const processorUrl = "test:processor";')
  .replace('from "./audio-output.js";', `from ${JSON.stringify(new URL("../src/core/player/audio-output.js", import.meta.url).href)};`)
  .replace('from "./alac-support.js";', `from ${JSON.stringify(new URL("../src/core/player/alac-support.js", import.meta.url).href)};`)
  .replace('import initAlacWasm, { AlacDecoder } from "../../wasm/alac-decoder/alac_decoder.js";', 'const initAlacWasm = async () => {}; const AlacDecoder = globalThis.__alacTestDecoder;');
const { AlacWorkletPlayer, createStreamResampler } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
const tick = () => new Promise((resolve) => setImmediate(resolve));
async function until(predicate) {
  for (let i = 0; i < 100; i += 1) { if (predicate()) return; await tick(); }
  assert.ok(predicate(), "condition timed out");
}
function fixture(packetCount = 40, sampleRate = 48000) {
  const result = new Uint8Array(48 + packetCount * 13);
  result.set(new TextEncoder().encode("SPLALAC1"));
  const view = new DataView(result.buffer);
  view.setUint32(8, 2); view.setUint32(12, sampleRate); result[16] = 2; result[17] = 24;
  view.setUint16(18, 24); view.setUint32(20, 4096);
  for (let i = 0; i < packetCount; i += 1) {
    const offset = 48 + i * 13;
    view.setUint32(offset, 5); view.setUint32(offset + 4, 4096);
    view.setUint32(offset + 8, 4096); result[offset + 12] = i + 1;
  }
  return result;
}
function mockFetch(bytes = fixture()) {
  globalThis.fetch = async () => new Response(new ReadableStream({
    start(controller) {
      // Deliberately split the header and packet framing across chunks.
      for (let i = 0; i < bytes.length; i += 17) controller.enqueue(bytes.slice(i, i + 17));
      controller.close();
    },
  }), { headers: { "content-length": String(bytes.length) } });
}
async function ready(options = {}) {
  mockFetch();
  const player = new AlacWorkletPlayer(options);
  await player.load("test:alac");
  await until(() => player.eof);
  await tick();
  return player;
}

function visibilityFixture(t, initial = "visible") {
  const previous = globalThis.document;
  const listeners = new Set();
  const document = {
    visibilityState: initial,
    addEventListener(type, callback) { if (type === "visibilitychange") listeners.add(callback); },
    removeEventListener(type, callback) { if (type === "visibilitychange") listeners.delete(callback); },
  };
  globalThis.document = document;
  t.after(() => { if (previous === undefined) delete globalThis.document; else globalThis.document = previous; });
  return {
    listeners,
    change(value) { document.visibilityState = value; for (const callback of listeners) callback(); },
  };
}

async function longReady(t, sampleRate = 48000, outputRate = sampleRate) {
  mockFetch(fixture(1000, sampleRate));
  const player = new AlacWorkletPlayer();
  player.prepare(outputRate);
  // Normally the app requests at most 192 kHz. Simulate a different device
  // output rate to exercise the defensive PCM byte ceiling as well.
  if (outputRate > 192000) player.preparedContext = new Context({ sampleRate: outputRate });
  t.after(() => player.destroy());
  await player.load("test:long-background-audio");
  await until(() => player.eof && player.decoding === null);
  await tick();
  return player;
}

test("hidden active playback buffers fifteen seconds while prepared and paused decks stay at two", async (t) => {
  const visibility = visibilityFixture(t, "hidden");
  const player = await longReady(t);
  assert.equal(visibility.listeners.size, 1);
  assert.ok(player.queuedFrames >= 48000 * 2);
  assert.ok(player.queuedFrames < 48000 * 2 + 4096);
  const before = player.decodeIndex;
  visibility.change("visible"); visibility.change("hidden"); await tick();
  assert.equal(player.decodeIndex, before, "a muted/prepared successor must not expand its buffer");
  await player.play();
  await until(() => player.queuedFrames >= 48000 * 15 && player.decoding === null);
  assert.ok(player.queuedFrames < 48000 * 15 + 4096);
  assert.ok(player.decodeIndex < player.packets.length, "background playback must never decode the full long song");
  player.pause(); await tick();
  const pausedIndex = player.decodeIndex;
  visibility.change("visible"); visibility.change("hidden"); await tick();
  assert.equal(player.decodeIndex, pausedIndex);
  await player.stop();
  assert.equal(visibility.listeners.size, 0);
});

test("returning foreground keeps existing PCM then resumes its two-second refill policy", async (t) => {
  const visibility = visibilityFixture(t);
  const player = await longReady(t);
  await player.play(); await tick();
  visibility.change("hidden");
  await until(() => player.queuedFrames >= 48000 * 15 && player.decoding === null);
  await tick();
  const buffered = player.queuedFrames;
  const decoded = player.decodeIndex;
  visibility.change("visible"); await tick();
  assert.equal(player.queuedFrames, buffered, "returning to the page must not flush buffered music");
  assert.equal(player.decodeIndex, decoded);
  player.node.render(48000); await tick();
  assert.equal(player.decodeIndex, decoded, "the larger queue is consumed without maintaining its hidden target");
  player.node.render(player.node.processor.pendingFrames - 48000); await tick();
  await until(() => player.queuedFrames >= 48000 * 2 && player.decoding === null);
  assert.ok(player.queuedFrames < 48000 * 2 + 4096);
});

test("Hi-Res background PCM stays below twenty-four MiB including the final whole packet", async (t) => {
  visibilityFixture(t, "hidden");
  const player = await longReady(t, 192000);
  await player.play();
  await until(() => player.queuedFrames >= 192000 * 15 && player.decoding === null);
  assert.ok(player.queuedFrames * 2 * Float32Array.BYTES_PER_ELEMENT <= 24 * 1024 * 1024);
  assert.ok(player.decodeIndex < player.packets.length);
  await player.stop();

  // At an unusually high output rate the memory ceiling wins over fifteen
  // seconds, including packets enlarged by resampling.
  const higherRate = await longReady(t, 192000, 384000);
  await higherRate.play();
  await until(() => higherRate.queuedFrames >= higherRate.pcmQueueTargetFrames && higherRate.decoding === null);
  await tick();
  assert.ok(higherRate.queuedFrames * 2 * Float32Array.BYTES_PER_ELEMENT <= 24 * 1024 * 1024);
  for (let index = 0; index < 4; index += 1) {
    // A partial first packet retains its consumed samples until fully read.
    // Include those real ArrayBuffer allocations when checking the limit.
    higherRate.node.render(4097); await tick();
    await until(() => higherRate.decoding === null);
    const allocated = higherRate.node.processor.queue.reduce((total, item) => total + item.floats.byteLength, 0);
    assert.ok(allocated <= 24 * 1024 * 1024);
  }
  assert.ok(higherRate.queuedFrames < higherRate.outputSampleRate * 15);
  assert.ok(higherRate.decodeIndex < higherRate.packets.length);
});

test("pause interrupts background refill slices and stopped listeners cannot decode a successor", async (t) => {
  const visibility = visibilityFixture(t);
  const player = await longReady(t);
  await player.play(); await tick();
  const decode = player.decoder.decodePacketF32.bind(player.decoder);
  let expandedPackets = 0;
  player.decoder.decodePacketF32 = (...args) => {
    expandedPackets += 1;
    if (expandedPackets === 3) player.pause();
    return decode(...args);
  };
  visibility.change("hidden");
  await until(() => player.decoding === null);
  assert.equal(expandedPackets, 3);
  assert.equal(player.playing, false);
  player.context.state = "interrupted";
  player.handleWorkletMessage({ type: "need-data", epoch: player.epoch, consumedFrames: player.consumedFrames });
  visibility.change("visible"); visibility.change("hidden"); await tick();
  assert.equal(expandedPackets, 3, "an explicit pause still wins if the context is interrupted");
  const staleListener = [...visibility.listeners][0];
  await player.stop();
  assert.equal(visibility.listeners.size, 0);
  mockFetch(); await player.load("test:successor"); await until(() => player.eof && player.decoding === null);
  assert.equal(visibility.listeners.size, 1);
  const successorIndex = player.decodeIndex;
  staleListener(); await tick();
  assert.equal(player.decodeIndex, successorIndex);
});

async function downloading(options = {}) {
  const bytes = fixture(60);
  let transport;
  let sent = 48 + 8 * 13;
  let requests = 0;
  globalThis.fetch = async () => {
    requests += 1;
    return new Response(new ReadableStream({
      start(controller) { transport = controller; controller.enqueue(bytes.slice(0, sent)); },
    }), { headers: { "content-length": String(bytes.length) } });
  };
  const player = new AlacWorkletPlayer(options);
  await player.load("test:downloading");
  await tick();
  return { player, bytes, requests: () => requests,
    advanceTo(packetCount) {
      const end = 48 + packetCount * 13;
      transport.enqueue(bytes.slice(sent, end)); sent = end;
    },
    finish() { transport.enqueue(bytes.slice(sent)); sent = bytes.length; transport.close(); },
  };
}

test("browser support detection does not invoke AudioContext prototype accessors", () => {
  assert.equal(AlacWorkletPlayer.isSupported(), true);
});

test("paused transport reaches EOF without decoding the entire song or refetching", async () => {
  const complete = [];
  const player = await ready({ onStreamComplete: (event) => complete.push(event) });
  assert.equal(player.playing, false);
  assert.equal(player.canSeek, true);
  assert.equal(complete.length, 1);
  assert.equal(complete[0].blob, player.completeBlob);
  assert.deepEqual(new Uint8Array(await player.completeBlob.arrayBuffer()), fixture());
  assert.ok(player.decodeIndex < player.packets.length);
  assert.ok(player.queuedFrames <= player.outputSampleRate * 2 + 4096);
  assert.ok(player.packets.every((packet) => packet.bytes === null));
  assert.equal(player.renderEofSent, false, "render EOF must wait for remaining compressed packets");
  await player.stop();
});

test("seek selects exact frame and stays paused until play", async () => {
  const positions = [];
  const states = [];
  const player = await ready({ onTime: (p) => positions.push(p), onState: (s) => states.push(s) });
  const frame = 8192 + 123;
  const actual = await player.seek(frame / 48);
  assert.ok(Math.abs(actual - frame / 48) < 1 / 48);
  await tick();
  assert.equal(player.node.render()[0][0], 0);
  assert.equal(player.node.processor.position, frame);
  assert.equal(player.playing, false);
  await player.play();
  await tick();
  assert.equal(player.node.render()[0][0], Math.fround(0.03));
  await tick();
  assert.equal(states.at(-1), "playing");
  assert.ok(positions.at(-1) > actual);
  await player.stop();
});

test("downloaded-prefix seek uses packet samples before EOF without another fetch", async () => {
  const h = await downloading();
  const player = h.player;
  assert.equal(player.canSeek, true);
  assert.equal(player.eof, false);
  assert.equal(player.completeBlob, null);
  assert.equal(player.seekableDuration, 8 * 4096 / 48);
  const target = (4096 + 123) / 48;
  assert.ok(Math.abs(await player.seek(target) - target) < 1 / 48);
  assert.equal(player.seekPending, false);
  await player.play(); await tick();
  assert.equal(player.node.render()[0][0], Math.fround(0.02));
  assert.equal(player.node.processor.position, 4096 + 123 + 128);
  assert.equal(h.requests(), 1);
  await player.stop();
});

test("a forward seek waits at the target then renders before EOF and keeps one exact full cache", async () => {
  const positions = [];
  const states = [];
  const completed = [];
  const h = await downloading({ onTime: (time) => positions.push(time), onState: (state) => states.push(state), onStreamComplete: (result) => completed.push(result) });
  const player = h.player;
  await player.play(); await tick(); player.node.render(); await tick();
  const oldEpoch = player.epoch;
  assert.equal(await player.seek(3000), 3000);
  await tick();
  assert.equal(player.seekPending, true);
  assert.equal(player.node.render()[0][0], 0);
  assert.equal(player.node.processor.position, 144000);
  assert.equal(states.at(-1), "buffering");
  player.handleWorkletMessage({ type: "position", epoch: oldEpoch, position: 128, consumedFrames: 128 });
  assert.equal(positions.at(-1), 3000);
  h.advanceTo(20); await until(() => player.packets.length === 20);
  assert.equal(player.node.render()[0][0], 0);
  assert.equal(player.node.processor.position, 144000);
  h.advanceTo(48); await until(() => !player.seekPending); await tick();
  assert.equal(player.eof, false);
  assert.equal(completed.length, 0);
  assert.equal(player.node.render()[0][0], Math.fround(0.36));
  await tick();
  assert.equal(states.at(-1), "playing");
  assert.ok(positions.at(-1) > 3000);
  h.finish(); await until(() => player.eof); await tick();
  assert.equal(completed.length, 1);
  assert.deepEqual(new Uint8Array(await player.completeBlob.arrayBuffer()), h.bytes);
  assert.equal(h.requests(), 1);
  assert.ok(player.queuedFrames <= player.outputSampleRate * 2 + 4096);
  await player.stop();
});

test("the latest seek replaces a pending forward target and later packets cannot resurrect it", async () => {
  const positions = [];
  const h = await downloading({ onTime: (time) => positions.push(time) });
  const player = h.player;
  await player.play(); await tick();
  await player.seek(4000);
  const supersededEpoch = player.epoch;
  const target = 4096 / 48;
  await player.seek(target); await tick();
  assert.equal(player.seekPending, false);
  assert.equal(player.node.render()[0][0], Math.fround(0.02));
  h.advanceTo(55); await until(() => player.packets.length === 55); await tick();
  player.handleWorkletMessage({ type: "position", epoch: supersededEpoch, position: 192000 });
  assert.ok(positions.at(-1) < 1000);
  assert.ok(player.node.processor.position < 48000);
  await player.stop();
});

test("pausing a pending seek lets download finish without silently resuming", async () => {
  const positions = [];
  const h = await downloading({ onTime: (time) => positions.push(time) });
  const player = h.player;
  await player.play(); await tick();
  await player.seek(3000);
  player.pause(); await tick();
  h.advanceTo(48); await until(() => !player.seekPending); await tick();
  assert.equal(player.playing, false);
  assert.equal(player.node.render()[0][0], 0);
  assert.equal(positions.at(-1), 3000);
  h.finish(); await until(() => player.eof); await tick();
  assert.equal(player.node.render()[0][0], 0);
  assert.deepEqual(new Uint8Array(await player.completeBlob.arrayBuffer()), h.bytes);
  await player.play(); await tick();
  assert.equal(player.node.render()[0][0], Math.fround(0.36));
  await player.stop();
});

test("a forward target beyond real EOF clamps to the real end and can restart", async () => {
  const positions = [];
  let ended = 0;
  const h = await downloading({ onTime: (time) => positions.push(time), onEnded: () => { ended += 1; } });
  const player = h.player;
  await player.play(); await tick();
  await player.seek(10000);
  h.finish(); await until(() => player.eof); await tick();
  player.node.render(); await tick();
  assert.equal(player.seekPending, false);
  assert.equal(positions.at(-1), 5120);
  assert.equal(ended, 1);
  await player.play(); await tick();
  assert.equal(player.node.render()[0][0], Math.fround(0.01));
  await player.stop();
});

test("changing tracks clears a pending seek without applying it to the successor", async () => {
  const h = await downloading();
  const player = h.player;
  await player.play(); await tick();
  await player.seek(3000);
  const staleEpoch = player.epoch;
  mockFetch();
  await player.load("test:successor"); await until(() => player.eof); await tick();
  assert.equal(player.seekPending, false);
  player.handleWorkletMessage({ type: "position", epoch: staleEpoch, position: 144000 });
  await player.play(); await tick();
  assert.equal(player.node.render()[0][0], Math.fround(0.01));
  assert.equal(player.node.processor.position, 128);
  await player.stop();
});

test("backward seek resets position reports and stale epoch messages cannot rewind", async () => {
  const positions = [];
  const player = await ready({ onTime: (p) => positions.push(p) });
  await player.seek(2000);
  const oldEpoch = player.epoch;
  await player.seek(0);
  player.handleWorkletMessage({ type: "position", epoch: oldEpoch, position: 96000, consumedFrames: 96000 });
  assert.equal(positions.at(-1), 0);
  await player.play(); await tick(); player.node.render(); await tick();
  assert.ok(positions.at(-1) > 0 && positions.at(-1) < 10);
  await player.stop();
});

test("pause during seek and rapid consecutive seeks cannot resume stale audio", async () => {
  const player = await ready();
  await player.play(); await tick();
  const first = player.seek(500);
  const second = player.seek(1500);
  player.pause();
  const result = await Promise.allSettled([first, second]);
  assert.equal(result[0].status, "rejected");
  assert.equal(result[0].reason.name, "AbortError");
  assert.equal(result[1].status, "fulfilled");
  await tick();
  assert.equal(player.playing, false);
  assert.equal(player.node.render()[0][0], 0);
  assert.equal(player.node.processor.position, 72000);
  await player.stop();
});

test("pause wins over a pending AudioContext resume", async () => {
  const player = await ready();
  let resume;
  player.context.resume = () => new Promise((resolve) => { resume = resolve; });
  const playing = player.play();
  player.pause();
  resume();
  await assert.rejects(playing, { name: "AbortError" });
  await tick();
  assert.equal(player.playing, false);
  assert.equal(player.node.render()[0][0], 0);
  player.context.resume = async () => {};
  await player.stop();
});

test("end fires exactly once after all decoded packets, and play restarts cached audio", async () => {
  let ended = 0;
  const player = await ready({ onEnded: () => ended += 1 });
  await player.seek(player.duration - 12);
  await player.play(); await tick();
  for (let i = 0; i < 12; i += 1) { player.node.render(); await tick(); }
  assert.equal(ended, 1);
  assert.equal(player.ended, true);
  await player.play(); await tick();
  assert.equal(player.node.render()[0][0], Math.fround(0.01));
  await tick();
  assert.equal(ended, 1);
  assert.equal(player.ended, false);
  await player.stop();
});

test("full playback drains compressed backlog without truncating at transport EOF", async () => {
  let ended = 0;
  const player = await ready({ onEnded: () => ended += 1 });
  await player.play(); await tick();
  for (let i = 0; i < 1300 && !ended; i += 1) { player.node.render(); await tick(); }
  assert.equal(ended, 1);
  assert.equal(player.node.processor.position, 40 * 4096);
  assert.equal(player.decodeIndex, 40);
  assert.equal(player.queuedFrames, 0);
  await player.stop();
});

test("stalled rendering freezes position and transitions back to playing only on real PCM", async () => {
  const states = [];
  const processor = new Processor();
  processor.emit = (message) => { if (message.type === "state") states.push(message.state); };
  processor.handleMessage({ type: "configure", epoch: 1, channels: 2 });
  processor.handleMessage({ type: "play", epoch: 1 });
  processor.process([], [[new Float32Array(128), new Float32Array(128)]]);
  assert.equal(processor.position, 0);
  assert.equal(states.at(-1), "buffering");
  processor.handleMessage({ type: "data", epoch: 1, format: "f32", frames: 256, data: new Float32Array(512).fill(0.25).buffer });
  processor.process([], [[new Float32Array(128), new Float32Array(128)]]);
  assert.equal(processor.position, 128);
  assert.equal(states.at(-1), "playing");
});

test("a delayed old fetch cannot tear down the next loaded track", async () => {
  let resolveOld;
  globalThis.fetch = () => new Promise((resolve) => { resolveOld = resolve; });
  const player = new AlacWorkletPlayer();
  const oldLoad = player.load("test:old");
  mockFetch();
  await player.load("test:new"); await until(() => player.eof);
  const currentNode = player.node;
  resolveOld(new Response(fixture()));
  await assert.rejects(oldLoad, { name: "AbortError" });
  assert.equal(player.node, currentNode);
  assert.equal(player.loaded, true);
  assert.equal(player.canSeek, true);
  await player.stop();
});

test("a slow old reader cancellation cannot block or detach a newer track", async () => {
  let finishCancel;
  globalThis.fetch = async () => new Response(new ReadableStream({
    start(controller) { controller.enqueue(fixture().slice(0, 48 + 2 * 13)); },
    cancel() { return new Promise((resolve) => { finishCancel = resolve; }); },
  }));
  const player = new AlacWorkletPlayer();
  const oldResult = player.load("test:slow-reader").then(() => null, (error) => error);
  await until(() => player.track);
  mockFetch();
  await player.load("test:new"); await until(() => player.eof);
  const currentNode = player.node;
  assert.equal((await oldResult).name, "AbortError");
  finishCancel(); await tick();
  assert.equal(player.node, currentNode);
  assert.equal(player.loaded, true);
  await player.stop();
});

test("truncated packet streams never become a complete cache and invalidate the current source", async () => {
  let complete = 0;
  const errors = [];
  mockFetch(fixture().slice(0, -1));
  const player = new AlacWorkletPlayer({ onStreamComplete: () => complete += 1, onError: (e) => errors.push(e) });
  await player.load("test:truncated").catch(() => {});
  await until(() => errors.length > 0);
  assert.equal(complete, 0);
  assert.equal(player.canSeek, false);
  assert.equal(player.playing, false);
  await player.stop();
});

test("an unlocked Hi-Res context survives cold loads and automatic track changes", async () => {
  const player = new AlacWorkletPlayer();
  player.prepare(192000);
  const context = player.preparedContext;
  mockFetch(fixture(100, 96000));
  await player.load("test:first"); await until(() => player.eof);
  assert.equal(player.context, context);
  assert.equal(player.outputSampleRate, 192000);
  assert.equal(player.track.sampleRate, 96000, "original ALAC metadata must remain intact");
  assert.ok(player.queuedFrames >= 96000, "Hi-Res startup uses half a second, not 24k frames");
  assert.ok(player.queuedFrames <= 192000 * 2 + 8192);
  await tick();
  assert.equal(player.node.processor.rebufferFrames, 192000 * 2);
  await player.stop();
  assert.equal(context.closed, undefined, "queue changes retain the unlocked context");
  await player.load("test:second");
  assert.equal(player.context, context);
  await player.destroy();
  assert.equal(context.closed, undefined, "destroying a deck preserves the shared context");
});

test("preparing a different quality retains active output and reuses the matching shared context", async () => {
  const player = await ready();
  const active = player.context;
  player.prepare(192000);
  const pending = player.preparedContext;
  assert.notEqual(active, pending);
  assert.equal(active.closed, undefined);
  player.prepare(48000);
  assert.equal(pending.closed, undefined, "another deck may still use the Hi-Res context");
  assert.equal(active.closed, undefined);
  const next = player.preparedContext;
  assert.equal(next, active, "returning to lossless reuses its authorized context");
  await player.load("test:next");
  assert.equal(active.closed, undefined);
  assert.equal(player.context, next);
  await player.destroy();
});

test("a prepared ALAC successor shares authorization, keeps its independent gains and survives retiring the old deck", async () => {
  mockFetch();
  const first = new AlacWorkletPlayer();
  const second = new AlacWorkletPlayer();
  first.prepare(48000); second.prepare(48000);
  assert.equal(first.preparedContext, second.preparedContext);
  second.setVolume(0.4);
  second.setLoudnessGain(0.6);
  second.cancelFade(0);
  await first.load("blob:first"); await until(() => first.eof);
  await second.load("blob:second"); await until(() => second.eof);
  assert.equal(second.output.volumeGain.gain.value, 0.4);
  assert.equal(second.output.normalizationGain.gain.value, 0.6);
  assert.equal(second.getFadeLevel(), 0);
  assert.equal(first.getFadeLevel(), 1);
  const context = first.context;
  await first.destroy();
  assert.equal(context.closed, undefined);
  await second.play(); await tick();
  assert.equal(second.node.render()[0][0], Math.fround(0.01));
  second.cancelFade(1);
  await second.load("blob:third");
  assert.equal(second.getFadeLevel(), 1);
  assert.equal(second.output.volumeGain.gain.value, 0.4);
  assert.equal(second.output.normalizationGain.gain.value, 0.6);
  await second.destroy();
  assert.equal(context.closed, undefined);
});

test("a blocked resume times out without losing packets and a later user retry plays", async () => {
  const player = await ready();
  const blob = player.completeBlob;
  player.context.resume = () => new Promise(() => {});
  await assert.rejects(player.play(), { name: "NotAllowedError" });
  assert.equal(player.loaded, true);
  assert.equal(player.completeBlob, blob);
  assert.equal(player.playing, false);
  player.context.resume = async () => {};
  await player.play(); await tick();
  assert.equal(player.node.render()[0][0], Math.fround(0.01));
  await player.destroy();
});

test("underrun recovery waits for a cushion but always renders a short EOF tail", () => {
  const processor = new Processor();
  processor.handleMessage({ type: "configure", epoch: 1, channels: 2, resumeFrames: 512, lowWaterFrames: 256 });
  const add = (frames) => processor.handleMessage({ type: "data", epoch: 1, format: "f32", frames, data: new Float32Array(frames * 2).fill(0.25).buffer });
  const render = () => processor.process([], [[new Float32Array(128), new Float32Array(128)]]);
  processor.handleMessage({ type: "play", epoch: 1 });
  add(128); render();
  assert.equal(processor.position, 0);
  add(512);
  for (let i = 0; i < 5; i += 1) render();
  assert.equal(processor.position, 640);
  add(128); render();
  assert.equal(processor.position, 640, "one packet must not repeatedly resume and stutter");
  processor.handleMessage({ type: "eof", epoch: 1 }); render();
  assert.equal(processor.position, 768);
  assert.equal(processor.ended, true);
});

test("queue changes retire worklets and release their PCM while preserving the authorized context", async () => {
  const player = new AlacWorkletPlayer();
  player.prepare(192000);
  const context = player.preparedContext;
  mockFetch(fixture(100, 96000));
  const retired = [];
  for (let index = 0; index < 12; index += 1) {
    await player.load(`test:track-${index}`); await until(() => player.eof); await tick();
    const node = player.node;
    const epoch = player.epoch;
    assert.ok(node.processor.pendingFrames > 0);
    assert.equal(node.processor.epoch, epoch);
    await player.play(); await tick();
    await player.stop(); await tick();
    assert.equal(node.processor.disposed, true, "dispose must use the retired node's epoch");
    assert.equal(node.processor.pendingFrames, 0);
    assert.equal(node.processor.queue.length, 0);
    assert.equal(node.processor.port.closed, true);
    assert.equal(node.port.closed, true);
    assert.equal(node.processor.process([], [[new Float32Array(128)]]), false, "retired processors must relinquish their worklet lifetime");
    node.processor.handleMessage({ type: "configure", epoch: epoch + 1, channels: 2 });
    assert.equal(node.processor.disposed, true, "late messages cannot resurrect a retired processor");
    assert.equal(player.preparedContext, context);
    assert.equal(context.closed, undefined);
    retired.push(node);
  }
  assert.ok(retired.every((node) => node.processor.queue.length === 0 && node.processor.disposed));
  await player.destroy();
  assert.equal(context.closed, undefined);
});

test("sustained half-rate delivery waits for two seconds after each underrun without fake progress", () => {
  const processor = new Processor();
  const sampleRate = 192000;
  processor.handleMessage({ type: "configure", epoch: 1, channels: 2,
    resumeFrames: sampleRate / 2, rebufferFrames: sampleRate * 2, lowWaterFrames: sampleRate / 2 });
  const add = (frames) => processor.handleMessage({ type: "data", epoch: 1, format: "f32", frames, data: new Float32Array(frames * 2).fill(0.25).buffer });
  const render = (frames) => {
    const output = [new Float32Array(frames), new Float32Array(frames)];
    processor.process([], [output]);
    return output;
  };
  processor.handleMessage({ type: "play", epoch: 1 });
  add(sampleRate / 2);
  assert.equal(render(sampleRate / 2)[0][0], 0.25, "first startup still needs only half a second");
  assert.equal(processor.needsBuffer, true);
  assert.equal(processor.bufferTargetFrames, sampleRate * 2);
  let recoveries = 0;
  let frozenTicks = 0;
  // Each 250 ms rendering interval receives only 125 ms of audio. This is
  // sustained insufficient throughput, not just one transient network gap.
  for (let interval = 0; interval < 80; interval += 1) {
    add(sampleRate / 8);
    const position = processor.position;
    const waiting = processor.needsBuffer;
    const pending = processor.pendingFrames;
    const output = render(sampleRate / 4);
    if (waiting && pending < sampleRate * 2) {
      assert.equal(processor.position, position);
      assert.ok(output[0].every((sample) => sample === 0));
      frozenTicks += 1;
    } else if (waiting) {
      assert.ok(pending >= sampleRate * 2);
      assert.ok(processor.position > position);
      recoveries += 1;
    }
  }
  assert.ok(recoveries >= 3, "exercise repeated underrun/recovery cycles");
  assert.ok(frozenTicks >= 30, "all waiting intervals must keep a frozen playback clock");
  render(processor.pendingFrames);
  assert.equal(processor.needsBuffer, true);
  const tailStart = processor.position;
  add(sampleRate / 8);
  processor.handleMessage({ type: "eof", epoch: 1 });
  render(sampleRate / 4);
  assert.equal(processor.position - tailStart, sampleRate / 8);
  assert.equal(processor.ended, true, "a short EOF tail bypasses the rebuffer threshold");
});

const taskId = 'applemusic--12345--hires';
const sourceRevision = 'a'.repeat(32);
const streamingSource = `https://app.example.test/api/v1/playbacks/${taskId}/audio/stream?v=${taskId}&rev=${sourceRevision}`;
const completedSource = streamingSource.replace('app.example.test', 'media.example.test');

async function rangeHarness({ responseKind = 'valid', pendingRange = false, manualRange = false,
  initialUrl = streamingSource, completionUrl = completedSource, trustedMediaOrigins = ['https://media.example.test'],
  responseUrl, initialResponseUrl,
} = {}) {
  const bytes = fixture();
  const prefix = 48 + 6 * 13 + 7;
  let original;
  let notify;
  let finishRange;
  let monitorSignal;
  let cancelled = 0;
  let completed = 0;
  let originalSent = prefix;
  let rangeBody;
  let rangeSent;
  let rangeCancelled = 0;
  const requests = [];
  globalThis.fetch = async (url, options = {}) => {
    requests.push({ url, options });
    if (!options.headers?.Range) {
      const response = new Response(new ReadableStream({
        start(controller) { original = controller; controller.enqueue(bytes.slice(0, prefix)); },
        cancel() { cancelled += 1; },
      }), { headers: { 'x-audio-stream-id': 'fixture-version', 'accept-ranges': 'none' } });
      Object.defineProperty(response, 'url', { value: initialResponseUrl || initialUrl });
      return response;
    }
    const offset = Number(/^bytes=(\d+)-$/.exec(options.headers.Range)[1]);
    if (responseKind === 'redirect') throw new TypeError('Redirect blocked by fetch');
    rangeSent = offset;
    const response = () => {
      const result = new Response(manualRange ? new ReadableStream({ start(controller) { rangeBody = controller; }, cancel() { rangeCancelled += 1; } }) : responseKind === '200' ? bytes : bytes.slice(offset), {
      status: responseKind === '200' ? 200 : 206,
      headers: {
        'content-type': 'application/x-splayer-alac',
        'content-range': `bytes ${offset + (responseKind === 'wrong-offset' ? 1 : 0)}-${bytes.length - 1}/${bytes.length}`,
        'content-length': String(bytes.length - offset + (responseKind === 'wrong-length' ? 1 : 0)),
        'x-audio-stream-id': responseKind === 'wrong-identity' ? 'another-file' : 'fixture-version',
        ...(responseKind === 'compressed' ? { 'content-encoding': 'gzip' } : {}),
      },
      });
      Object.defineProperty(result, 'url', { value: responseUrl || url });
      return result;
    };
    return pendingRange ? new Promise((resolve) => { finishRange = () => resolve(response()); }) : response();
  };
  const player = new AlacWorkletPlayer({ onStreamComplete: () => { completed += 1; } });
  await player.load(initialUrl, { trustedMediaOrigins, waitForCompletedSource: ({ signal }) => {
    monitorSignal = signal;
    return new Promise((resolve) => { notify = () => resolve(completionUrl); });
  } });
  const requestHandoff = async () => {
    notify();
    await until(() => requests.length === 2);
  };
  return { player, bytes, prefix, requests, requestHandoff, finishRange: () => finishRange(),
    advanceOriginal: (count) => { original.enqueue(bytes.slice(originalSent, originalSent + count)); originalSent += count; },
    advanceRange: (count) => { rangeBody.enqueue(bytes.slice(rangeSent, rangeSent + count)); rangeSent += count; },
    finishRangeBody: () => { rangeBody.enqueue(bytes.slice(rangeSent)); rangeBody.close(); },
    failRangeBody: () => rangeBody.error(new Error('candidate disconnected')),
    closeRangeBody: () => rangeBody.close(), rangeCancelled: () => rangeCancelled,
    finishOriginal: () => { original.enqueue(bytes.slice(originalSent)); original.close(); },
    closeOriginal: () => { original.enqueue(bytes.slice(originalSent)); original.close(); },
    notify: () => notify(), cancelled: () => cancelled, completed: () => completed, signal: () => monitorSignal };
}

test('trusted CDN completed Range handoff preserves a partial packet and produces one exact cache Blob', async () => {
  const h = await rangeHarness();
  const node = h.player.node;
  const decoder = h.player.decoder;
  await h.requestHandoff();
  await until(() => h.player.eof);
  assert.equal(h.requests[1].options.headers.Range, `bytes=${h.prefix}-`);
  assert.equal(h.requests[0].url, streamingSource);
  assert.equal(h.requests[0].options.cache, 'no-store');
  assert.equal(h.requests[0].options.credentials, 'omit');
  assert.equal(h.requests[1].url, completedSource);
  assert.equal(h.requests[1].options.cache, 'default');
  assert.equal(h.requests[1].options.credentials, 'omit');
  assert.equal(h.requests[1].options.redirect, 'error');
  assert.deepEqual(new Uint8Array(await h.player.completeBlob.arrayBuffer()), h.bytes);
  assert.equal(h.player.readBytes, h.bytes.length);
  assert.equal(h.player.totalBytes, h.bytes.length);
  assert.equal(h.player.node, node);
  assert.equal(h.player.decoder, decoder);
  assert.equal(h.cancelled(), 1);
  assert.equal(h.completed(), 1);
  assert.equal(h.signal().aborted, true);
  await h.player.destroy();
});

for (const [name, options] of [
  ['production API origin', { completionUrl: streamingSource, trustedMediaOrigins: [] }],
  ['development same origin', {
    initialUrl: streamingSource.replace('https://app.example.test', 'http://localhost:5173'),
    completionUrl: streamingSource.replace('https://app.example.test', 'http://localhost:5173'), trustedMediaOrigins: [],
  }],
  ['reordered query parameters', { completionUrl: completedSource.replace(`v=${taskId}&rev=${sourceRevision}`, `rev=${sourceRevision}&v=${taskId}`) }],
  ['legacy URLs without revision', { initialUrl: streamingSource.replace(`&rev=${sourceRevision}`, ''), completionUrl: completedSource.replace(`&rev=${sourceRevision}`, '') }],
  ['task parameter added on completion', { initialUrl: streamingSource.replace(`v=${taskId}&`, '') }],
]) {
  test(`completed handoff accepts ${name} while preserving exact bytes`, async () => {
    const h = await rangeHarness(options);
    await h.requestHandoff(); await until(() => h.player.eof);
    assert.equal(h.cancelled(), 1);
    assert.equal(h.completed(), 1);
    assert.deepEqual(new Uint8Array(await h.player.completeBlob.arrayBuffer()), h.bytes);
    await h.player.destroy();
  });
}

for (const [name, completionUrl, extra = {}] of [
  ['unknown origin', completedSource.replace('media.example.test', 'untrusted.example.test')],
  ['deceptive hostname', completedSource.replace('media.example.test', 'media.example.test.example.test')],
  ['unconfigured CDN', completedSource, { trustedMediaOrigins: [] }],
  ['credentialed URL', completedSource.replace('https://', 'https://user:password@')],
  ['native representation', completedSource.replace('/audio/stream', '/audio')],
  ['another task', completedSource.replaceAll('12345', '99999')],
  ['another quality', completedSource.replaceAll('--hires', '--lossless')],
  ['another generation', completedSource.replace(sourceRevision, 'b'.repeat(32))],
  ['missing revision', completedSource.replace(`&rev=${sourceRevision}`, '')],
  ['unverifiable new revision', completedSource, { initialUrl: streamingSource.replace(`&rev=${sourceRevision}`, '') }],
  ['duplicate revision', `${completedSource}&rev=${sourceRevision}`],
  ['invalid revision', completedSource.replace(sourceRevision, 'invalid')],
  ['another task query', completedSource.replace(`v=${taskId}`, 'v=other')],
  ['duplicate task query', `${completedSource}&v=${taskId}`],
  ['missing task query', completedSource.replace(`v=${taskId}&`, '')],
  ['changed representation query', `${completedSource}&format=flac`],
  ['fragment', `${completedSource}#other`],
  ['non-HTTP source', 'data:application/x-splayer-alac;base64,AA=='],
]) {
  test(`untrusted completion (${name}) never requests a candidate or interrupts the original`, async () => {
    const h = await rangeHarness({ ...extra, completionUrl });
    h.notify(); await tick(); await tick();
    assert.equal(h.requests.length, 1);
    assert.equal(h.cancelled(), 0);
    h.finishOriginal(); await until(() => h.player.eof);
    assert.equal(h.completed(), 1);
    assert.deepEqual(new Uint8Array(await h.player.completeBlob.arrayBuffer()), h.bytes);
    await h.player.destroy();
  });
}

for (const responseUrl of [
  completedSource.replace('media.example.test', 'unknown.example.test'),
  completedSource.replace('/audio/stream', '/audio'),
  completedSource.replace(sourceRevision, 'c'.repeat(32)),
]) {
  test(`a candidate's changed final URL cannot bypass packet identity: ${new URL(responseUrl).pathname} ${new URL(responseUrl).host}`, async () => {
    const h = await rangeHarness({ responseUrl });
    await h.requestHandoff(); await until(() => h.player.rangeCandidate === null);
    assert.equal(h.cancelled(), 0);
    h.finishOriginal(); await until(() => h.player.eof);
    assert.deepEqual(new Uint8Array(await h.player.completeBlob.arrayBuffer()), h.bytes);
    await h.player.destroy();
  });
}

test('an original response redirected outside the trusted resource disables optional handoff', async () => {
  const h = await rangeHarness({ initialResponseUrl: streamingSource.replace('app.example.test', 'unknown.example.test') });
  assert.equal(h.signal(), undefined);
  h.finishOriginal(); await until(() => h.player.eof);
  assert.equal(h.requests.length, 1);
  assert.deepEqual(new Uint8Array(await h.player.completeBlob.arrayBuffer()), h.bytes);
  await h.player.destroy();
});

test('a Range handshake longer than 2.5 seconds never stops the original consumer', async () => {
  const h = await rangeHarness({ pendingRange: true });
  await h.requestHandoff();
  for (let index = 1; index <= 6; index += 1) {
    await new Promise((resolve) => setTimeout(resolve, 450));
    h.advanceOriginal(13);
    await until(() => h.player.readBytes === h.prefix + index * 13);
    assert.equal(h.cancelled(), 0);
    assert.equal(h.requests[1].options.signal.aborted, false);
  }
  h.finishRange(); await until(() => h.player.eof);
  assert.deepEqual(new Uint8Array(await h.player.completeBlob.arrayBuffer()), h.bytes);
  assert.equal(h.completed(), 1);
  await h.player.destroy();
});

test('a candidate discards repeated prefixes and takes over a stalled old read with one chunk', async () => {
  const h = await rangeHarness({ manualRange: true });
  await h.requestHandoff();
  h.advanceOriginal(100); await until(() => h.player.readBytes === h.prefix + 100);
  h.advanceRange(60); await tick();
  assert.equal(h.player.rangeCandidate.chunk, null, 'duplicate prefix is discarded immediately');
  assert.equal(h.cancelled(), 0);
  h.advanceOriginal(60); await until(() => h.player.readBytes === h.prefix + 160);
  h.advanceRange(103); // Only the final three bytes are new to the parser.
  await until(() => h.cancelled() === 1);
  assert.equal(h.player.readBytes, h.prefix + 163);
  assert.equal(h.player.rangeCandidate, null);
  h.finishRangeBody(); await until(() => h.player.eof);
  assert.deepEqual(new Uint8Array(await h.player.completeBlob.arrayBuffer()), h.bytes);
  assert.equal(h.completed(), 1);
  await h.player.destroy();
});

test('candidate body failure leaves the original reader and its exact cache intact', async () => {
  const h = await rangeHarness({ manualRange: true });
  await h.requestHandoff();
  h.advanceOriginal(100); await until(() => h.player.readBytes === h.prefix + 100);
  h.advanceRange(50); await tick(); h.failRangeBody();
  await until(() => h.player.rangeCandidate === null);
  assert.equal(h.cancelled(), 0);
  h.finishOriginal(); await until(() => h.player.eof);
  assert.deepEqual(new Uint8Array(await h.player.completeBlob.arrayBuffer()), h.bytes);
  assert.equal(h.completed(), 1);
  await h.player.destroy();
});

test('the eight-second candidate deadline includes catching up without interrupting the original', async (t) => {
  const h = await rangeHarness({ manualRange: true });
  t.mock.timers.enable({ apis: ['setTimeout'] });
  try {
    await h.requestHandoff();
    h.advanceOriginal(100); await until(() => h.player.readBytes === h.prefix + 100);
    h.advanceRange(50); await tick();
    t.mock.timers.tick(8000); await tick();
    assert.equal(h.player.rangeCandidate, null);
    assert.equal(h.requests[1].options.signal.aborted, true);
    assert.equal(h.cancelled(), 0);
    h.finishOriginal(); await until(() => h.player.eof);
    assert.deepEqual(new Uint8Array(await h.player.completeBlob.arrayBuffer()), h.bytes);
    assert.equal(h.completed(), 1);
  } finally { t.mock.timers.reset(); await h.player.destroy(); }
});

test('old EOF aborts an in-flight candidate and remains the single completion owner', async () => {
  const h = await rangeHarness({ pendingRange: true });
  await h.requestHandoff();
  h.finishOriginal(); await until(() => h.player.eof);
  assert.equal(h.requests[1].options.signal.aborted, true);
  h.finishRange(); await tick();
  assert.equal(h.completed(), 1);
  assert.deepEqual(new Uint8Array(await h.player.completeBlob.arrayBuffer()), h.bytes);
  await h.player.destroy();
});

test('stopping during candidate catch-up cancels both readers without retaining pending PCM', async () => {
  const h = await rangeHarness({ manualRange: true });
  await h.requestHandoff();
  h.advanceOriginal(100); await until(() => h.player.readBytes === h.prefix + 100);
  h.advanceRange(50); await tick();
  await h.player.stop(); await tick();
  assert.equal(h.cancelled(), 1);
  assert.equal(h.rangeCancelled(), 1);
  assert.equal(h.requests[1].options.signal.aborted, true);
  assert.equal(h.player.rangeCandidate, null);
  assert.equal(h.player.transportWaiter, null);
  assert.equal(h.player.packets.length, 0);
  assert.equal(h.completed(), 0);
  await h.player.destroy();
});

test('a truncated adopted Range never becomes a complete Blob or notifies cache completion', async () => {
  const h = await rangeHarness({ manualRange: true });
  const errors = [];
  h.player.onError = (error) => errors.push(error);
  await h.requestHandoff();
  h.advanceRange(20); await until(() => h.cancelled() === 1);
  h.closeRangeBody(); await until(() => errors.length === 1);
  assert.match(errors[0].message, /字节数不符/);
  assert.equal(h.player.completeBlob, null);
  assert.equal(h.completed(), 0);
  await h.player.destroy();
});

for (const responseKind of ['200', 'wrong-offset', 'wrong-length', 'wrong-identity', 'compressed', 'redirect']) {
  test(`invalid completed Range (${responseKind}) retains the original reader`, async () => {
    const h = await rangeHarness({ responseKind });
    await h.requestHandoff();
    await until(() => h.player.rangeController === null);
    assert.equal(h.cancelled(), 0);
    h.finishOriginal();
    await until(() => h.player.eof);
    assert.deepEqual(new Uint8Array(await h.player.completeBlob.arrayBuffer()), h.bytes);
    assert.equal(h.completed(), 1);
    assert.equal(h.requests.length, 2, 'handoff is attempted only once');
    await h.player.destroy();
  });
}

test('stopping or changing tracks during a pending Range cannot adopt its late response', async () => {
  for (const replace of [false, true]) {
    const h = await rangeHarness({ pendingRange: true });
    await h.requestHandoff();
    const pendingSignal = h.requests[1].options.signal;
    if (replace) { mockFetch(); await h.player.load('test:replacement'); await until(() => h.player.eof); }
    else await h.player.stop();
    const node = h.player.node;
    h.finishRange(); await tick(); await tick();
    assert.equal(pendingSignal.aborted, true);
    assert.equal(h.signal().aborted, true);
    assert.equal(h.player.node, node);
    assert.equal(h.player.loaded, replace);
    assert.equal(h.completed(), replace ? 1 : 0);
    await h.player.destroy();
  }
});

test('original EOF wins over a late completion notification without a second request', async () => {
  const h = await rangeHarness();
  h.closeOriginal(); await until(() => h.player.eof);
  h.notify(); await tick();
  assert.equal(h.requests.length, 1);
  assert.equal(h.completed(), 1);
  assert.equal(h.signal().aborted, true);
  await h.player.destroy();
});

for (const sourceKind of ['completed', 'blob', 'legacy-server', 'compressed']) {
  test(`${sourceKind} audio never starts completion polling`, async () => {
    const bytes = fixture();
    let options;
    globalThis.fetch = async (_url, fetchOptions) => { options = fetchOptions; return new Response(bytes, { headers: {
      ...(sourceKind === 'completed' ? { 'content-length': String(bytes.length), 'accept-ranges': 'bytes' } : {}),
      ...(sourceKind !== 'legacy-server' ? { 'x-audio-stream-id': 'fixture-version' } : {}),
      ...(sourceKind === 'compressed' ? { 'content-encoding': 'gzip', 'content-length': '99' } : {}),
    } }); };
    let monitors = 0;
    const player = new AlacWorkletPlayer();
    await player.load(sourceKind === 'blob' ? 'blob:local' : streamingSource, { completed: sourceKind === 'completed', waitForCompletedSource: () => { monitors += 1; } });
    await until(() => player.eof);
    assert.equal(monitors, 0);
    assert.equal(options.cache, sourceKind === 'completed' ? 'default' : 'no-store');
    assert.equal(options.credentials, 'omit');
    await player.destroy();
  });
}

function resampleInPackets(sourceRate, targetRate, packetFrames, packets, frequency) {
  const resampler = createStreamResampler(2, sourceRate, targetRate);
  const parts = [];
  for (let packet = 0; packet < packets; packet += 1) {
    const input = new Float32Array(packetFrames * 2);
    for (let frame = 0; frame < packetFrames; frame += 1) {
      const value = Math.sin(2 * Math.PI * frequency * (packet * packetFrames + frame) / sourceRate);
      input[frame * 2] = value;
      input[frame * 2 + 1] = -value;
    }
    parts.push(resampler.process(input));
  }
  parts.push(resampler.flush());
  const output = new Float32Array(parts.reduce((sum, part) => sum + part.length, 0));
  let offset = 0;
  for (const part of parts) { output.set(part, offset); offset += part.length; }
  return output;
}

for (const [sourceRate, targetRate] of [[44100, 48000], [96000, 192000], [48000, 192000]]) {
  test(`resampling ${sourceRate} -> ${targetRate} keeps phase continuous across packet boundaries`, () => {
    const packetFrames = 4096;
    const packets = 12;
    const frequency = 1000;
    const output = resampleInPackets(sourceRate, targetRate, packetFrames, packets, frequency);
    const frames = output.length / 2;
    const expectedFrames = packetFrames * packets * targetRate / sourceRate;
    assert.ok(Math.abs(frames - expectedFrames) <= 1, `${frames} frames for ${expectedFrames}`);
    // Every output frame must sit on the ideal waveform at its own time,
    // including the frames on each side of a packet boundary.
    let maxError = 0;
    for (let frame = 0; frame < frames; frame += 1) {
      const ideal = Math.sin(2 * Math.PI * frequency * frame / targetRate);
      maxError = Math.max(maxError, Math.abs(output[frame * 2] - ideal), Math.abs(output[frame * 2 + 1] + ideal));
    }
    assert.ok(maxError < 0.002, `max error ${maxError}`);
  });
}

for (const [sourceRate, targetRate, frequency] of [[44100, 48000, 10000], [44100, 48000, 15000], [44100, 192000, 15000]]) {
  // Cubic interpolation left 10-15 kHz tones only 14-26 dB above their
  // distortion at 44.1 -> 48 kHz: an audible hiss on cymbals and sibilants.
  test(`resampling ${sourceRate} -> ${targetRate} keeps a ${frequency / 1000} kHz tone clean (< -80 dB error)`, () => {
    const output = resampleInPackets(sourceRate, targetRate, 4096, 8, frequency);
    const frames = output.length / 2;
    let error = 0;
    let power = 0;
    // Skip the stream edges, which are extrapolated.
    for (let frame = 200; frame < frames - 200; frame += 1) {
      const ideal = Math.sin(2 * Math.PI * frequency * frame / targetRate);
      error += (output[frame * 2] - ideal) ** 2;
      power += ideal ** 2;
    }
    const db = 10 * Math.log10(error / power);
    assert.ok(db < -80, `error ${db.toFixed(1)} dB`);
  });
}

test("same-rate conversion copies samples unchanged", () => {
  const input = Float32Array.from([0.1, -0.1, 0.2, -0.2, 0.3]);
  const output = createStreamResampler(2, 48000, 48000).process(input);
  assert.deepEqual([...output], [...input.subarray(0, 4)]);
  assert.notEqual(output.buffer, input.buffer, "the result is transferable without detaching the decoder buffer");
});

test("decoding after EOF reads the cached Blob in windows, not once per packet", async () => {
  const player = await ready();
  const slices = [];
  const blob = player.completeBlob;
  const originalSlice = blob.slice.bind(blob);
  blob.slice = (...args) => { slices.push(args); return originalSlice(...args); };
  await player.seek(0);
  await until(() => player.decoding === null && player.queuedFrames > 0);
  const decoded = player.decodeIndex;
  assert.ok(decoded > 4, `decoded ${decoded} packets`);
  assert.ok(slices.length <= Math.ceil(blob.size / (2 * 1024 * 1024)), `${slices.length} Blob reads for ${decoded} packets`);
  await player.stop();
});
