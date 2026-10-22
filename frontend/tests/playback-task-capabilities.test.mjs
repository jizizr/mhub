import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { audioUrl } from "../src/apis/music.js";
import { trackFromPlayback } from "../src/utils/track.js";
import { cacheKey, cacheEntryMatches, reusableNativeFlacCache } from "../src/services/localCache.js";

const appSource = await readFile(new URL("../src/App.vue", import.meta.url), "utf8");
const start = appSource.indexOf("async function waitForReady(");
const end = appSource.indexOf("\n/**", start);
assert.ok(start >= 0 && end > start);
const functionSource = appSource.slice(start, end).trim();
function harness(responses = []) {
  let polls = 0;
  const getPlayback = async () => {
    assert.ok(polls < responses.length, "unexpected extra poll");
    return responses[polls++];
  };
  const window = { setTimeout: (resolve) => queueMicrotask(resolve) };
  const waitForReady = new Function("getPlayback", "window", `return (${functionSource});`)(getPlayback, window);
  return { waitForReady, polls: () => polls };
}
const shared = {
  id: "shared-task", platform: "applemusic", quality: "lossless", status: "streaming",
  streaming_mode: "wasm_alac", streaming_url: "/api/v1/playbacks/shared-task/audio/stream",
  streaming_codec: "alac", streaming_protocol: "splayer-alac-v2",
  audio_url: "/api/v1/playbacks/shared-task/audio",
};

for (const quality of ["lossless", "hires"]) {
  test(`native ${quality} waits for completion even when create cleared the shared WASM mode`, async () => {
    const completed = { ...shared, quality, status: "completed" };
    const h = harness([{ ...shared, quality }, completed]);
    const result = await h.waitForReady({ ...shared, quality, streaming_mode: null, streaming_url: null });
    assert.equal(h.polls(), 2);
    assert.equal(result.status, "completed");
    for (const field of ["streaming_mode", "streaming_url", "streaming_codec", "streaming_protocol"]) assert.equal(result[field], null);
    assert.equal(completed.streaming_mode, "wasm_alac", "shared server task must remain unchanged");
    assert.equal(audioUrl(result), "/api/v1/playbacks/shared-task/audio?v=shared-task");
    assert.equal(trackFromPlayback(result, shared).streaming_mode, "");
  });
}

test("native metadata-cache rejoin also waits for a shared WASM task", async () => {
  const h = harness([{ ...shared, status: "completed" }]);
  const result = await h.waitForReady(shared, { allowAlacStream: false });
  assert.equal(h.polls(), 1);
  assert.equal(result.streaming_mode, null);
  assert.ok(!audioUrl(result).includes("/stream"));
});

test("completed shared task is sanitized without further polling for native playback", async () => {
  const h = harness();
  const result = await h.waitForReady({ ...shared, status: "completed" });
  assert.equal(h.polls(), 0);
  assert.equal(result.streaming_mode, null);
});

test("WASM-capable playback returns the first streaming fragment immediately", async () => {
  const h = harness();
  assert.equal(await h.waitForReady(shared, { allowAlacStream: true }), shared);
  assert.equal(h.polls(), 0);
  assert.ok(audioUrl(shared).includes("/audio/stream?"));
});

test("WASM-capable queued task stops polling as soon as streaming becomes ready", async () => {
  const h = harness([shared]);
  const result = await h.waitForReady({ ...shared, status: "queued" }, { allowAlacStream: true });
  assert.equal(h.polls(), 1);
  assert.equal(result.streaming_mode, "wasm_alac");
  assert.equal(result.status, "streaming");
});

test("native AAC retains first-fragment startup", async () => {
  const h = harness();
  const result = await h.waitForReady({ ...shared, quality: "high", streaming_mode: null, streaming_url: null });
  assert.equal(h.polls(), 0);
  assert.equal(result.status, "streaming");
  assert.equal(result.streaming_mode, null);
});

test("failure while native playback waits is surfaced instead of returning packet audio", async () => {
  const h = harness([{ ...shared, status: "failed", error: "provider failed" }]);
  await assert.rejects(h.waitForReady(shared), /provider failed/);
  assert.equal(h.polls(), 1);
});

for (const fromMetadataCache of [false, true]) {
  for (const allowAlacStream of [false, true]) {
    test(`${fromMetadataCache ? "cached" : "new"} task resolution negotiates ${allowAlacStream ? "WASM" : "native"} playback using current browser capability`, async () => {
      const resolverSource = appSource.slice(appSource.indexOf("async function resolveTrackSource("), appSource.indexOf("\n// Cold playback"));
      const requestTrack = { ...shared, source_id: "song-id", selected_id: "song-id", streaming_mode: allowAlacStream ? "wasm_alac" : null };
      const h = harness([{ ...shared, status: "completed" }]);
      const capabilities = [];
      let creations = 0;
      const deps = {
        requestedStreamingMode: () => allowAlacStream ? "wasm_alac" : "",
        status: { quality: "lossless", setNotice() {} },
        inflightTrackResolutions: new Map(), cacheKey, cacheEntryMatches, reusableNativeFlacCache, audioUrl, trackFromPlayback,
        getCachedTrack: async () => fromMetadataCache ? { task: requestTrack, track: requestTrack } : null,
        hasCachedAudio: () => false,
        putCachedTrack: async (_key, value) => value,
        deleteCachedTrack: async () => {}, refreshLocalCache() {},
        getPlayback: async () => shared,
        createPlayback: async () => { creations++; return { playback: requestTrack }; },
        waitForReady: async (task, options) => { capabilities.push(options.allowAlacStream); return h.waitForReady(task, options); },
      };
      const resolve = new Function("deps", `const { ${Object.keys(deps).join(", ")} } = deps; return (${resolverSource.trim()});`)(deps);
      const resolved = await resolve(requestTrack);
      assert.deepEqual(capabilities, [allowAlacStream]);
      assert.equal(creations, fromMetadataCache && !allowAlacStream ? 0 : 1);
      assert.equal(h.polls(), allowAlacStream ? 0 : 1);
      assert.equal(resolved.track.streaming_mode, allowAlacStream ? "wasm_alac" : "");
      assert.equal(resolved.source.includes("/audio/stream?"), allowAlacStream);
      assert.equal(resolved.track.status, allowAlacStream ? "streaming" : "completed");
    });
  }
}

for (const upgraded of [true, false]) {
  test(`WASM metadata restored after server restart ${upgraded ? "renegotiates the packet stream" : "respects a declined transport"}`, async () => {
    const resolverSource = appSource.slice(appSource.indexOf("async function resolveTrackSource("), appSource.indexOf("\n// Cold playback"));
    const old = { ...shared, source_id: "song-id", selected_id: "song-id", status: "completed" };
    const response = { ...old, streaming_mode: upgraded ? "wasm_alac" : null, streaming_url: upgraded ? shared.streaming_url : null };
    const requests = [];
    const deps = {
      requestedStreamingMode: () => "wasm_alac", status: { quality: "hires", setNotice() {} },
      inflightTrackResolutions: new Map(), cacheKey, cacheEntryMatches, reusableNativeFlacCache, audioUrl, trackFromPlayback,
      getCachedTrack: async () => ({ task: { ...old, quality: "hires" }, track: { ...old, quality: "hires" } }),
      hasCachedAudio: () => false,
      putCachedTrack: async (_key, value) => value,
      deleteCachedTrack: async () => {}, refreshLocalCache() {},
      getPlayback: async () => { throw new Error("GET cannot negotiate restored packet audio"); },
      createPlayback: async (request) => { requests.push(request); return { playback: { ...response, quality: "hires" } }; },
      waitForReady: async (task) => task,
    };
    const resolve = new Function("deps", `const { ${Object.keys(deps).join(", ")} } = deps; return (${resolverSource.trim()});`)(deps);
    const resolved = await resolve({ ...old, quality: "hires" });
    assert.equal(requests.length, 1);
    assert.equal(requests[0].selectedId, "song-id");
    assert.equal(requests[0].quality, "hires");
    assert.equal(requests[0].streamingMode, "wasm_alac");
    assert.equal(resolved.source.includes("/audio/stream?"), upgraded);
    assert.equal(resolved.track.streaming_mode, upgraded ? "wasm_alac" : "");
  });
}

test("a failed WASM metadata renegotiation is surfaced without a second POST", async () => {
  const resolverSource = appSource.slice(appSource.indexOf("async function resolveTrackSource("), appSource.indexOf("\n// Cold playback"));
  const old = { ...shared, source_id: "song-id", selected_id: "song-id" };
  let creations = 0;
  const deps = {
    requestedStreamingMode: () => "wasm_alac", status: { quality: "lossless", setNotice() {} },
    inflightTrackResolutions: new Map(), cacheKey, cacheEntryMatches, reusableNativeFlacCache, audioUrl, trackFromPlayback,
    getCachedTrack: async () => ({ task: old, track: old }), hasCachedAudio: () => false,
    deleteCachedTrack: async () => {},
    createPlayback: async () => { creations += 1; throw new Error("temporary provider failure"); },
  };
  const resolve = new Function("deps", `const { ${Object.keys(deps).join(", ")} } = deps; return (${resolverSource.trim()});`)(deps);
  await assert.rejects(resolve(old), /temporary provider failure/);
  assert.equal(creations, 1);
});

function completionHarness(responses) {
  const start = appSource.indexOf("async function waitForCompletedSource(");
  const source = appSource.slice(start, appSource.indexOf("\n/**", start)).trim();
  const requests = [];
  const timers = new Map();
  let timerId = 0;
  const window = {
    setTimeout(resolve) { const id = ++timerId; timers.set(id, resolve); return id; },
    clearTimeout(id) { timers.delete(id); },
  };
  const getPlayback = async (id, options) => {
    requests.push({ id, options });
    assert.ok(responses.length, "unexpected extra completion poll");
    const response = responses.shift();
    if (response instanceof Error) throw response;
    return typeof response === "function" ? response(options) : response;
  };
  const waitForCompletedSource = new Function("getPlayback", "audioUrl", "window", `return (${source});`)(getPlayback, audioUrl, window);
  return { waitForCompletedSource, requests, timers, advance: () => [...timers.values()].forEach((run) => run()) };
}
const flush = () => new Promise((resolve) => setImmediate(resolve));

test("completion monitoring passes its cancellation signal and returns only the completed packet URL", async () => {
  const h = completionHarness([shared, new Error("temporary poll failure"), { ...shared, status: "completed" }]);
  const controller = new AbortController();
  const result = h.waitForCompletedSource({ ...shared, id: "source-song", playback_id: shared.id }, { signal: controller.signal });
  assert.equal(h.requests.length, 0, "the fresh streaming status is not queried again immediately");
  h.advance();
  await flush(); h.advance(); await flush(); h.advance();
  assert.equal(await result, audioUrl(shared));
  assert.equal(h.requests.length, 3);
  assert.ok(h.requests.every(({ id, options }) => id === shared.id && options.signal === controller.signal));
  assert.equal(h.timers.size, 0);
});

test("completion monitoring stops immediately on EOF or track cancellation", async () => {
  const h = completionHarness([shared]);
  const controller = new AbortController();
  const result = h.waitForCompletedSource(shared, { signal: controller.signal });
  await flush(); assert.equal(h.timers.size, 1);
  controller.abort();
  assert.equal(await result, null);
  assert.equal(h.timers.size, 0);
  assert.equal(h.requests.length, 0, "EOF cancels the initial wait without a status request");
});

test("completion monitoring does not poll completed sources or adopt a declined/failed packet task", async () => {
  const h = completionHarness([]);
  assert.equal(await h.waitForCompletedSource({ ...shared, status: "completed" }), null);
  assert.equal(h.requests.length, 0);
  for (const response of [{ ...shared, status: "completed", streaming_mode: null }, { ...shared, status: "failed" }]) {
    const pending = completionHarness([response]);
    const result = pending.waitForCompletedSource(shared);
    pending.advance();
    assert.equal(await result, null);
    assert.equal(pending.requests.length, 1);
    assert.equal(pending.timers.size, 0);
  }
});

test("completion monitoring aborts an already pending status read without scheduling another poll", async () => {
  const h = completionHarness([({ signal }) => new Promise((resolve, reject) => {
    signal.addEventListener("abort", () => reject(new DOMException("Cancelled", "AbortError")), { once: true });
  })]);
  const controller = new AbortController();
  const result = h.waitForCompletedSource(shared, { signal: controller.signal });
  h.advance(); await flush();
  assert.equal(h.requests.length, 1);
  controller.abort();
  assert.equal(await result, null);
  assert.equal(h.timers.size, 0);
});

test("native streaming seeks can wait for a completed range-capable audio URL", async () => {
  const h = completionHarness([{ ...shared, status: "completed" }]);
  const source = await h.waitForCompletedSource({ ...shared, streaming_mode: null });
  assert.equal(source, "/api/v1/playbacks/shared-task/audio?v=shared-task");
  assert.equal(h.requests.length, 1);
});
