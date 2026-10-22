import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createPinia, setActivePinia } from "pinia";
import { trackIdentity, useQueueStore } from "../src/stores/queue.js";
import { trackFromSearch } from "../src/utils/track.js";

const tick = () => new Promise(resolve => setImmediate(resolve));
let instance = 0;
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
class Audio extends EventTarget {
  src = ""; currentTime = 0; duration = 12; readyState = 4; paused = true; volume = 0.8;
  buffered = { length: 1, start: () => 0, end: () => 12 };
  get currentSrc() { return this.src; }
  async play() { if (this.rejectPlay) throw new Error("Playback blocked"); this.paused = false; this.emit("play"); this.emit("playing"); }
  pause() { this.paused = true; this.emit("pause"); }
  load() {}
  removeAttribute() { this.src = ""; }
  emit(name) { this.dispatchEvent(new Event(name)); }
}
async function harness({ enabled = true, cache, cachedSource, alac = false, soundCheck = false, gain = -6, warm } = {}) {
  const saved = new Map();
  globalThis.localStorage = { getItem: key => saved.get(key), setItem: (key, value) => saved.set(key, value), removeItem: key => saved.delete(key) };
  setActivePinia(createPinia());
  const queue = useQueueStore();
  const tracks = ["a", "b", "c"].map(id => ({ ...trackFromSearch({ platform: "applemusic", id, song: id, singer: "Synthetic" }, alac ? "hires" : "high"), duration: 12000, ...(alac ? { streaming_mode: "wasm_alac" } : {}) }));
  queue.setQueue(tracks);
  const status = { volume: 0.8, speed: 1, state: "playing", playIndex: 0, repeatMode: "off", shuffleMode: "off", quality: alac ? "hires" : "high", position: 0, duration: 12000,
    crossfadeEnabled: enabled, crossfadeSeconds: 4, soundCheckEnabled: soundCheck, currentSource: "blob:active", currentSourceTrackId: tracks[0].id, trackLoading: false,
    get currentTrack() { return queue.getTrack(this.playIndex); }, get isPlaying() { return ["playing", "buffering"].includes(this.state); }, persist() {}, setNotice() {} };
  const media = { track: tracks[0], setTrack(value) { this.track = value; }, beginLyricLoad() {}, clearLyric() {} };
  const timers = new Map(); let timerId = 0;
  globalThis.window = { location: { origin: "http://localhost" }, setTimeout(fn, delay) { timers.set(++timerId, { fn, delay }); return timerId; }, clearTimeout(id) { timers.delete(id); } };
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: {} });
  const elements = [];
  globalThis.document = { createElement() { const element = new Audio(); elements.push(element); return element; }, body: { appendChild() {} } };
  const output = () => ({ fade: 1, volume: 0.8, loudness: 1, ramps: [], context: { state: "running", currentTime: 0, async resume() {} }, setVolume(value) { this.volume = value; }, setLoudnessGain(value) { this.loudness = value; }, cancelFade(value = this.fade) { this.fade = value; }, fadeTo(target, seconds) { this.ramps.push({ target, seconds }); this.fade = target; }, getFadeLevel() { return this.fade; } });
  const graphs = new Map(); const worklets = []; const events = [];
  let analysis = soundCheck ? { state: "ready", gain_db: gain } : null;
  const playback = { setPlaying() {}, setSeeking() {}, setDuration() {}, setCurrentTime: value => value, reset() {} };
  globalThis.__transitions = { status, queue, media, playback, trackIdentity, trackFromSearch };
  let source = await readFile(new URL("../src/core/player/index.js", import.meta.url), "utf8");
  source = source
    .replace('import { useMediaStore } from "../../stores/media";', 'const useMediaStore = () => globalThis.__transitions.media;')
    .replace('import { trackIdentity, useQueueStore } from "../../stores/queue";', 'const { trackIdentity } = globalThis.__transitions; const useQueueStore = () => globalThis.__transitions.queue;')
    .replace('import { useStatusStore } from "../../stores/status";', 'const useStatusStore = () => globalThis.__transitions.status;')
    .replace('import * as playback from "../../services/playback";', 'const playback = globalThis.__transitions.playback;')
    .replace('import { trackFromSearch } from "../../utils/track";', 'const { trackFromSearch } = globalThis.__transitions;');
  source += '\nexport { prepareTransition as prepareCandidate, checkTransition as checkCandidate, cancelTransition as cancelCandidate };';
  const player = await import(`data:text/javascript;base64,${Buffer.from(source + `\n// ${++instance}`).toString("base64")}`);
  const audio = new Audio(); audio.src = "blob:active"; audio.paused = false;
  const factory = alac ? { canRequest: () => true, canHandle: () => true, create() {
    const worklet = { ...output(), eof: true, completeBlob: new Blob(["packets"]), prepare(rate) { this.rate = rate; }, async load(_source, options = {}) { this.signal = options.signal; queueMicrotask(() => this.onStreamComplete?.({ duration: 12000 })); return { duration: 0 }; }, async play() { this.requestedPlay = true; }, stop() { this.stopped = true; }, pause() { this.paused = true; } };
    worklets.push(worklet); return worklet;
  } } : null;
  player.initPlayer({ audio, alacPlayerFactory: factory, outputFactory: { prepare() {}, createNative(element) { const graph = output(); graphs.set(element, graph); return graph; } },
    lookupLoudness: () => analysis, warmLoudness: warm || (async () => analysis || { state: "pending" }),
    cacheActiveTrackSource: async track => { events.push(["cache", track.quality]); return cache ? cache.promise : true; },
    resolveCachedTrackSource: async track => { events.push(["read", track.source_id]); return cachedSource ? cachedSource.promise : { source: `blob:${track.source_id}`, track: { ...track, playback_id: `task-${track.source_id}`, format: "alac", byte_size: 100 } }; },
    resolveTrackSource: async track => ({ source: `blob:${track.source_id}`, track }),
  });
  // Use the real load lifecycle so the active output/ALAC callbacks exist.
  await player.load("blob:active", false, tracks[0]);
  status.state = "playing"; status.trackLoading = false; audio.paused = false;
  return { player, status, queue, media, audio, elements, graphs, worklets, events, timers, setAnalysis: value => { analysis = value; } };
}
const nearEnd = async h => { h.status.position = 9000; h.audio.currentTime = 9; h.player.checkCandidate(); await tick(); };

test("candidate decoding waits for successful current-file cache persistence", async () => {
  const cache = deferred(); const h = await harness({ cache });
  const pending = h.player.prepareCandidate(); await tick();
  assert.deepEqual(h.events, [["cache", "high"]]); assert.equal(h.elements.length, 0);
  cache.resolve(true); await pending;
  assert.deepEqual(h.events, [["cache", "high"], ["read", "b"]]);
  assert.equal(h.elements[0].paused, true); assert.equal(h.graphs.get(h.elements[0]).fade, 0);
  assert.equal(h.status.playIndex, 0);
});

test("missing complete next file leaves the current song untouched", async () => {
  const cachedSource = deferred(); const h = await harness({ cachedSource });
  const pending = h.player.prepareCandidate(); await tick(); cachedSource.resolve(null); await pending;
  await nearEnd(h);
  assert.equal(h.status.playIndex, 0); assert.equal(h.graphs.get(h.audio).fade, 1); assert.equal(h.audio.paused, false);
});

test("confirmed native overlap advances once and ignores old ended/time events", async () => {
  const h = await harness(); await h.player.prepareCandidate(); await nearEnd(h);
  const next = h.elements[0];
  assert.equal(h.status.playIndex, 1); assert.equal(h.audio.paused, false); assert.equal(next.paused, false);
  assert.deepEqual(h.graphs.get(h.audio).ramps, [{ target: 0, seconds: 3 }]);
  assert.deepEqual(h.graphs.get(next).ramps, [{ target: 1, seconds: 3 }]);
  assert.equal(h.media.track.playback_id, "task-b", "resolved metadata must survive promotion");
  h.audio.currentTime = 12; h.audio.emit("timeupdate"); h.audio.emit("ended"); await tick();
  assert.equal(h.status.playIndex, 1); assert.equal(h.status.position, 0); assert.equal(next.paused, false);
});

test("blocked candidate play never fades or replaces the current song", async () => {
  const h = await harness(); await h.player.prepareCandidate(); h.elements[0].rejectPlay = true;
  await nearEnd(h);
  assert.equal(h.status.playIndex, 0); assert.deepEqual(h.graphs.get(h.audio).ramps, []);
  assert.equal(h.audio.paused, false); assert.equal(h.elements[0].src, "");
});

test("pause during overlap stops both transports and restores the active fade", async () => {
  const h = await harness(); await h.player.prepareCandidate(); await nearEnd(h); h.player.pause();
  assert.equal(h.audio.paused, true); assert.equal(h.elements[0].paused, true);
  assert.equal(h.graphs.get(h.elements[0]).fade, 1); assert.equal(h.status.state, "paused");
});

test("late cache completion after cancellation cannot resurrect a candidate", async () => {
  const cachedSource = deferred(); const h = await harness({ cachedSource });
  const pending = h.player.prepareCandidate(); await tick(); h.player.pause();
  cachedSource.resolve({ source: "blob:late", track: h.queue.getTrack(1) }); await pending;
  assert.equal(h.elements.length, 0); assert.equal(h.status.playIndex, 0); assert.equal(h.status.state, "paused");
});

test("repeat-one, buffering and sequential albums do not fade", async () => {
  for (const mode of ["one", "buffering", "album"]) {
    const h = await harness();
    if (mode === "one") h.status.repeatMode = "one";
    if (mode === "buffering") h.status.state = "buffering";
    if (mode === "album") h.queue.setQueue(h.queue.queue, { id: "synthetic-album", resource_type: "albums" });
    await h.player.prepareCandidate(); await nearEnd(h);
    assert.equal(h.status.playIndex, 0, mode);
    assert.deepEqual(h.graphs.get(h.audio).ramps, [], mode);
  }
});

test("quality changes cancel prepared bytes and retain the active song quality", async () => {
  const h = await harness(); await h.player.prepareCandidate();
  h.status.quality = "hires"; h.player.onQualityChanged();
  assert.equal(h.elements[0].src, ""); assert.equal(h.media.track.quality, "high");
  assert.equal(h.audio.paused, false); assert.equal(h.status.playIndex, 0);
});

test("ALAC promotion requires actual rendering rather than resolved play command", async () => {
  const h = await harness({ alac: true }); await h.player.prepareCandidate(); await nearEnd(h);
  const old = h.worklets[0], next = h.worklets[1];
  assert.equal(next.requestedPlay, true); assert.equal(h.status.playIndex, 0); assert.deepEqual(old.ramps, []);
  next.onState("playing"); await tick();
  assert.equal(h.status.playIndex, 1); assert.equal(next.rate, 192000);
  assert.equal(h.media.track.streaming_mode, "wasm_alac");
  assert.equal(old.ramps.length, 1); assert.equal(next.ramps.length, 1);
});

test("ALAC fade duration shrinks to the outgoing remainder while waiting for rendered PCM", async () => {
  const h = await harness({ alac: true }); await h.player.prepareCandidate(); await nearEnd(h);
  const old = h.worklets[0], next = h.worklets[1];
  assert.equal(h.status.playIndex, 0);
  // Decoder startup took two seconds; fading for the original three seconds
  // would leave a volume dip after the outgoing song had already ended.
  h.status.position = 11000;
  next.onState("playing"); await tick();
  assert.equal(h.status.playIndex, 1);
  assert.deepEqual(old.ramps, [{ target: 0, seconds: 1 }]);
  assert.deepEqual(next.ramps, [{ target: 1, seconds: 1 }]);
});

test("pause during a candidate context resume cannot start muted audio after cancellation", async () => {
  const h = await harness(); await h.player.prepareCandidate();
  const next = h.elements[0], resumed = deferred();
  let playCalls = 0;
  const originalPlay = next.play.bind(next);
  next.play = () => { playCalls += 1; return originalPlay(); };
  h.graphs.get(next).context.resume = () => resumed.promise;
  await nearEnd(h);
  assert.equal(playCalls, 0, "the candidate must not consume its opening before context permission");
  h.player.pause(); resumed.resolve(); await tick(); await tick();
  assert.equal(playCalls, 0);
  assert.equal(next.paused, true);
  assert.equal(next.src, "");
  assert.equal(h.status.state, "paused");
  assert.equal(h.status.playIndex, 0);
  assert.deepEqual(h.graphs.get(h.audio).ramps, []);
});

test("a queued pause from an earlier native load cannot pause the promoted playing track", async () => {
  const h = await harness(); await h.player.prepareCandidate(); await nearEnd(h);
  const next = h.elements[0];
  assert.equal(h.status.playIndex, 1);
  assert.equal(next.paused, false);
  next.emit("pause");
  assert.equal(h.status.state, "playing");
  assert.equal(h.status.playIndex, 1);
  h.player.pause();
  assert.equal(h.status.state, "paused", "a real pause still wins");
});

test("promoting ALAC cancels candidate loudness polling without aborting its playing packet transport", async () => {
  const polling = [];
  const h = await harness({ alac: true, soundCheck: true, warm: async (track, { signal }) => {
    polling.push({ track, signal }); return new Promise(() => {});
  } });
  await h.player.prepareCandidate(); await nearEnd(h);
  const next = h.worklets[1];
  const candidatePoll = polling.find(item => item.track.source_id === "b");
  assert.ok(candidatePoll);
  assert.equal(candidatePoll.signal.aborted, false);
  assert.notEqual(candidatePoll.signal, next.signal);
  next.onState("playing"); await tick();
  assert.equal(h.status.playIndex, 1);
  assert.equal(candidatePoll.signal.aborted, true);
  assert.equal(next.signal.aborted, false, "the promoted ALAC load retains its independent lifetime");
});

test("disabled effects create no extra decoder or native output", async () => {
  const h = await harness({ enabled: false }); await h.player.prepareCandidate(); await nearEnd(h);
  assert.equal(h.graphs.size, 0); assert.equal(h.elements.length, 0); assert.deepEqual(h.events, []);
});

test("a finished overlap releases its deck for the following transition", async () => {
  const h = await harness(); await h.player.prepareCandidate(); await nearEnd(h);
  assert.equal(h.status.playIndex, 1);
  const retire = [...h.timers.values()].find(timer => timer.delay === 3050);
  assert.ok(retire); retire.fn(); await tick();
  assert.equal(h.audio.src, "blob:c", "third song prepares after the old tail frees the deck");
  h.status.position = 9000; h.elements[0].currentTime = 9;
  h.player.checkCandidate(); await tick();
  assert.equal(h.status.playIndex, 2);
  assert.equal(h.elements.length, 1, "two native elements are reused across the queue");
});
