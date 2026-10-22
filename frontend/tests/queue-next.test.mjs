import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createPinia, setActivePinia } from "pinia";
import { trackIdentity, useQueueStore } from "../src/stores/queue.js";
import { trackFromSearch } from "../src/utils/track.js";

const tick = () => new Promise((resolve) => setImmediate(resolve));
const track = (id, quality = "hires") => trackFromSearch({ platform: "applemusic", id, song: id, singer: "Test artist" }, quality);
const ids = (items) => items.map((item) => item.source_id);
let moduleId = 0;

async function harness({ tracks = [], playIndex = -1, originalQueue = null, shuffleMode = "off", repeatMode = "list", state = "playing", prefetch = false } = {}) {
  const saved = new Map();
  globalThis.localStorage = { getItem: (key) => saved.get(key), setItem: (key, value) => saved.set(key, value), removeItem: (key) => saved.delete(key) };
  setActivePinia(createPinia());
  const queue = useQueueStore();
  queue.setQueue(tracks);
  queue.originalQueue = originalQueue;
  const status = { volume: 0.8, speed: 1, state, playIndex, repeatMode, shuffleMode, quality: "hires", position: 12000, duration: 180000,
    currentSource: playIndex >= 0 ? "blob:active" : null, currentSourceTrackId: tracks[playIndex]?.id || null, trackLoading: false,
    get currentTrack() { return queue.getTrack(this.playIndex); }, get isPlaying() { return this.state === "playing"; }, persist() {}, setNotice() {} };
  const media = { track: status.currentTrack, setTrack(item) { this.track = item; } };
  const timers = new Map();
  let timerId = 0;
  globalThis.window = { location: { origin: "http://localhost" }, setTimeout(fn, delay) { timers.set(++timerId, { fn, delay }); return timerId; }, clearTimeout(id) { timers.delete(id); } };
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: {} });
  const audioEvents = new Map();
  const audioCalls = [];
  const audio = { src: status.currentSource || "", currentSrc: status.currentSource || "", currentTime: 12, duration: 180, readyState: 4, paused: state === "paused",
    buffered: { length: 1, start: () => 0, end: () => 180 }, addEventListener: (event, callback) => audioEvents.set(event, callback),
    play() { audioCalls.push("play"); }, pause() { audioCalls.push("pause"); }, load() { audioCalls.push("load"); }, removeAttribute() { audioCalls.push("remove source"); } };
  globalThis.__queueNextTest = { status, media, queue, trackIdentity, trackFromSearch,
    playback: { setPlaying() {}, setSeeking() {}, setDuration() {}, setCurrentTime: (value) => value } };
  let source = await readFile(new URL("../src/core/player/index.js", import.meta.url), "utf8");
  source = source
    .replace('import { useMediaStore } from "../../stores/media";', 'const useMediaStore = () => globalThis.__queueNextTest.media;')
    .replace('import { trackIdentity, useQueueStore } from "../../stores/queue";', 'const { trackIdentity } = globalThis.__queueNextTest; const useQueueStore = () => globalThis.__queueNextTest.queue;')
    .replace('import { useStatusStore } from "../../stores/status";', 'const useStatusStore = () => globalThis.__queueNextTest.status;')
    .replace('import * as playback from "../../services/playback";', 'const playback = globalThis.__queueNextTest.playback;')
    .replace('import { trackFromSearch } from "../../utils/track";', 'const { trackFromSearch } = globalThis.__queueNextTest;');
  const player = await import(`data:text/javascript;base64,${Buffer.from(source + `\n// instance ${++moduleId}`).toString("base64")}`);
  const events = [];
  let persist;
  player.initPlayer({ audio,
    cacheActiveTrackSource: prefetch ? async (item) => { events.push(["persist", item.source_id]); return new Promise((resolve) => { persist = resolve; }); } : null,
    prefetchTrackSource: prefetch ? async (item) => { events.push(["prefetch", item.source_id]); return true; } : null,
  });
  return { player, queue, status, media, audio, audioCalls, audioEvents, timers, events, persist: () => persist(true) };
}

async function runTimers(h, delay) {
  const pending = [...h.timers.entries()].filter(([, value]) => delay === undefined || value.delay === delay);
  pending.forEach(([id]) => h.timers.delete(id));
  pending.forEach(([, { fn }]) => fn());
  await tick();
}

test("play next moves overlaps from both sides of current into playlist order, without restarting current", async () => {
  const h = await harness({ tracks: [track("a"), track("current", "lossless"), track("b"), track("c")], playIndex: 1 });
  const current = h.status.currentTrack;
  assert.equal(h.player.queueNextMany([track("c", "high"), track("a"), track("current"), track("new"), track("c")]), 3);
  assert.deepEqual(ids(h.queue.queue), ["current", "c", "a", "new", "b"]);
  assert.equal(h.status.playIndex, 0);
  assert.equal(h.status.currentTrack, current);
  assert.equal(h.status.currentTrack.quality, "lossless", "the playing source quality is not rewritten");
  assert.equal(h.queue.queue[1].quality, "hires", "newly scheduled tracks use the current quality preference");
  assert.equal(h.status.position, 12000);
  assert.equal(h.status.state, "playing");
  assert.equal(h.status.currentSource, "blob:active");
  assert.deepEqual(h.audioCalls, []);
});

test("explicit next-up segment is ordered in shuffle and survives returning to original order", async () => {
  const h = await harness({ tracks: [track("c"), track("a"), track("b"), track("d")], playIndex: 1,
    originalQueue: [track("a"), track("b"), track("c"), track("d")], shuffleMode: "on" });
  assert.equal(h.player.queueNextMany([track("d"), track("b"), track("new")]), 3);
  assert.deepEqual(ids(h.queue.queue), ["c", "a", "d", "b", "new"]);
  assert.deepEqual(ids(h.queue.originalQueue), ["a", "d", "b", "new", "c"]);
  assert.equal(h.status.shuffleMode, "on");
  assert.equal(h.status.currentTrack.source_id, "a");
  h.player.toggleShuffleMode();
  assert.deepEqual(ids(h.queue.queue), ["a", "d", "b", "new", "c"]);
  assert.equal(h.status.playIndex, 0);
  assert.equal(h.status.currentTrack.source_id, "a");
  assert.equal(h.queue.originalQueue, null);
});

test("empty queue schedules unique tracks without selecting a current item or producing audio", async () => {
  const h = await harness({ state: "idle" });
  assert.equal(h.player.queueNextMany([track("a"), track("b"), track("a"), null]), 2);
  assert.deepEqual(ids(h.queue.queue), ["a", "b"]);
  assert.equal(h.status.playIndex, -1);
  assert.equal(h.status.currentTrack, null);
  assert.equal(h.status.state, "idle");
  assert.equal(h.media.track, null);
  assert.deepEqual(h.audioCalls, []);
  assert.equal(h.timers.size, 0);
});

test("with no active selection the next-up segment goes first and retains the rest", async () => {
  const h = await harness({ tracks: [track("a"), track("b")], state: "idle" });
  assert.equal(h.player.queueNextMany([track("b"), track("c")]), 2);
  assert.deepEqual(ids(h.queue.queue), ["b", "c", "a"]);
  assert.equal(h.status.playIndex, -1);
  assert.deepEqual(h.audioCalls, []);
});

test("paused playback remains paused and only successful scheduling releases repeat-one", async () => {
  const h = await harness({ tracks: [track("a"), track("b")], playIndex: 0, state: "paused", repeatMode: "one" });
  assert.equal(h.player.queueNextMany([track("a"), null]), 0);
  assert.equal(h.status.repeatMode, "one");
  assert.equal(h.player.queueNextMany([]), 0);
  assert.equal(h.status.repeatMode, "one");
  assert.equal(h.player.queueNextMany([track("b")]), 1);
  assert.equal(h.status.repeatMode, "list");
  assert.equal(h.status.state, "paused");
  assert.equal(h.status.position, 12000);
  assert.deepEqual(h.audioCalls, []);
});

test("next-up prefetch waits for current cache persistence and follows the newly scheduled song", async () => {
  const h = await harness({ tracks: [track("current"), track("old-next")], playIndex: 0, prefetch: true });
  h.audioEvents.get("playing")();
  await runTimers(h, 2500);
  assert.deepEqual(h.events, [["persist", "current"]]);
  h.player.queueNextMany([track("new-next")]);
  await runTimers(h, 2500);
  assert.deepEqual(h.events, [["persist", "current"]], "inserting next must not bypass the current cache gate");
  h.persist(); await tick(); await runTimers(h, 0);
  assert.deepEqual(h.events, [["persist", "current"], ["prefetch", "new-next"]]);
  assert.equal(h.status.currentTrack.source_id, "current");
});

test("an old next-song request queued before insertion is invalidated before it starts", async () => {
  const h = await harness({ tracks: [track("current"), track("old-next")], playIndex: 0, prefetch: true });
  h.audioEvents.get("playing")();
  await runTimers(h, 2500);
  h.persist(); await tick();
  assert.equal([...h.timers.values()].filter(({ delay }) => delay === 0).length, 1);
  h.player.queueNextMany([track("new-next")]);
  await runTimers(h, 0);
  assert.deepEqual(h.events, [["persist", "current"]], "the old next song must not start after the queue changed");
  await runTimers(h, 2500);
  await runTimers(h, 0);
  assert.deepEqual(h.events, [["persist", "current"], ["prefetch", "new-next"]]);
});

test("append keeps the existing queue and current transport while adding at the end", async () => {
  const h = await harness({ tracks: [track("past"), track("current", "lossless"), track("next"), track("tail")], playIndex: 1 });
  const current = h.status.currentTrack;
  assert.equal(h.player.insertManyToQueue([track("added")], h.queue.queueLength), 1);
  assert.deepEqual(ids(h.queue.queue), ["past", "current", "next", "tail", "added"]);
  assert.equal(h.status.playIndex, 1);
  assert.equal(h.status.currentTrack, current);
  assert.equal(h.status.currentTrack.quality, "lossless", "an append must not rewrite the playing source to the new preference");
  assert.equal(h.queue.queue.at(-1).quality, "hires");
  assert.equal(h.media.track, current);
  assert.equal(h.status.position, 12000);
  assert.equal(h.status.state, "playing");
  assert.equal(h.status.currentSource, "blob:active");
  assert.deepEqual(h.audioCalls, [], "adding a song must not play, pause, load or replace the current source");
});

test("append deduplicates existing tracks without moving or rewriting any queue item", async () => {
  const tracks = [track("past", "high"), track("current", "lossless"), track("next", "high"), track("tail")];
  const h = await harness({ tracks, playIndex: 1, originalQueue: [...tracks], shuffleMode: "on", state: "paused", repeatMode: "one" });
  const before = [...h.queue.queue], originalBefore = [...h.queue.originalQueue];
  assert.equal(h.player.insertManyToQueue([track("tail"), track("current"), track("past"), track("next")], h.queue.queueLength), 0);
  assert.deepEqual(ids(h.queue.queue), ["past", "current", "next", "tail"]);
  h.queue.queue.forEach((item, index) => assert.equal(item, before[index]));
  h.queue.originalQueue.forEach((item, index) => assert.equal(item, originalBefore[index]));
  assert.equal(h.status.currentTrack.quality, "lossless");
  assert.equal(h.status.playIndex, 1);
  assert.equal(h.status.position, 12000);
  assert.equal(h.status.state, "paused");
  assert.equal(h.status.repeatMode, "one");
  assert.equal(h.status.shuffleMode, "on");
  assert.deepEqual(h.audioCalls, []);
});

test("append to an empty queue preserves idle selection and does not start playback", async () => {
  const h = await harness({ state: "idle" });
  assert.equal(h.player.insertManyToQueue([track("first"), track("second"), track("first"), null], h.queue.queueLength), 2);
  assert.deepEqual(ids(h.queue.queue), ["first", "second"]);
  assert.equal(h.status.playIndex, -1);
  assert.equal(h.status.currentTrack, null);
  assert.equal(h.media.track, null);
  assert.equal(h.status.state, "idle");
  assert.equal(h.status.currentSource, null);
  assert.deepEqual(h.audioCalls, []);
  assert.equal(h.timers.size, 0);
});
