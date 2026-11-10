import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createPinia, setActivePinia } from "pinia";
import { trackIdentity, useQueueStore } from "../src/stores/queue.js";
import { trackFromSearch } from "../src/utils/track.js";

// iOS 16 Safari can lose the bytes of a Blob stored in IndexedDB; playing its
// blob: URL then fails with NotSupportedError. The player must drop that one
// cached record and reload the song from the network instead of leaving it
// unplayable in that browser.

const tick = () => new Promise((resolve) => setImmediate(resolve));
const track = (id) => trackFromSearch({ id, platform: "netease", song: `Song ${id}`, singer: "Synthetic artist" }, "high");
let instance = 0;

async function harness({ dropResult = true, failWith = "play" } = {}) {
  const saved = new Map();
  globalThis.localStorage = { getItem: (key) => saved.get(key), setItem: (key, value) => saved.set(key, value), removeItem: (key) => saved.delete(key) };
  setActivePinia(createPinia());
  const queue = useQueueStore();
  queue.setQueue(["a", "b"].map(track));
  const toasts = [], notices = [];
  const status = { volume: 0.8, speed: 1, state: "paused", playIndex: 0, repeatMode: "off", shuffleMode: "off", quality: "high", position: 0, duration: 180000,
    autoplayEnabled: false, autoplayState: "idle", autoplayError: "", currentSource: null, currentSourceTrackId: null, trackLoading: false,
    get currentTrack() { return queue.getTrack(this.playIndex); }, get isPlaying() { return this.state === "playing" || this.state === "buffering"; },
    persist() {}, setNotice(message) { notices.push(message); }, toast(message) { toasts.push(message); } };
  const media = { track: status.currentTrack, setTrack(item) { this.track = item; }, beginLyricLoad() {}, clearLyric() {} };
  globalThis.window = { location: { origin: "http://localhost" }, setTimeout: () => 0, clearTimeout() {} };
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { connection: { saveData: false } } });
  const listeners = new Map();
  const audio = { src: "", currentSrc: "", currentTime: 0, duration: 180, readyState: 4, paused: true, error: null,
    buffered: { length: 1, start: () => 0, end: () => 180 },
    addEventListener(name, fn) { listeners.set(name, [...(listeners.get(name) || []), fn]); },
    async play() {
      if (this.src.startsWith("blob:")) {
        if (failWith === "event") {
          this.error = { code: 4 };
          for (const fn of listeners.get("error") || []) fn();
        }
        throw new DOMException("The operation is not supported.", "NotSupportedError");
      }
      this.error = null; this.paused = false; this.currentSrc = this.src;
    },
    pause() { this.paused = true; }, load() { this.currentSrc = this.src; }, removeAttribute() { this.src = ""; } };
  globalThis.__blobTest = { status, media, queue, trackIdentity, trackFromSearch,
    playback: { setPlaying() {}, setSeeking() {}, setDuration() {}, setCurrentTime: (value) => value, setOutputDelay() {}, reset() {} } };
  let source = await readFile(new URL("../src/core/player/index.js", import.meta.url), "utf8");
  source = source
    .replace('import { useMediaStore } from "../../stores/media";', "const useMediaStore = () => globalThis.__blobTest.media;")
    .replace('import { trackIdentity, useQueueStore } from "../../stores/queue";', "const { trackIdentity } = globalThis.__blobTest; const useQueueStore = () => globalThis.__blobTest.queue;")
    .replace('import { useStatusStore } from "../../stores/status";', "const useStatusStore = () => globalThis.__blobTest.status;")
    .replace('import * as playback from "../../services/playback";', "const playback = globalThis.__blobTest.playback;")
    .replace('import { trackFromSearch } from "../../utils/track";', "const { trackFromSearch } = globalThis.__blobTest;");
  const player = await import(`data:text/javascript;base64,${Buffer.from(source + `\n// instance ${++instance}`).toString("base64")}`);
  const cached = new Set(["a"]);
  const resolves = [], drops = [];
  player.initPlayer({ audio,
    resolveTrackSource: async (item) => {
      resolves.push(item.source_id);
      return { source: cached.has(item.source_id) ? `blob:${item.source_id}` : `https://media.invalid/${item.source_id}`, track: item };
    },
    dropCachedTrackSource: async (url) => {
      drops.push(url);
      if (dropResult) cached.delete(url.slice("blob:".length));
      return dropResult;
    },
  });
  const settle = async () => { for (let round = 0; round < 20; round += 1) await tick(); };
  return { player, status, audio, toasts, notices, resolves, drops, settle };
}

for (const failWith of ["play", "event"]) {
  test(`an unreadable cached Blob reloads the song from the network (${failWith})`, async () => {
    const h = await harness({ failWith });
    await h.player.playAtIndex(0);
    await h.settle();
    assert.deepEqual(h.drops, ["blob:a"]);
    assert.deepEqual(h.resolves, ["a", "a"]);
    assert.equal(h.audio.src, "https://media.invalid/a");
    assert.equal(h.status.currentTrack.source_id, "a");
    assert.ok(h.status.isPlaying);
    assert.deepEqual(h.toasts, []);
    assert.ok(h.notices.some((message) => /改为在线播放/.test(message)));
  });
}

test("a Blob the app cannot drop pauses with advice instead of looping", async () => {
  const h = await harness({ dropResult: false });
  await h.player.playAtIndex(0);
  await h.settle();
  assert.deepEqual(h.drops, ["blob:a"]);
  assert.deepEqual(h.resolves, ["a"]);
  assert.equal(h.status.state, "paused");
  assert.ok(h.toasts.some((message) => /本地缓存无法读取/.test(message)));
});
