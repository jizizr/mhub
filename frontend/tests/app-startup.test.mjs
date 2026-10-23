import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

const source = await readFile(new URL("../src/App.vue", import.meta.url), "utf8");
const between = (start, end) => {
  const from = source.indexOf(start);
  const to = source.indexOf(end, from + start.length);
  assert.ok(from >= 0 && to > from, `Missing application hook: ${start}`);
  return source.slice(from, to);
};
const mounted = between("onMounted(", "\n\nonBeforeUnmount(");
const unmounted = between("onBeforeUnmount(", "\n\nwatch(");
const refresh = between("async function refreshLocalCache()", "\nasync function removeCacheItem(");
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const tick = () => new Promise((resolve) => setImmediate(resolve));

function harness() {
  const platformRequest = deferred();
  const cacheRequest = deferred();
  const timers = new Map();
  const state = { platforms: 0, recommendations: 0, cache: 0, ticks: 0, warnings: [] };
  const context = {
    console: { warn: (...args) => state.warnings.push(args) },
    window: { addEventListener() {}, removeEventListener() {},
      setInterval(callback) { timers.set(1, callback); return 1; },
      clearInterval(id) { timers.delete(id); } },
    document: { addEventListener() {}, removeEventListener() {}, body: { classList: { remove() {} } } },
    player: { initPlayer() {}, destroyPlayer() {} },
    platforms: { value: [] }, cacheItems: { value: [] }, cacheLoading: { value: false }, audioRef: { value: {} },
    status: { setNotice() {} },
    getPlatforms: () => { state.platforms += 1; return platformRequest.promise; },
    listCachedTracks: () => { state.cache += 1; return cacheRequest.promise; },
    loadRecommendations: () => { state.recommendations += 1; return Promise.resolve(); },
    syncUiPlaybackTime: () => { state.ticks += 1; },
    onMounted: (callback) => { state.mount = callback; },
    onBeforeUnmount: (callback) => { state.unmount = callback; },
  };
  for (const name of ["handlePopstate", "handleDocumentPointerDown", "handleDocumentKeydown", "handleAudioGesture",
    "trustedMediaOrigins", "resolveTrackSource", "resolveCachedTrackSource", "prepareOutput", "preparePlaybackSession",
    "createNativeOutput", "createMediaSessionController", "createPlaybackLifecycle", "getCachedLoudness", "warmLoudness",
    "waitForCompletedSource", "prefetchTrackSource", "cacheActiveTrackSource", "loadLyricsForTrack", "getContinuationRecommendations",
    "canUseAlacWorklet", "canRequestAlacWorklet", "loadAlacModule"]) context[name] = () => {};
  vm.createContext(context);
  vm.runInContext(`let appUnmounted = false, uiSyncTimer = 0;
    let searchAbort = null, recommendationAbort = null, recommendationRequestId = 0, recommendationDetailVersion = 0;
    const UI_SYNC_INTERVAL_MS = 250;\n${refresh}\n${mounted}\n${unmounted}`, context);
  return { context, state, timers, platformRequest, cacheRequest };
}

test("slow platform and IndexedDB reads do not gate recommendations or the UI clock", async () => {
  const h = harness();
  h.state.mount();
  assert.equal(h.state.platforms, 1);
  assert.equal(h.state.recommendations, 1, "recommendations start before platforms resolves");
  assert.equal(h.state.cache, 1);
  assert.equal(h.state.ticks, 1);
  assert.equal(h.timers.size, 1);
  h.timers.get(1)();
  assert.equal(h.state.ticks, 2, "the UI clock also runs while IndexedDB is pending");
  h.platformRequest.resolve([{ id: "applemusic", label: "Apple Music" }]);
  h.cacheRequest.resolve([{ key: "fixture" }]);
  await tick();
  assert.equal(h.context.platforms.value[0].id, "applemusic");
  assert.equal(h.context.cacheItems.value[0].key, "fixture");
  h.state.unmount();
});

test("unmount prevents late startup results and cannot restart a cleared clock", async () => {
  const h = harness();
  h.state.mount();
  h.state.unmount();
  h.platformRequest.resolve([{ id: "late" }]);
  h.cacheRequest.resolve([{ key: "late" }]);
  await tick();
  assert.equal(h.timers.size, 0);
  assert.equal(h.context.platforms.value.length, 0);
  assert.equal(h.context.cacheItems.value.length, 0);
  assert.equal(h.state.recommendations, 1, "no delayed second recommendation request");
});

test("unavailable IndexedDB leaves the app clock and platform fallback usable", async () => {
  const h = harness();
  h.state.mount();
  h.platformRequest.reject(new Error("offline"));
  h.cacheRequest.reject(new Error("storage unavailable"));
  await tick();
  assert.equal(h.context.platforms.value.length, 3);
  assert.equal(h.context.cacheLoading.value, false);
  assert.equal(h.state.warnings.length, 1);
  assert.equal(h.timers.size, 1);
  assert.equal(h.state.recommendations, 1);
  h.state.unmount();
});
