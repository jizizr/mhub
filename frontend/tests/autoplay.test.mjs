import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createPinia, setActivePinia } from "pinia";
import { trackIdentity, useQueueStore } from "../src/stores/queue.js";
import { trackFromSearch } from "../src/utils/track.js";

const tick = () => new Promise((resolve) => setImmediate(resolve));
const raw = (id, platform = "applemusic") => ({ id, platform, song: `Song ${id}`, singer: "Synthetic artist" });
const track = (id, extras = {}) => ({ ...trackFromSearch(raw(id), "hires"), ...extras });
const ids = (items) => items.map((item) => item.source_id);
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
let instance = 0;

async function harness({ tracks = [track("current")], playIndex = 0, state = "playing", saveData = false, prefetch = false, resolver = null, recommend = null } = {}) {
  const saved = new Map();
  globalThis.localStorage = { getItem: (key) => saved.get(key), setItem: (key, value) => saved.set(key, value), removeItem: (key) => saved.delete(key) };
  setActivePinia(createPinia());
  const queue = useQueueStore();
  queue.setQueue(tracks);
  const status = { volume: 0.8, speed: 1, state, playIndex, repeatMode: "list", shuffleMode: "off", quality: "hires", position: 12000, duration: 180000,
    autoplayEnabled: false, autoplayState: "idle", autoplayError: "", currentSource: "blob:active", currentSourceTrackId: tracks[playIndex]?.id || null, trackLoading: false,
    get currentTrack() { return queue.getTrack(this.playIndex); }, get isPlaying() { return this.state === "playing" || this.state === "buffering"; }, persist() {}, setNotice(value) { this.notice = value; } };
  const media = { track: status.currentTrack, setTrack(item) { this.track = item; }, beginLyricLoad() {}, clearLyric() {} };
  const timers = new Map();
  let timerId = 0;
  globalThis.window = { location: { origin: "http://localhost" }, setTimeout(fn, delay) { timers.set(++timerId, { fn, delay }); return timerId; }, clearTimeout(id) { timers.delete(id); } };
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { connection: { saveData } } });
  const audioEvents = new Map(), audioCalls = [], requests = [], resolves = [], cacheEvents = [];
  const audio = { src: "blob:active", currentSrc: "blob:active", currentTime: 12, duration: 180, readyState: 4, paused: state === "paused",
    buffered: { length: 1, start: () => 0, end: () => 180 }, addEventListener: (name, fn) => audioEvents.set(name, fn),
    async play() { this.paused = false; this.currentSrc = this.src; audioCalls.push("play"); }, pause() { this.paused = true; audioCalls.push("pause"); },
    load() { audioCalls.push("load"); }, removeAttribute() { this.src = ""; this.currentSrc = ""; } };
  globalThis.__autoplayTest = { status, media, queue, trackIdentity, trackFromSearch,
    playback: { setPlaying() {}, setSeeking() {}, setDuration() {}, setCurrentTime: (value) => value, reset() {} } };
  let source = await readFile(new URL("../src/core/player/index.js", import.meta.url), "utf8");
  source = source
    .replace('import { useMediaStore } from "../../stores/media";', 'const useMediaStore = () => globalThis.__autoplayTest.media;')
    .replace('import { trackIdentity, useQueueStore } from "../../stores/queue";', 'const { trackIdentity } = globalThis.__autoplayTest; const useQueueStore = () => globalThis.__autoplayTest.queue;')
    .replace('import { useStatusStore } from "../../stores/status";', 'const useStatusStore = () => globalThis.__autoplayTest.status;')
    .replace('import * as playback from "../../services/playback";', 'const playback = globalThis.__autoplayTest.playback;')
    .replace('import { trackFromSearch } from "../../utils/track";', 'const { trackFromSearch } = globalThis.__autoplayTest;');
  const player = await import(`data:text/javascript;base64,${Buffer.from(source + `\n// instance ${++instance}`).toString("base64")}`);
  let cacheReady;
  player.initPlayer({ audio,
    resolveTrackSource: async (item) => { resolves.push(item); return resolver ? resolver(item) : { source: `blob:${item.source_id}`, track: item }; },
    getContinuationRecommendations: async (options) => {
      requests.push(options);
      if (recommend) return recommend(options);
      const pending = deferred(); requests.at(-1).pending = pending; return pending.promise;
    },
    cacheActiveTrackSource: prefetch ? async (item) => { cacheEvents.push(["persist", item.source_id]); return new Promise((resolve) => { cacheReady = resolve; }); } : null,
    prefetchTrackSource: prefetch ? async (item) => { cacheEvents.push(["prefetch", item.source_id]); return true; } : null,
  });
  return { player, queue, status, media, audio, audioCalls, audioEvents, timers, requests, resolves, cacheEvents, persist: () => cacheReady(true) };
}

async function runTimers(h, delay) {
  const pending = [...h.timers.entries()].filter(([, item]) => delay === undefined || item.delay === delay);
  pending.forEach(([id]) => h.timers.delete(id));
  pending.forEach(([, { fn }]) => fn());
  await tick();
}

test("autoplay is opt-in, preserves shuffle, and explicit repeat disables it", async () => {
  const h = await harness({ state: "paused" });
  assert.equal(h.status.autoplayEnabled, false);
  h.status.shuffleMode = "on";
  h.player.toggleAutoplay();
  assert.equal(h.status.autoplayEnabled, true);
  assert.equal(h.status.repeatMode, "off");
  assert.equal(h.status.shuffleMode, "on");
  h.player.setPlaybackMode("shuffle");
  assert.equal(h.status.repeatMode, "off");
  h.player.setPlaybackMode("one");
  assert.equal(h.status.autoplayEnabled, false);
  assert.equal(h.status.repeatMode, "one");
  assert.equal(h.requests.length, 0);
});

test("metadata can prepare early but audio waits for the complete current cache", async () => {
  const h = await harness({ prefetch: true });
  h.player.toggleAutoplay(); await tick();
  assert.deepEqual(h.requests[0].exclude, ["current"]);
  h.requests[0].pending.resolve({ tracks: [raw("current"), raw("next"), raw("next"), raw("later")] }); await tick();
  assert.deepEqual(ids(h.queue.queue), ["current", "next", "later"]);
  assert.equal(h.queue.queue[1].autoplayRecommended, true);
  assert.deepEqual(h.resolves, []);
  await runTimers(h, 2500);
  assert.deepEqual(h.cacheEvents, [["persist", "current"]]);
  h.persist(); await tick(); await runTimers(h, 0);
  assert.deepEqual(h.cacheEvents, [["persist", "current"], ["prefetch", "next"]]);
  assert.equal(h.status.currentTrack.source_id, "current");
});

test("tail next and natural ended share one request and start one new song", async () => {
  const h = await harness({ state: "paused" });
  h.player.toggleAutoplay();
  const one = h.player.nextTrack(true), two = h.player.nextTrack(false);
  await tick(); assert.equal(h.requests.length, 1);
  h.requests[0].pending.resolve({ tracks: [raw("new1"), raw("new2"), raw("new3"), raw("new4")] });
  await Promise.all([one, two]);
  assert.equal(h.status.currentTrack.source_id, "new1");
  assert.deepEqual(ids(h.resolves), ["new1"]);
  assert.equal(h.audioCalls.filter((item) => item === "play").length, 1);
});

for (const operation of ["pause", "disable", "clear", "replace", "change track"]) {
  test(`${operation} during pending tail continuation cannot resurrect playback`, async () => {
    const h = await harness({ tracks: [track("older"), track("current")], playIndex: 1, state: "paused" });
    h.player.toggleAutoplay();
    const advance = h.player.nextTrack(true); await tick();
    const request = h.requests[0];
    if (operation === "pause") h.player.pause();
    if (operation === "disable") h.player.setAutoplayEnabled(false);
    if (operation === "clear") await h.player.clearQueue();
    if (operation === "replace") await h.player.playFrom([track("replacement"), track("b"), track("c"), track("d")]);
    if (operation === "change track") await h.player.playAtIndex(0);
    const callsBeforeResponse = h.resolves.length;
    assert.equal(request.signal.aborted, true);
    request.pending.resolve({ tracks: [raw("stale")] }); await advance; await tick();
    assert.equal(h.queue.queue.some((item) => item.source_id === "stale"), false);
    assert.equal(h.resolves.length, callsBeforeResponse);
    if (operation === "pause") assert.equal(h.status.state, "paused");
    if (operation === "clear") assert.equal(h.status.currentTrack, null);
  });
}

test("failed and empty continuations stop at the tail with explicit retry and no wrap", async () => {
  const h = await harness({ state: "paused" });
  h.player.toggleAutoplay();
  const failed = h.player.nextTrack(true); await tick();
  h.requests[0].pending.reject(new Error("network")); await failed;
  assert.equal(h.status.autoplayState, "error");
  assert.equal(h.status.state, "stopped");
  assert.deepEqual(h.resolves, []);
  await h.player.nextTrack(true);
  assert.equal(h.requests.length, 1, "transport controls do not loop failed requests");
  const retry = h.player.retryAutoplay(); await tick();
  h.requests[1].pending.resolve({ tracks: [] }); await retry;
  assert.equal(h.status.autoplayState, "empty");
  assert.deepEqual(h.resolves, []);
  const retryAgain = h.player.retryAutoplay(); await tick();
  h.requests[2].pending.resolve({ tracks: [raw("recovered"), raw("r2"), raw("r3"), raw("r4")] }); await retryAgain;
  assert.equal(h.status.currentTrack.source_id, "recovered");
});

test("turning autoplay off retains current/history and user-added songs in shuffled queues", async () => {
  const h = await harness({ state: "paused", tracks: [track("past", { autoplayRecommended: true }), track("current", { autoplayRecommended: true }), track("auto", { autoplayRecommended: true }), track("manual")], playIndex: 1 });
  h.queue.originalQueue = [...h.queue.queue]; h.status.shuffleMode = "on";
  h.player.toggleAutoplay();
  h.player.insertManyToQueue([track("user")], h.queue.queueLength);
  assert.deepEqual(ids(h.queue.queue), ["past", "current", "user", "auto", "manual"]);
  h.player.setAutoplayEnabled(false);
  assert.deepEqual(ids(h.queue.queue), ["past", "current", "user", "manual"]);
  assert.equal(h.queue.originalQueue.some((item) => item.source_id === "auto"), false);
  assert.equal(h.status.currentTrack.source_id, "current");
});

test("explicitly queued automatic songs become manual and survive disabling autoplay", async () => {
  const h = await harness({ state: "paused", tracks: [track("current"), track("auto", { autoplayRecommended: true }), track("auto2", { autoplayRecommended: true })] });
  h.player.toggleAutoplay();
  h.player.queueNextMany([track("auto")]);
  h.player.setAutoplayEnabled(false);
  assert.deepEqual(ids(h.queue.queue), ["current", "auto"]);
  assert.equal(h.queue.queue[1].autoplayRecommended, false);
});

test("playback quality changes keep recommendation provenance", async () => {
  const h = await harness({ state: "paused", tracks: [track("current"), track("auto", { autoplayRecommended: true }), track("b"), track("c"), track("d")] });
  h.player.toggleAutoplay(); h.status.quality = "lossless";
  await h.player.playAtIndex(1);
  assert.equal(h.status.currentTrack.quality, "lossless");
  assert.equal(h.status.currentTrack.autoplayRecommended, true);
});

test("Data Saver fetches nothing in advance but allows requested continuation at the end", async () => {
  const h = await harness({ saveData: true, prefetch: true });
  h.player.toggleAutoplay(); await tick();
  assert.equal(h.requests.length, 0);
  const advance = h.player.nextTrack(true); await tick();
  assert.equal(h.requests.length, 1);
  h.requests[0].pending.resolve({ tracks: [raw("next"), raw("b"), raw("c"), raw("d")] }); await advance;
  await runTimers(h);
  assert.deepEqual(h.cacheEvents, []);
  assert.equal(h.status.currentTrack.source_id, "next");
});

test("unsupported platforms do not submit IDs to Apple or loop the old queue", async () => {
  const h = await harness({ state: "paused", tracks: [trackFromSearch(raw("qq-id", "qq"))] });
  h.player.toggleAutoplay(); await h.player.nextTrack(true);
  assert.equal(h.requests.length, 0);
  assert.equal(h.status.autoplayState, "empty");
  assert.match(h.status.autoplayError, /音乐来源/);
  assert.deepEqual(h.resolves, []);
});

test("clearing during an uncached source resolve also prevents stale audio from playing", async () => {
  const pending = deferred();
  const h = await harness({ resolver: () => pending.promise });
  const loading = h.player.playFrom([track("cold")]); await tick();
  await h.player.clearQueue();
  pending.resolve({ source: "blob:cold", track: track("cold") }); await loading;
  assert.equal(h.queue.queueLength, 0);
  assert.equal(h.audioCalls.includes("play"), false);
  assert.equal(h.status.currentSource, null);
});

test("user additions during a pending continuation play before the generated tail", async () => {
  const h = await harness({ state: "paused" });
  h.player.toggleAutoplay();
  const advance = h.player.nextTrack(true); await tick();
  h.player.insertManyToQueue([track("chosen")], h.queue.queueLength);
  h.requests[0].pending.resolve({ tracks: [raw("chosen"), raw("new1"), raw("new2"), raw("new3")] }); await advance;
  assert.deepEqual(ids(h.queue.queue), ["current", "chosen", "new1", "new2", "new3"]);
  assert.equal(h.status.currentTrack.source_id, "chosen");
  assert.equal(h.status.currentTrack.autoplayRecommended, false);
});

test("shuffle with autoplay keeps automatic songs behind the manual queue", async () => {
  const h = await harness({ state: "paused", tracks: [track("current"), track("manual1"), track("auto1", { autoplayRecommended: true }), track("manual2"), track("auto2", { autoplayRecommended: true })] });
  h.player.toggleAutoplay();
  h.player.setPlaybackMode("shuffle");
  assert.equal(h.status.currentTrack.source_id, "current");
  assert.equal(h.queue.queue[1].autoplayRecommended, undefined);
  assert.equal(h.queue.queue[2].autoplayRecommended, undefined);
  assert.equal(h.queue.queue[3].autoplayRecommended, true);
  assert.equal(h.queue.queue[4].autoplayRecommended, true);
  assert.equal(h.status.repeatMode, "off");
});

for (const state of ["playing", "paused", "buffering"]) {
  test(`retrying recommendations while the last song is ${state} preserves its transport`, async () => {
    const h = await harness({ state });
    h.player.toggleAutoplay();
    let initial;
    if (state === "paused") initial = h.player.retryAutoplay();
    await tick();
    h.requests[0].pending.reject(new Error("warm metadata failure"));
    await initial; await tick();
    assert.equal(h.status.autoplayState, "error");
    const initialCalls = [...h.audioCalls];
    const initialSource = h.audio.currentSrc;
    const retry = h.player.retryAutoplay(); await tick();
    assert.equal(h.status.state, state);
    assert.deepEqual(h.audioCalls, initialCalls, "retry must not pause, reload or play the current source");
    h.requests[1].pending.resolve({ tracks: [raw("new1"), raw("new2"), raw("new3")] }); await retry;
    assert.equal(h.status.autoplayState, "ready");
    assert.equal(h.status.currentTrack.source_id, "current");
    assert.equal(h.status.state, state);
    assert.equal(h.status.position, 12000);
    assert.equal(h.audio.currentSrc, initialSource);
    assert.deepEqual(h.audioCalls, initialCalls);
    assert.deepEqual(h.resolves, []);
  });
}

test("retry after the native transport actually ended advances even if its pause event ran first", async () => {
  const h = await harness({ state: "paused" });
  h.player.toggleAutoplay();
  h.audio.ended = true;
  const retry = h.player.retryAutoplay(); await tick();
  h.requests[0].pending.resolve({ tracks: [raw("after-end"), raw("b"), raw("c"), raw("d")] }); await retry;
  assert.equal(h.status.currentTrack.source_id, "after-end");
  assert.deepEqual(ids(h.resolves), ["after-end"]);
});

test("a continuation response cannot silently switch playback providers", async () => {
  const h = await harness({ state: "paused" });
  h.player.toggleAutoplay();
  const advance = h.player.nextTrack(true); await tick();
  h.requests[0].pending.resolve({ tracks: [raw("other-provider", "qq")] }); await advance;
  assert.equal(h.status.autoplayState, "empty");
  assert.deepEqual(ids(h.queue.queue), ["current"]);
  assert.deepEqual(h.resolves, []);
});

for (const exitShuffle of ["toggle", "sequence"]) {
  test(`leaving shuffle via ${exitShuffle} keeps later manual additions ahead of recommendations`, async () => {
    const h = await harness({ state: "paused", tracks: [track("past"), track("current"), track("queued")], playIndex: 1 });
    h.player.toggleAutoplay();
    h.player.setPlaybackMode("shuffle");
    const retry = h.player.retryAutoplay(); await tick();
    h.requests[0].pending.resolve({ tracks: [raw("auto1"), raw("auto2"), raw("auto3")] }); await retry;
    h.player.insertManyToQueue([track("chosen1"), track("chosen2")], h.queue.queueLength);
    const callsBefore = [...h.audioCalls];
    const sourceBefore = h.audio.currentSrc;
    assert.deepEqual(ids(h.queue.originalQueue), ["past", "current", "queued", "auto1", "auto2", "auto3", "chosen1", "chosen2"]);
    if (exitShuffle === "toggle") h.player.toggleShuffleMode();
    else h.player.setPlaybackMode("sequence");
    assert.deepEqual(ids(h.queue.queue), ["past", "current", "queued", "chosen1", "chosen2", "auto1", "auto2", "auto3"]);
    assert.equal(h.status.currentTrack.source_id, "current");
    assert.equal(h.status.playIndex, 1);
    assert.equal(h.status.position, 12000);
    assert.equal(h.status.state, "paused");
    assert.equal(h.audio.currentSrc, sourceBefore);
    assert.deepEqual(h.audioCalls, callsBefore);
    assert.equal(h.queue.originalQueue, null);
  });
}
