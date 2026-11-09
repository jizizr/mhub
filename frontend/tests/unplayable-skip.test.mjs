import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createPinia, setActivePinia } from "pinia";
import { trackIdentity, useQueueStore } from "../src/stores/queue.js";
import { trackFromSearch } from "../src/utils/track.js";
import { UnplayableError, playbackFailure } from "../src/apis/music.js";

const tick = () => new Promise((resolve) => setImmediate(resolve));
const track = (id) => trackFromSearch({ id, platform: "applemusic", song: `Song ${id}`, singer: "Synthetic artist" }, "high");
let instance = 0;

async function harness({ ids, playIndex = 0, unplayable = [], prefetch = false }) {
  const saved = new Map();
  globalThis.localStorage = { getItem: (key) => saved.get(key), setItem: (key, value) => saved.set(key, value), removeItem: (key) => saved.delete(key) };
  setActivePinia(createPinia());
  const queue = useQueueStore();
  queue.setQueue(ids.map(track));
  const toasts = [];
  const status = { volume: 0.8, speed: 1, state: "playing", playIndex, repeatMode: "off", shuffleMode: "off", quality: "high", position: 0, duration: 180000,
    autoplayEnabled: false, autoplayState: "idle", autoplayError: "", currentSource: "blob:active", currentSourceTrackId: null, trackLoading: false,
    get currentTrack() { return queue.getTrack(this.playIndex); }, get isPlaying() { return this.state === "playing" || this.state === "buffering"; },
    persist() {}, setNotice() {}, toast(message) { toasts.push(message); } };
  const media = { track: status.currentTrack, setTrack(item) { this.track = item; }, beginLyricLoad() {}, clearLyric() {} };
  const timers = new Map();
  let timerId = 0;
  globalThis.window = { location: { origin: "http://localhost" }, setTimeout(fn, delay) { timers.set(++timerId, { fn, delay }); return timerId; }, clearTimeout(id) { timers.delete(id); } };
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { connection: { saveData: false } } });
  const resolves = [], prefetches = [];
  const audio = { src: "", currentSrc: "", currentTime: 0, duration: 180, readyState: 4, paused: false,
    buffered: { length: 1, start: () => 0, end: () => 180 }, addEventListener() {},
    async play() { this.paused = false; this.currentSrc = this.src; }, pause() { this.paused = true; }, load() {}, removeAttribute() { this.src = ""; } };
  globalThis.__skipTest = { status, media, queue, trackIdentity, trackFromSearch,
    playback: { setPlaying() {}, setSeeking() {}, setDuration() {}, setCurrentTime: (value) => value, setOutputDelay() {}, reset() {} } };
  let source = await readFile(new URL("../src/core/player/index.js", import.meta.url), "utf8");
  source = source
    .replace('import { useMediaStore } from "../../stores/media";', "const useMediaStore = () => globalThis.__skipTest.media;")
    .replace('import { trackIdentity, useQueueStore } from "../../stores/queue";', "const { trackIdentity } = globalThis.__skipTest; const useQueueStore = () => globalThis.__skipTest.queue;")
    .replace('import { useStatusStore } from "../../stores/status";', "const useStatusStore = () => globalThis.__skipTest.status;")
    .replace('import * as playback from "../../services/playback";', "const playback = globalThis.__skipTest.playback;")
    .replace('import { trackFromSearch } from "../../utils/track";', "const { trackFromSearch } = globalThis.__skipTest;");
  const player = await import(`data:text/javascript;base64,${Buffer.from(source + `\n// instance ${++instance}`).toString("base64")}`);
  const fail = (item) => { throw new UnplayableError(`《${item.title}》在 Apple Music 只有旧版加密音频，暂时无法播放`); };
  player.initPlayer({ audio,
    resolveTrackSource: async (item) => {
      resolves.push(item.source_id);
      if (unplayable.includes(item.source_id)) fail(item);
      return { source: `blob:${item.source_id}`, track: item };
    },
    cacheActiveTrackSource: prefetch ? async () => true : null,
    prefetchTrackSource: prefetch ? async (item) => {
      prefetches.push(item.source_id);
      if (unplayable.includes(item.source_id)) fail(item);
      return true;
    } : null,
  });
  const runTimers = async () => {
    for (let round = 0; round < 10 && timers.size; round += 1) {
      const pending = [...timers.entries()];
      pending.forEach(([id]) => timers.delete(id));
      for (const [, { fn }] of pending) fn();
      await tick(); await tick();
    }
  };
  return { player, status, toasts, resolves, prefetches, runTimers };
}

test("a failed task with the unplayable code becomes a skippable error", () => {
  assert.equal(playbackFailure({ status: "failed", error: "x", error_code: "unplayable" }).code, "unplayable");
  assert.equal(playbackFailure({ status: "failed", error: "x" }).code, undefined);
});

test("an unplayable song is skipped at once and the next song plays", async () => {
  const h = await harness({ ids: ["a", "dead", "c"], unplayable: ["dead"] });
  await h.player.nextTrack(true);
  await h.runTimers();
  assert.equal(h.status.currentTrack.source_id, "c");
  assert.equal(h.status.state === "playing" || h.status.state === "buffering", true);
  assert.deepEqual(h.resolves, ["dead", "c"]);
  assert.ok(h.toasts.some((message) => /已跳过/.test(message)));
});

test("a song already known to be unplayable is stepped over without a request", async () => {
  const h = await harness({ ids: ["a", "dead", "c", "d"], unplayable: ["dead"] });
  await h.player.nextTrack(true); await h.runTimers();
  assert.equal(h.status.currentTrack.source_id, "c");
  h.status.playIndex = 0;
  await h.player.nextTrack(true); await h.runTimers();
  assert.equal(h.status.currentTrack.source_id, "c");
  assert.deepEqual(h.resolves, ["dead", "c", "c"]);
});

test("several unplayable songs in a row still reach a playable one", async () => {
  const h = await harness({ ids: ["a", "x1", "x2", "x3", "ok"], unplayable: ["x1", "x2", "x3"] });
  await h.player.nextTrack(true); await h.runTimers();
  assert.equal(h.status.currentTrack.source_id, "ok");
});
