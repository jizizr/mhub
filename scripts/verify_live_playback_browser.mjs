/**
 * Real public-catalog playback through an explicitly selected API origin.
 * Uses the current frontend decoder and a real Chromium audio render thread.
 * It never requests recommendations, account data or private playlists.
 *
 * MUSIC_API_ORIGIN=http://127.0.0.1:18083 APPLE_MUSIC_TEST_ID=1398581242 \
 * APPLE_MUSIC_TEST_QUALITY=lossless node scripts/verify_live_playback_browser.mjs
 * Optional: PLAYWRIGHT_MODULE, BROWSER_EXECUTABLE, STOP_AFTER_MS (default 2500),
 * PROBE_NATIVE=0 to measure startup without extra compatibility round trips.
 */
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
assert(process.env.MUSIC_API_ORIGIN, "MUSIC_API_ORIGIN must explicitly select the API under test");
const apiOrigin = new URL(process.env.MUSIC_API_ORIGIN);
assert(["http:", "https:"].includes(apiOrigin.protocol) && !apiOrigin.username && !apiOrigin.password, "Use an HTTP API origin without embedded credentials");
const trackId = process.env.APPLE_MUSIC_TEST_ID;
assert(/^\d{6,}$/.test(trackId || ""), "APPLE_MUSIC_TEST_ID must be a public Apple Music song ID");
const quality = process.env.APPLE_MUSIC_TEST_QUALITY || "lossless";
assert(["standard", "high", "lossless", "hires"].includes(quality), "Unsupported quality");
const stopAfterMs = Math.max(500, Math.min(60000, Number(process.env.STOP_AFTER_MS) || 2500));
const probeNative = process.env.PROBE_NATIVE !== "0";
const { createServer } = await import(pathToFileURL(path.join(root, "frontend/node_modules/vite/dist/node/index.js")));
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const server = await createServer({
  configFile: false,
  root: path.join(root, "frontend"),
  server: { host: "127.0.0.1", port: 0, proxy: { "/api": apiOrigin.origin } },
  plugins: [{
    name: "public-playback-verification",
    configureServer(vite) {
      vite.middlewares.use((request, response, next) => {
        if (request.url !== "/__playback_test") return next();
        response.setHeader("content-type", "text/html");
        response.end("<!doctype html><title>Public audio playback verification</title>");
      });
    },
  }],
});
let browser;
function redact(value) {
  if (typeof value === "string") return value.replace(/(?:https?|skd):\/\/[^\s"'<>]+/gi, "[redacted URL]").replace(/eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g, "[redacted token]");
  if (Array.isArray(value)) return value.map(redact);
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, redact(item)]));
  return value;
}
try {
  await server.listen();
  browser = await chromium.launch({
    headless: true,
    ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}),
    args: ["--autoplay-policy=no-user-gesture-required"],
  });
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__playback_test`);
  const result = await page.evaluate(async ({ trackId, quality, stopAfterMs, probeNative }) => {
    const started = performance.now();
    const elapsed = () => Math.round(performance.now() - started);
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const getTask = async (id) => (await fetch(`/api/v1/playbacks/${id}`, { signal: AbortSignal.timeout(45000) })).json();
    const wasm = quality === "lossless" || quality === "hires";
    const create = async (streamingMode) => {
      const response = await fetch("/api/v1/playbacks", {
        method: "POST", signal: AbortSignal.timeout(45000), headers: { "content-type": "application/json" },
        body: JSON.stringify({ platform: "applemusic", query: trackId, selected_id: trackId, quality, include_cover: false, streaming_mode: streamingMode }),
      });
      if (!response.ok) throw new Error(`create playback HTTP ${response.status}`);
      return (await response.json()).playback;
    };
    let task = await create(wasm ? "wasm_alac" : null);
    const phases = [{ status: task.status, ms: elapsed(), mode: task.streaming_mode }];
    while (!["streaming", "completed", "failed"].includes(task.status)) {
      if (elapsed() > 120000) throw new Error("Playback task timed out");
      await sleep(300);
      task = await getTask(task.id);
      if (task.status !== phases.at(-1).status || task.streaming_mode !== phases.at(-1).mode) phases.push({ status: task.status, ms: elapsed(), mode: task.streaming_mode });
    }
    if (task.status === "failed") return { trackId, quality, phases, error: task.error };
    if (wasm && task.streaming_mode !== "wasm_alac") return { trackId, quality, phases, fallback: true };
    if (!wasm) {
      const audio = new Audio();
      audio.preload = "auto";
      audio.src = task.audio_url;
      const context = new AudioContext();
      const source = context.createMediaElementSource(audio);
      const analyser = context.createAnalyser();
      analyser.fftSize = 2048;
      source.connect(analyser).connect(context.destination);
      let rms = 0;
      let playingMs = 0;
      let firstProgressMs = 0;
      let waiting = 0;
      let error = null;
      audio.addEventListener("playing", () => { if (!playingMs) playingMs = elapsed(); });
      audio.addEventListener("waiting", () => { waiting += 1; });
      audio.addEventListener("timeupdate", () => { if (!firstProgressMs && audio.currentTime > 0) firstProgressMs = elapsed(); });
      audio.addEventListener("error", () => { error = { code: audio.error?.code, message: audio.error?.message }; });
      const pcm = new Float32Array(analyser.fftSize);
      const meter = setInterval(() => {
        analyser.getFloatTimeDomainData(pcm);
        rms = Math.max(rms, Math.sqrt(pcm.reduce((sum, value) => sum + value * value, 0) / pcm.length));
      }, 20);
      try {
        await context.resume();
        await audio.play();
        while (audio.currentTime * 1000 < stopAfterMs && !error) {
          if (elapsed() > 120000) throw new Error("Native playback timed out");
          await sleep(30);
        }
        return { trackId, quality, phases, playingMs, firstProgressMs, timeMs: audio.currentTime * 1000, rms, waiting, readyState: audio.readyState, error };
      } finally {
        clearInterval(meter);
        audio.pause();
        audio.removeAttribute("src");
        audio.load();
        source.disconnect();
        await context.close();
      }
    }
    let nativeProbe = null;
    if (probeNative && task.status === "streaming") {
      const nativeTask = await create(null);
      const nativeAudio = await fetch(task.audio_url, { signal: AbortSignal.timeout(45000) });
      nativeProbe = { sharedStatus: nativeTask.status, sharedMode: nativeTask.streaming_mode, nativeAudioStatus: nativeAudio.status };
      await nativeAudio.body?.cancel();
      if (nativeAudio.status === 200) nativeProbe.statusAfterNativeAudio = (await getTask(task.id)).status;
    }
    const { AlacWorkletPlayer } = await import("/src/core/player/alac-worklet.js");
    let firstBytesMs = 0;
    let firstPcmMs = 0;
    let readBytes = 0;
    let timeMs = 0;
    let rms = 0;
    let completeBytes = 0;
    let error = null;
    const renderStates = [];
    const player = new AlacWorkletPlayer({
      onState: (state) => {
        if (renderStates.at(-1)?.state !== state) renderStates.push({ state, ms: elapsed(), timeMs });
      },
      onProgress: (bytes) => { readBytes = bytes; if (!firstBytesMs) firstBytesMs = elapsed(); },
      onTime: (ms) => { timeMs = ms; if (!firstPcmMs && ms > 0) firstPcmMs = elapsed(); },
      onError: (value) => { error = String(value); },
      onStreamComplete: ({ blob }) => { completeBytes = blob.size; },
    });
    let meter;
    let output;
    try {
      const metadata = await player.load(task.streaming_url, { signal: AbortSignal.timeout(120000) });
      const readyMs = elapsed();
      const analyser = player.context.createAnalyser();
      analyser.fftSize = 2048;
      player.gain.connect(analyser);
      const pcm = new Float32Array(analyser.fftSize);
      meter = setInterval(() => {
        analyser.getFloatTimeDomainData(pcm);
        rms = Math.max(rms, Math.sqrt(pcm.reduce((sum, value) => sum + value * value, 0) / pcm.length));
      }, 20);
      await player.play();
      const playStartedMs = elapsed();
      while (timeMs < stopAfterMs && !error) {
        if (elapsed() > 120000) throw new Error("AudioWorklet playback timed out");
        await sleep(30);
      }
      output = { trackId, quality, phases, nativeProbe, metadata, firstBytesMs, readyMs, playStartedMs, firstPcmMs, readBytes, timeMs, rms, completeBytes, renderStates, error };
    } finally {
      clearInterval(meter);
      await player.destroy();
    }
    // Stop the large stream before the control request, particularly when an
    // SSH tunnel shares a slow TCP connection with audio bytes.
    const finalTask = await getTask(task.id);
    return { ...output, lastStatus: finalTask.status, lastMode: finalTask.streaming_mode };
  }, { trackId, quality, stopAfterMs, probeNative });
  console.log(JSON.stringify(redact({ browser: await browser.version(), ...result }), null, 2));
  assert(!result.fallback, "ALAC task used completed-file fallback; the packet stream was not exercised");
  assert.equal(result.error, null);
  assert(result.rms > 0.00001, "Audio render graph was silent");
  assert(result.timeMs >= stopAfterMs, "Playback did not advance");
  if (result.nativeProbe?.sharedStatus === "streaming") {
    assert.equal(result.nativeProbe.sharedMode, null, "Native create response leaked WASM capability");
    // A fast producer may complete between the two requests; 200 is valid
    // only after completion. Cold acceptance runs should observe 409 here.
    assert([409, 200].includes(result.nativeProbe.nativeAudioStatus));
    if (result.nativeProbe.nativeAudioStatus === 200) assert.equal(result.nativeProbe.statusAfterNativeAudio, "completed");
  }
} catch (error) {
  console.error(redact(String(error)));
  process.exitCode = 1;
} finally {
  await browser?.close();
  await server.close();
}
