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
async function harness({ enabled = false, cache, cachedSource, alac = true, soundCheck = false, gain = -6, warm } = {}) {
  const syncs = []; let companion = null;
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
  const playback = { setPlaying() {}, setSeeking() {}, setDuration() {}, setCurrentTime: value => value, setOutputDelay() {}, reset() {} };
  globalThis.__companion = { status, queue, media, playback, trackIdentity, trackFromSearch };
  let source = await readFile(new URL("../src/core/player/index.js", import.meta.url), "utf8");
  source = source
    .replace('import { useMediaStore } from "../../stores/media";', 'const useMediaStore = () => globalThis.__companion.media;')
    .replace('import { trackIdentity, useQueueStore } from "../../stores/queue";', 'const { trackIdentity } = globalThis.__companion; const useQueueStore = () => globalThis.__companion.queue;')
    .replace('import { useStatusStore } from "../../stores/status";', 'const useStatusStore = () => globalThis.__companion.status;')
    .replace('import * as playback from "../../services/playback";', 'const playback = globalThis.__companion.playback;')
    .replace('import { trackFromSearch } from "../../utils/track";', 'const { trackFromSearch } = globalThis.__companion;');
  source += '\nexport { prepareTransition as prepareCandidate, checkTransition as checkCandidate, cancelTransition as cancelCandidate };';
  const player = await import(`data:text/javascript;base64,${Buffer.from(source + `\n// ${++instance}`).toString("base64")}`);
  const audio = new Audio(); audio.src = "blob:active"; audio.paused = false;
  const factory = alac ? { canRequest: () => true, canHandle: () => true, create() {
    const worklet = { ...output(), eof: true, completeBlob: new Blob(["packets"]), prepare(rate) { this.rate = rate; }, async load(_source, options = {}) { this.signal = options.signal; queueMicrotask(() => this.onStreamComplete?.({ duration: 12000 })); return { duration: 0 }; }, async play() { this.requestedPlay = true; }, stop() { this.stopped = true; }, pause() { this.paused = true; } };
    worklets.push(worklet); return worklet;
  } } : null;
  player.initPlayer({ audio, alacPlayerFactory: factory,
    mediaCompanionFactory: (callbacks) => { companion = { ...callbacks, syncs }; return { sync: (wanted) => syncs.push(wanted), dispose() { syncs.push("disposed"); } }; }, outputFactory: { prepare() {}, createNative(element) { const graph = output(); graphs.set(element, graph); return graph; } },
    lookupLoudness: () => analysis, warmLoudness: warm || (async () => analysis || { state: "pending" }),
    cacheActiveTrackSource: async track => { events.push(["cache", track.quality]); return cache ? cache.promise : true; },
    resolveCachedTrackSource: async track => { events.push(["read", track.source_id]); return cachedSource ? cachedSource.promise : { source: `blob:${track.source_id}`, track: { ...track, playback_id: `task-${track.source_id}`, format: "alac", byte_size: 100 } }; },
    resolveTrackSource: async track => ({ source: `blob:${track.source_id}`, track }),
  });
  // Use the real load lifecycle so the active output/ALAC callbacks exist.
  await player.load("blob:active", false, tracks[0]);
  status.state = "playing"; status.trackLoading = false; audio.paused = false;
  return { player, status, queue, media, audio, elements, graphs, worklets, events, timers, syncs, companion: () => companion };
}

test("the companion follows play and pause of WASM playback", async () => {
  const h = await harness();
  assert.equal(await h.player.play(), true);
  assert.equal(h.syncs.at(-1), true);
  h.player.pause();
  assert.equal(h.syncs.at(-1), false);
  assert.equal(h.worklets.at(-1).paused, true);
});

test("a natural WASM EOF keeps the companion playing until the next song plays", async () => {
  const h = await harness();
  await h.player.play();
  const worklet = h.worklets.at(-1);
  worklet.onState?.("playing");
  const before = h.syncs.length;
  worklet.ended = true;
  worklet.onState?.("paused");
  worklet.onEnded();
  for (let i = 0; i < 10; i += 1) await tick();
  const during = h.syncs.slice(before);
  assert.ok(during.length > 0, "the hand-off republishes the session");
  assert.ok(!during.includes(false), `no pause between songs: ${JSON.stringify(during)}`);
  assert.equal(h.status.playIndex, 1);
});

test("a browser pause of the companion pauses the music; its resume plays again", async () => {
  const h = await harness();
  await h.player.play();
  h.worklets.at(-1).onState?.("playing");
  h.companion().onExternalPause();
  assert.equal(h.status.state, "paused");
  assert.equal(h.syncs.at(-1), false);
  h.companion().onExternalPlay();
  for (let i = 0; i < 4; i += 1) await tick();
  assert.equal(h.syncs.at(-1), true);
});

test("stop and destroy release the companion", async () => {
  const h = await harness();
  await h.player.play();
  h.player.stop();
  assert.equal(h.syncs.at(-1), false);
  h.player.destroyPlayer();
  assert.equal(h.syncs.at(-1), "disposed");
});
