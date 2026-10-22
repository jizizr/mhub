import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const tick = () => new Promise((resolve) => setImmediate(resolve));
let moduleId = 0;
async function harness({ saveData = false, failPersistenceOnce = false, growing = false } = {}) {
  const current = { id: "current", platform: "applemusic", quality: "lossless", streaming_mode: "wasm_alac", duration: 1000 };
  const next = { ...current, id: "next" };
  const status = { volume: 0.8, speed: 1, state: "paused", playIndex: 0, repeatMode: "list", currentTrack: current, quality: "lossless", persist() {}, setNotice() {} };
  const media = { track: current, beginLyricLoad() {}, setTrack(track) { this.track = track; } };
  const queue = { queueLength: 2, getTrack: (i) => [current, next][i] };
  const timers = new Map();
  let timerId = 0;
  globalThis.window = { location: { origin: "http://localhost" }, setTimeout(fn, delay) { timers.set(++timerId, { fn, delay }); return timerId; }, clearTimeout(id) { timers.delete(id); } };
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { connection: { saveData } } });
  globalThis.localStorage = { removeItem() {} };
  globalThis.__playerTest = {
    status, media, queue,
    playback: { setPlaying() {}, setSeeking() {}, setDuration() {}, setCurrentTime: (value) => value },
  };
  let source = await readFile(new URL("../src/core/player/index.js", import.meta.url), "utf8");
  source = source
    .replace('import { useMediaStore } from "../../stores/media";', 'const useMediaStore = () => globalThis.__playerTest.media;')
    .replace('import { trackIdentity, useQueueStore } from "../../stores/queue";', 'const trackIdentity = (track) => track?.id; const useQueueStore = () => globalThis.__playerTest.queue;')
    .replace('import { useStatusStore } from "../../stores/status";', 'const useStatusStore = () => globalThis.__playerTest.status;')
    .replace('import * as playback from "../../services/playback";', 'const playback = globalThis.__playerTest.playback;')
    .replace('import { trackFromSearch } from "../../utils/track";', 'const trackFromSearch = (track) => track;');
  const player = await import(`data:text/javascript;base64,${Buffer.from(source + `\n// test instance ${++moduleId}`).toString("base64")}`);
  const audio = { src: "", currentSrc: "", addEventListener() {}, pause() {}, removeAttribute() { this.src = ""; }, load() {} };
  const worklet = { eof: !growing, completeBlob: growing ? null : new Blob(["complete packets"]), stop() {}, setVolume() {},
    load: async (_source, options) => { worklet.loadOptions = options; return { duration: 1000 }; } };
  const events = [];
  let persist;
  let persistAttempts = 0;
  player.initPlayer({
    audio, alacPlayerFactory: { canHandle: () => true, create: () => worklet },
    waitForCompletedSource: async (track, { signal }) => {
      assert.equal(track.id, "current");
      assert.equal(signal.aborted, false);
      return "test:current";
    },
    cacheActiveTrackSource: async (track, { blob, signal }) => {
      events.push({ type: "persist", track, blob, signal });
      persistAttempts += 1;
      if (failPersistenceOnce && persistAttempts === 1) throw new DOMException("Temporary cache write failure", "QuotaExceededError");
      return new Promise((resolve) => { persist = resolve; });
    },
    prefetchTrackSource: async (track) => { events.push({ type: "prefetch", track }); return true; },
  });
  await player.load("test:current", false, current);
  return { player, worklet, events, timers, persist: () => persist(true) };
}

async function runTimers(h) {
  const pending = [...h.timers.values()];
  h.timers.clear();
  pending.forEach(({ fn }) => fn());
  await tick();
}

test("ALAC EOF starts the next request immediately after persistence without another startup delay", async () => {
  const h = await harness();
  h.worklet.onStreamComplete({ duration: 1000 });
  await tick();
  assert.equal(h.events.length, 1);
  assert.equal(h.events[0].type, "persist");
  assert.equal(h.events[0].blob, h.worklet.completeBlob);
  assert.equal(h.timers.size, 0, "next-track timer waits for successful persistence");
  h.persist(); await tick();
  assert.deepEqual([...h.timers.values()].map(({ delay }) => delay), [0], "only a cooperative yield is allowed after the current song is fully cached");
  await runTimers(h); // prefetch's cooperative zero-delay yield
  assert.deepEqual(h.events.map((e) => e.type), ["persist", "prefetch"]);
  assert.equal(h.events[1].track.id, "next");
});

test("Data Saver still persists received ALAC bytes but blocks next-track network prefetch", async () => {
  const h = await harness({ saveData: true });
  h.worklet.onStreamComplete({ duration: 1000 });
  await tick();
  assert.equal(h.events[0].type, "persist");
  h.persist(); await tick();
  assert.equal(h.timers.size, 0, "Data Saver blocks even an immediate prefetch attempt");
  await runTimers(h);
  assert.deepEqual(h.events.map((e) => e.type), ["persist"]);
});

test("a rejected cache write can retry and still gates the next song", async () => {
  const h = await harness({ failPersistenceOnce: true });
  const warn = console.warn;
  const warnings = [];
  console.warn = (...args) => warnings.push(args);
  try {
    h.worklet.onStreamComplete({ duration: 1000 });
    await tick();
  } finally {
    console.warn = warn;
  }
  assert.equal(warnings.length, 1);
  assert.deepEqual(h.events.map((e) => e.type), ["persist"]);
  await runTimers(h);
  assert.deepEqual(h.events.map((e) => e.type), ["persist", "persist"], "retry must not reuse the rejected promise");
  assert.equal(h.events[1].blob, h.worklet.completeBlob);
  h.persist(); await tick();
  await runTimers(h);
  assert.deepEqual(h.events.map((e) => e.type), ["persist", "persist", "prefetch"]);
});

test("backend completion alone cannot persist partial audio or start next-track prefetch", async () => {
  const h = await harness({ growing: true });
  const completedSource = await h.worklet.loadOptions.waitForCompletedSource({ signal: new AbortController().signal });
  assert.equal(completedSource, "test:current");
  h.worklet.onState("playing"); await tick(); await runTimers(h);
  assert.equal(h.worklet.eof, false);
  assert.deepEqual(h.events, []);
  assert.equal(h.timers.size, 0);
  // Only the actual reader EOF makes an immutable Blob available. Even then
  // the next song must wait until that Blob's storage write succeeds.
  h.worklet.eof = true; h.worklet.completeBlob = new Blob(["complete packets"]);
  h.worklet.onStreamComplete({ duration: 1000 }); await tick();
  assert.deepEqual(h.events.map((event) => event.type), ["persist"]);
  assert.equal(h.timers.size, 0);
  h.persist(); await tick(); await runTimers(h);
  assert.deepEqual(h.events.map((event) => event.type), ["persist", "prefetch"]);
});
