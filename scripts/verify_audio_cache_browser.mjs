// Real IndexedDB regression. Uses synthetic bytes in an isolated browser.
// Optional: PLAYWRIGHT_MODULE and BROWSER_EXECUTABLE override installed tools.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const { createServer } = await import(pathToFileURL(path.join(root, "frontend/node_modules/vite/dist/node/index.js")));
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const browserName = process.env.BROWSER || "chromium";
const offlinePlayback = process.env.CACHE_TEST_OFFLINE !== "0";
const browserType = playwright[browserName];
assert.ok(browserType, `Unsupported browser ${browserName}`);
// Locally generated public-domain test tone; no account or media library used.
const fixtureDirectory = await mkdtemp(path.join(os.tmpdir(), "music-cache-flac-"));
let flacBytes;
try {
  const fixturePath = path.join(fixtureDirectory, "tone.flac");
  // Use a seekable output so STREAMINFO includes final duration/sample count.
  execFileSync(process.env.FFMPEG || "ffmpeg", ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=880:duration=1", "-ar", "96000", "-c:a", "flac", "-sample_fmt", "s32", fixturePath]);
  flacBytes = [...await readFile(fixturePath)];
} finally {
  await rm(fixtureDirectory, { recursive: true, force: true });
}
const server = await createServer({ root: path.join(root, "frontend"), server: { host: "127.0.0.1", port: 0 } });
let browser;
let persistentContext;
let profileDirectory;
try {
  await server.listen();
  const launchOptions = { headless: true, ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) };
  let page;
  if (browserName === "webkit") {
    // macOS Playwright WebKit's ephemeral context cannot prepare Blob data
    // for IndexedDB. An isolated, disposable disk profile supports it.
    profileDirectory = await mkdtemp(path.join(os.tmpdir(), "music-cache-webkit-"));
    persistentContext = await browserType.launchPersistentContext(profileDirectory, launchOptions);
    page = await persistentContext.newPage();
  } else {
    browser = await browserType.launch(launchOptions);
    page = await browser.newPage();
  }
  await page.route("**/__cache_test", route => route.fulfill({ contentType: "text/html", body: "<!doctype html><title>Audio cache regression</title>" }));
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__cache_test`);
  const results = await page.evaluate(async (flacBytes) => {
    const results = [];
    let phase = "module import";
    try {
    const idbErrors = [];
    const originalTransaction = IDBDatabase.prototype.transaction;
    IDBDatabase.prototype.transaction = function (...args) {
      const transaction = originalTransaction.apply(this, args);
      transaction.addEventListener("error", (event) => idbErrors.push(`${event.target?.error?.name}: ${event.target?.error?.message}`));
      return transaction;
    };
    window.__cacheTestIdbErrors = idbErrors;
    const cache = await import("/src/services/localCache.js");
    const check = (value, label) => { if (!value) throw new Error(label); results.push(label); };
    phase = "clear IndexedDB";
    await cache.clearCache();
    phase = "persist initial packet Blob";
    const bytes = new Uint8Array(64);
    bytes.set(new TextEncoder().encode("SPLALAC1"));
    new DataView(bytes.buffer).setUint32(8, 2);
    const blob = new Blob([bytes], { type: "application/x-splayer-alac" });
    const task = { platform: "test", track_id: "1", quality: "lossless", streaming_mode: "wasm_alac" };
    const key = cache.cacheKey(task);
    let fetches = 0;
    const originalFetch = window.fetch;
    window.fetch = async () => { fetches++; throw new Error("Unexpected duplicate download"); };
    const saved = await cache.cacheAudioBlob(key, "test:already-downloaded", task, task, { blob });
    check(fetches === 0 && cache.hasCachedAudio(saved) && saved.blob.size === bytes.length, "complete stream persisted without a second download");
    check(saved.track.status === "completed" && cache.cacheEntryMatches(saved, task), "cache is ready only after IndexedDB commit");
    const restored = await cache.getCachedTrack(key);
    check([...new Uint8Array(await restored.blob.arrayBuffer())].every((b, i) => b === bytes[i]), "persisted bytes match the stream");
    const key2 = `${key}:concurrent`;
    await Promise.all([
      cache.putCachedTrack(key2, { task, track: task, blob }),
      cache.putCachedTrack(key2, { byte_size: bytes.length }),
      cache.touchCachedTrack(key2),
    ]);
    check(cache.hasCachedAudio(await cache.getCachedTrack(key2)), "concurrent metadata writes preserve cached audio");
    const controller = new AbortController(); controller.abort();
    let aborted = false;
    try { await cache.cacheAudioBlob(`${key}:aborted`, null, task, task, { blob, signal: controller.signal }); }
    catch (error) { aborted = error.name === "AbortError"; }
    check(aborted && !(await cache.getCachedTrack(`${key}:aborted`)), "cancelled track does not persist");
    let rejected = false;
    try { await cache.cacheAudioBlob(`${key}:invalid`, null, task, task, { blob: new Blob(["not an ALAC stream"]) }); }
    catch { rejected = true; }
    check(rejected && !(await cache.getCachedTrack(`${key}:invalid`)), "incorrect representation is rejected");
    const priorities = [];
    window.fetch = async (_url, options) => {
      fetches++;
      priorities.push(options.priority);
      await new Promise(resolve => setTimeout(resolve, 10));
      return new Response(blob);
    };
    const key3 = `${key}:dedup`;
    await Promise.all([cache.cacheAudioBlob(key3, "test:download", task, task), cache.cacheAudioBlob(key3, "test:download", task, task)]);
    check(fetches === 1 && priorities[0] === "low" && cache.hasCachedAudio(await cache.getCachedTrack(key3)), "concurrent downloads are deduplicated and current-track caching remains low priority");
    window.fetch = originalFetch;

    // Exercise the actual resolver body with real IndexedDB and deterministic
    // API responses. No Vue UI or provider account is needed for negotiation.
    const { default: appSource } = await import("/src/App.vue?raw");
    const { trackFromPlayback } = await import("/src/utils/track.js");
    const start = appSource.indexOf("async function resolveTrackSource(");
    const end = appSource.indexOf("\n// Cold playback", start);
    if (start < 0 || end < 0) throw new Error("Playback resolver source not found");
    const legacyTask = { ...task, id: "legacy-native-task", streaming_mode: null };
    const legacyTrack = { ...task, source_id: task.track_id, title: "Synthetic legacy track" };
    const legacyBlob = new Blob(["legacy native audio"], { type: "audio/flac" });
    await cache.putCachedTrack(key, { task: legacyTask, track: legacyTrack, blob: legacyBlob });
    const legacy = await cache.getCachedTrack(key);
    check(!cache.cacheEntryMatches(legacy, task) && !cache.cacheEntryMatches(legacy, legacyTask), "legacy task/track transport conflicts are rejected for both playback paths");
    let responseTask = { ...task, id: "replacement-wasm-task", audio_url: "/audio", streaming_url: "/packets", status: "completed" };
    let createCalls = 0;
    const deps = {
      ...cache,
      requestedStreamingMode: () => "wasm_alac",
      status: { quality: "lossless", setNotice() {} },
      inflightTrackResolutions: new Map(),
      waitForReady: async (value) => value,
      getPlayback: async () => { throw new Error("Conflicting legacy task must not be reused"); },
      createPlayback: async () => { createCalls++; return { playback: responseTask }; },
      trackFromPlayback,
      audioUrl: (value) => value.streaming_mode === "wasm_alac" ? value.streaming_url : value.audio_url,
      refreshLocalCache() {},
    };
    const resolveTrackSource = new Function("deps", `const { ${Object.keys(deps).join(", ")} } = deps; return (${appSource.slice(start, end).trim()});`)(deps);
    const rebuilt = await resolveTrackSource(legacyTrack);
    const afterRebuild = await cache.getCachedTrack(key);
    check(createCalls === 1 && rebuilt.source === "/packets" && afterRebuild.task.id === "replacement-wasm-task" && !cache.hasCachedAudio(afterRebuild), "resolver deletes incompatible legacy bytes before merging replacement metadata");
    const repaired = await cache.cacheAudioBlob(key, null, responseTask, rebuilt.track, { blob });
    check(cache.cacheEntryMatches(repaired, responseTask) && repaired.blob.size === blob.size, "repaired packet cache contains only newly validated stream bytes");

    const nativeTask = { ...legacyTask, track_id: "native-fallback", id: "native-fallback-task", audio_url: "/native/audio", status: "completed" };
    const nativeTrack = trackFromPlayback(nativeTask, { title: "Synthetic fallback" });
    const nativeKey = cache.cacheKey(nativeTrack);
    await cache.putCachedTrack(nativeKey, { task: nativeTask, track: nativeTrack, blob: legacyBlob });
    responseTask = nativeTask;
    const fallback = await resolveTrackSource({ ...nativeTrack, streaming_mode: "wasm_alac" });
    check(fallback.source.startsWith("blob:") && fallback.track.streaming_mode === "" && cache.hasCachedAudio(await cache.getCachedTrack(nativeKey)), "server native fallback reuses its existing local Blob instead of downloading again");
    cache.releaseCachedAudioUrl(fallback.source);

    const playableFlac = new Blob([new Uint8Array(flacBytes)], { type: "audio/mp4" });
    const oldFlacTask = { ...nativeTask, track_id: "legacy-flac", id: "legacy-flac-task", quality: "hires", format: "m4a", content_type: "audio/mp4" };
    const oldFlacTrack = trackFromPlayback(oldFlacTask, { title: "Generated FLAC tone" });
    const oldFlacKey = cache.cacheKey(oldFlacTrack);
    await cache.putCachedTrack(oldFlacKey, { task: oldFlacTask, track: oldFlacTrack, blob: playableFlac });
    deps.status.quality = "hires";
    const createsBeforeReuse = createCalls;
    const reused = await resolveTrackSource({ ...oldFlacTrack, streaming_mode: "wasm_alac" });
    check(createCalls === createsBeforeReuse && reused.source.startsWith("blob:") && reused.track.format === "flac" && reused.track.streaming_mode === "", "old FLAC bytes with stale M4A metadata are reused before any task POST");
    check(!(await cache.getCachedTrack(`${oldFlacKey}:wasm_alac_v2`)) && cache.hasCachedAudio(await cache.getCachedTrack(oldFlacKey)), "native FLAC keeps its separate key and complete Blob");
    // Keep the native Blob URL for a trusted click after the browser's real
    // network is disabled. No autoplay policy override is used.
    window.__cacheTestAudio = new Audio(reused.source);
    const button = document.createElement("button");
    button.id = "play-cached-flac";
    button.textContent = "Play generated cached FLAC";
    button.onclick = () => window.__cacheTestAudio.play().then(() => { window.__cacheTestPlayStarted = true; }).catch((error) => { window.__cacheTestError = error.message; });
    document.body.append(button, window.__cacheTestAudio);
    deps.status.quality = "lossless";

    window.fetch = async (_url, options) => {
      priorities.push(options.priority);
      return new Response(blob);
    };
    responseTask = { ...task, id: "next-track-task", track_id: "next-full-priority", streaming_url: "/packets", status: "completed" };
    await resolveTrackSource({ ...responseTask, source_id: responseTask.track_id }, { prefetch: true });
    check(priorities.at(-1) === "auto" && cache.hasCachedAudio(await cache.getCachedTrack(cache.cacheKey(responseTask))), "next-track resolver downloads at normal priority after the current-track gate");

    const retryKey = `${key}:priority-retry`;
    const oldController = new AbortController();
    const newController = new AbortController();
    let rejectOld;
    window.fetch = async (_url, options) => {
      priorities.push(options.priority);
      if (options.signal === oldController.signal) return new Promise((_resolve, reject) => { rejectOld = reject; });
      return new Response(blob);
    };
    const oldRequest = cache.cacheAudioBlob(retryKey, "/old", task, task, { signal: oldController.signal }).catch(() => null);
    while (!rejectOld) await new Promise(resolve => setTimeout(resolve, 0));
    const nextRequest = cache.cacheAudioBlob(retryKey, "/next", task, task, { signal: newController.signal, priority: "auto" });
    // Allow the second read-only transaction to discover the active download.
    await new Promise(resolve => setTimeout(resolve, 20));
    rejectOld(new DOMException("old request cancelled", "AbortError"));
    await oldRequest;
    const retried = await nextRequest;
    check(priorities.at(-1) === "auto" && cache.hasCachedAudio(retried), "retry behind a failed existing download preserves normal next-track priority");
    window.fetch = originalFetch;
    return results;
    } catch (error) {
      throw new Error(`${error?.name || "Cache verification failed"}: ${error?.message || String(error)} at ${phase}, after ${results.at(-1) || "setup"}; IndexedDB: ${window.__cacheTestIdbErrors?.join("; ")}\n${error?.stack || ""}`);
    }
  }, flacBytes);
  await page.context().setOffline(offlinePlayback);
  await page.bringToFront();
  await page.locator("#play-cached-flac").click();
  await page.waitForFunction(() => window.__cacheTestError || window.__cacheTestAudio.error || window.__cacheTestAudio.currentTime > 0.2, undefined, { timeout: 10000 }).catch(async (error) => {
    const state = await page.evaluate(() => ({ time: window.__cacheTestAudio.currentTime, duration: window.__cacheTestAudio.duration, ready: window.__cacheTestAudio.readyState, network: window.__cacheTestAudio.networkState, paused: window.__cacheTestAudio.paused, started: window.__cacheTestPlayStarted, error: window.__cacheTestError || window.__cacheTestAudio.error?.message }));
    throw new Error(`${error.message}; native audio ${JSON.stringify(state)}`);
  });
  const nativePlayback = await page.evaluate(() => ({ time: window.__cacheTestAudio.currentTime, duration: window.__cacheTestAudio.duration, error: window.__cacheTestError || window.__cacheTestAudio.error?.message || null }));
  assert.equal(nativePlayback.error, null);
  assert.ok(nativePlayback.time > 0.2);
  results.push(`complete legacy FLAC plays through native audio after a trusted click${offlinePlayback ? " with the browser offline" : ""}`);
  assert.equal(results.length, 16);
  console.log(JSON.stringify({ browser: browserName, offlinePlayback, passed: results.length, nativePlayback, checks: results }, null, 2));
} finally {
  await persistentContext?.close();
  await browser?.close();
  await server.close();
  if (profileDirectory) await rm(profileDirectory, { recursive: true, force: true });
}
