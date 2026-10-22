/**
 * Exercise a real mobile Vue App search/click/playback with an isolated profile.
 * Only recommendations are stubbed, so no account recommendations are read.
 * Search, playback creation/status, audio and codec requests remain real.
 * No autoplay-policy override, injected queue or direct player call is used.
 *
 * MUSIC_API_ORIGIN=http://127.0.0.1:8000 PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs \
 *   node scripts/verify_live_app_playback_browser.mjs
 * MUSIC_WEB_ORIGIN=http://127.0.0.1:8000 verifies a deployed App instead of Vite.
 * MUSIC_WEB_DIR=/path/to/web verifies a local production build through a same-origin
 * API proxy without publishing it. Completed audio must stay on the preview origin.
 * APPLE_MUSIC_TEST_IDS=635770202,1453925093 APPLE_MUSIC_TEST_QUERY='Hotel California Eagles'
 * ALLOW_WARM=1 permits a replay; otherwise a candidate must return HTTP 404 first.
 * EXPECT_HANDOFF=1 additionally requires a same-file 206 adopted for over 8 seconds.
 * OBSERVE_MS defaults to 60000 after first sound; TOTAL_TIMEOUT_MS defaults to 120000.
 * REQUIRE_HIRES=0 permits a completed catalog file with a lower actual sample rate.
 * MUSIC_MEDIA_ORIGIN verifies completed/Range packet requests use that CDN origin.
 */
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const apiOrigin = new URL(process.env.MUSIC_API_ORIGIN || process.env.MUSIC_WEB_ORIGIN || "http://127.0.0.1:8000");
const webOrigin = process.env.MUSIC_WEB_ORIGIN ? new URL(process.env.MUSIC_WEB_ORIGIN) : null;
const webDirectory = process.env.MUSIC_WEB_DIR ? path.resolve(process.env.MUSIC_WEB_DIR) : null;
assert(!(webDirectory && webOrigin), "Choose a local production build or deployed web origin");
const appMode = webDirectory ? "local-production-build" : webOrigin ? "deployed" : "local-vite";
for (const url of [apiOrigin, webOrigin].filter(Boolean)) {
  assert(["http:", "https:"].includes(url.protocol) && !url.username && !url.password, "Use an HTTP origin without embedded credentials");
}
const candidates = (process.env.APPLE_MUSIC_TEST_IDS || "635770202,1453925093").split(",").map(value => value.trim());
assert(candidates.length > 0 && candidates.every(value => /^\d{6,}$/.test(value)), "Use public Apple Music song IDs");
const query = process.env.APPLE_MUSIC_TEST_QUERY || "Hotel California Eagles";
const observeMs = Math.max(5000, Math.min(60000, Number(process.env.OBSERVE_MS) || 60000));
const timeoutMs = Math.max(observeMs + 10000, Math.min(180000, Number(process.env.TOTAL_TIMEOUT_MS) || 120000));
const engine = process.env.BROWSER || "webkit";
const requireHiRes = process.env.REQUIRE_HIRES !== "0";
let expectedMediaHost = process.env.MUSIC_MEDIA_ORIGIN ? new URL(process.env.MUSIC_MEDIA_ORIGIN).host : null;
assert(["webkit", "chromium"].includes(engine), "BROWSER must be webkit or chromium");
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
let server;
let browser;
let context;
let page;
let result;
const coldChecks = [];
const pageErrors = [];

function redact(value) {
  if (typeof value === "string") return value.replace(/(?:https?|skd):\/\/[^\s"'<>]+/gi, "[redacted URL]").replace(/eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g, "[redacted token]");
  if (Array.isArray(value)) return value.map(redact);
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, redact(item)]));
  return value;
}

try {
  let trackId;
  for (const id of candidates) {
    const response = await fetch(new URL(`/api/v1/playbacks/applemusic--${id}--hires`, apiOrigin), { signal: AbortSignal.timeout(20000) });
    coldChecks.push({ trackId: id, status: response.status });
    await response.body?.cancel();
    if (response.status === 404 || (response.ok && process.env.ALLOW_WARM === "1")) { trackId = id; break; }
    assert(response.ok, `Preflight for ${id} failed with HTTP ${response.status}; this is not an absent cache`);
  }
  assert(trackId, "All candidate tasks already exist; choose another public song or explicitly set ALLOW_WARM=1");
  let base = webOrigin?.origin;
  if (webDirectory) {
    const { preview } = await import(pathToFileURL(path.join(root, "frontend/node_modules/vite/dist/node/index.js")));
    const previewServer = await preview({ root: path.join(root, "frontend"), build: { outDir: webDirectory },
      preview: { host: "127.0.0.1", port: 0, proxy: { "/api": apiOrigin.origin, "/health": apiOrigin.origin } } });
    server = { close: () => new Promise(resolve => previewServer.httpServer.close(resolve)) };
    base = `http://127.0.0.1:${previewServer.httpServer.address().port}`;
    expectedMediaHost ||= new URL(base).host;
  } else if (!base) {
    const { createServer } = await import(pathToFileURL(path.join(root, "frontend/node_modules/vite/dist/node/index.js")));
    server = await createServer({ root: path.join(root, "frontend"), server: { host: "127.0.0.1", port: 0, proxy: { "/api": apiOrigin.origin, "/health": apiOrigin.origin } } });
    await server.listen();
    base = `http://127.0.0.1:${server.httpServer.address().port}`;
  }
  browser = await playwright[engine].launch({ headless: true, ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
  context = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 1, isMobile: true, hasTouch: true, serviceWorkers: "block" });
  page = await context.newPage();
  page.on("pageerror", error => pageErrors.push(error.message));
  await page.route("**/api/v1/recommendations**", route => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(route.request().url().includes("/home") ? { tracks: [], playlists: [], tags: [] } : []) }));
  await page.addInitScript(() => {
    localStorage.setItem("music-web-splayer-status-v3", JSON.stringify({ quality: "hires" }));
    const probe = window.__liveAppAudioProbe = {
      clickedAt: null, awaitingClick: false, firstSoundAt: null, maxRms: 0,
      contexts: [], requests: [], taskPhases: [], streams: [], states: [], samples: [], errors: [],
    };
    document.addEventListener("click", event => {
      if (probe.awaitingClick && event.target.closest(".song-row button")) {
        probe.clickedAt = performance.now();
        probe.awaitingClick = false;
      }
    }, true);

    const OriginalContext = window.AudioContext || window.webkitAudioContext;
    if (OriginalContext) {
      const WrappedContext = new Proxy(OriginalContext, {
        construct(Target, args) {
          const audio = Reflect.construct(Target, args);
          const info = { createdAt: performance.now(), sampleRate: audio.sampleRate, states: [{ at: performance.now(), state: audio.state }], maxRms: 0 };
          probe.contexts.push(info);
          audio.addEventListener("statechange", () => info.states.push({ at: performance.now(), state: audio.state }));
          const createGain = audio.createGain.bind(audio);
          audio.createGain = (...gainArgs) => {
            const gain = createGain(...gainArgs);
            const connect = gain.connect.bind(gain);
            let metered = false;
            gain.connect = (destination, ...connectArgs) => {
              const connected = connect(destination, ...connectArgs);
              if (destination === audio.destination && !metered) {
                metered = true;
                const analyser = audio.createAnalyser();
                analyser.fftSize = 2048;
                const silent = createGain();
                silent.gain.value = 0;
                connect(analyser);
                analyser.connect(silent).connect(audio.destination);
                const data = new Float32Array(analyser.fftSize);
                const timer = setInterval(() => {
                  if (audio.state === "closed") { clearInterval(timer); return; }
                  analyser.getFloatTimeDomainData(data);
                  const rms = Math.sqrt(data.reduce((sum, value) => sum + value * value, 0) / data.length);
                  info.maxRms = Math.max(info.maxRms, rms);
                  probe.maxRms = Math.max(probe.maxRms, rms);
                  if (probe.clickedAt !== null && probe.firstSoundAt === null && rms > 1e-7) probe.firstSoundAt = performance.now();
                }, 20);
              }
              return connected;
            };
            return gain;
          };
          return audio;
        },
      });
      if (window.AudioContext) window.AudioContext = WrappedContext;
      if (window.webkitAudioContext) window.webkitAudioContext = WrappedContext;
    }

    // Observe the application's existing read() calls. This does not tee the
    // large response, preconsume it, or send a second stream/native request.
    const originalFetch = window.fetch.bind(window);
    window.fetch = async (...args) => {
      const url = new URL(typeof args[0] === "string" ? args[0] : args[0]?.url || String(args[0]), location.href);
      const method = args[1]?.method || (args[0] instanceof Request ? args[0].method : "GET");
      const requestHeaders = new Headers(args[1]?.headers || (args[0] instanceof Request ? args[0].headers : undefined));
      const request = { host: url.host, path: url.pathname, method, at: performance.now(), range: requestHeaders.get("range"), cache: args[1]?.cache, credentials: args[1]?.credentials };
      if (url.pathname.endsWith("/audio/stream")) request.bytesObservedBeforeRequest = probe.streams.reduce((sum, stream) => sum + stream.bytes, 0);
      const playback = url.pathname.startsWith("/api/v1/playbacks");
      if (playback) probe.requests.push(request);
      let response;
      try { response = await originalFetch(...args); }
      catch (error) { if (playback) { request.error = error.message; request.errorAt = performance.now(); } throw error; }
      request.responseAt = performance.now();
      request.status = response.status;
      if (playback && !/\/audio(?:\/stream)?$/.test(url.pathname) && !/\/(lyrics|cover)$/.test(url.pathname)) {
        void response.clone().json().then(payload => {
          const task = payload.playback || payload;
          if (task.id && task.status) {
            const last = probe.taskPhases.at(-1);
            if (!last || last.status !== task.status || last.mode !== task.streaming_mode) probe.taskPhases.push({ at: request.responseAt, id: task.id, status: task.status, mode: task.streaming_mode });
          }
        }).catch(() => {});
      }
      if (url.pathname.endsWith("/audio/stream") && response.body) {
        const responseHeaders = Object.fromEntries(["content-length", "content-range", "content-type", "cache-control", "accept-ranges", "content-encoding", "server", "transfer-encoding", "x-accel-buffering", "x-audio-stream-id"].map(key => [key, response.headers.get(key)]));
        const stream = { host: url.host, requestAt: request.at, responseAt: request.responseAt, requestRange: request.range, bytesObservedBeforeRequest: request.bytesObservedBeforeRequest, status: response.status, responseHeaders, bytes: 0, firstByteAt: null, lastByteAt: null, maxGapMs: 0, header: null, headerChecked: false, eofAt: null, cancelAt: null, bodyCancelAt: null, readErrorAt: null, bytesPerSecond: [] };
        probe.streams.push(stream);
        const body = response.body;
        const cancelBody = body.cancel.bind(body);
        body.cancel = (...cancelArgs) => { stream.bodyCancelAt = performance.now(); return cancelBody(...cancelArgs); };
        const getReader = body.getReader.bind(body);
        body.getReader = (...readerArgs) => {
          const reader = getReader(...readerArgs);
          const read = reader.read.bind(reader);
          const cancel = reader.cancel.bind(reader);
          reader.cancel = (...cancelArgs) => { stream.cancelAt = performance.now(); return cancel(...cancelArgs); };
          let prefix = new Uint8Array(0);
          reader.read = async (...readArgs) => {
            let chunk;
            try { chunk = await read(...readArgs); }
            catch (error) { stream.readErrorAt = performance.now(); stream.readError = error.message; throw error; }
            const now = performance.now();
            if (chunk.done) { stream.eofAt = now; return chunk; }
            if (!chunk.value?.byteLength) return chunk;
            if (stream.firstByteAt === null) stream.firstByteAt = now;
            if (stream.lastByteAt !== null) stream.maxGapMs = Math.max(stream.maxGapMs, now - stream.lastByteAt);
            stream.lastByteAt = now;
            stream.bytes += chunk.value.byteLength;
            const second = Math.floor((now - stream.firstByteAt) / 1000);
            stream.bytesPerSecond[second] = (stream.bytesPerSecond[second] || 0) + chunk.value.byteLength;
            if (!stream.headerChecked) {
              const needed = Math.max(0, 24 - prefix.byteLength);
              const joined = new Uint8Array(prefix.byteLength + Math.min(needed, chunk.value.byteLength));
              joined.set(prefix);
              joined.set(chunk.value.subarray(0, needed), prefix.byteLength);
              prefix = joined;
              if (prefix.byteLength >= 24) {
                stream.headerChecked = true;
                const magic = new TextDecoder().decode(prefix.subarray(0, 8));
                // A resumed 206 begins at an arbitrary packet byte, not at
                // the file header. Keep that segment's metadata null.
                if (magic === "SPLALAC1") {
                  const view = new DataView(prefix.buffer);
                  stream.header = { magic, version: view.getUint32(8), sampleRate: view.getUint32(12), channels: prefix[16], bits: prefix[17] };
                }
              }
            }
            return chunk;
          };
          return reader;
        };
      }
      return response;
    };
    let lastSample = 0;
    setInterval(() => {
      const provides = document.querySelector("[data-v-app]")?.__vue_app__?._context?.provides;
      const pinia = provides && Reflect.ownKeys(provides).map(key => provides[key]).find(value => value?._s?.get);
      const status = pinia?._s.get("status");
      if (!status || probe.clickedAt === null) return;
      const now = performance.now();
      const sample = { at: now, state: status.state, positionMs: Math.round(status.position), loading: status.trackLoading, notice: status.notice };
      const last = probe.states.at(-1);
      if (!last || last.state !== sample.state || last.loading !== sample.loading) probe.states.push(sample);
      if (now - lastSample >= 1000) { probe.samples.push(sample); lastSample = now; }
      probe.current = sample;
    }, 100);
  });
  await page.goto(base, { waitUntil: "domcontentloaded", timeout: 45000 });
  const search = page.getByPlaceholder("搜索歌曲、艺人或粘贴音乐链接");
  await search.fill(query);
  const searchResponse = page.waitForResponse(response => {
    const url = new URL(response.url());
    return url.pathname.endsWith("/search") && url.searchParams.get("q") === query;
  }, { timeout: 45000 });
  await search.press("Enter");
  const payload = await (await searchResponse).json();
  const found = Array.isArray(payload) ? payload : payload.items || payload.tracks || [];
  const rowIndex = found.findIndex(track => String(track.id || track.source_id) === trackId);
  assert(rowIndex >= 0, `Public target ${trackId} was not present in the real search results; no direct player fallback was used`);
  const row = page.locator(".song-list__rows .song-row").nth(rowIndex);
  await row.waitFor({ timeout: 45000 });
  assert(await page.evaluate(() => {
    const provides = document.querySelector("[data-v-app]")?.__vue_app__?._context?.provides;
    const pinia = provides && Reflect.ownKeys(provides).map(key => provides[key]).find(value => value?._s?.get);
    return pinia?._s.get("status")?.quality === "hires";
  }), "The real App status store was not observable; stop before consuming the cold candidate");
  const selected = { id: trackId, title: await row.locator(".song-row__title").innerText(), artist: await row.locator(".song-row__artist").innerText() };
  await page.evaluate(() => { window.__liveAppAudioProbe.awaitingClick = true; });
  await row.getByTitle("播放", { exact: true }).click();
  let stopReason = "timeout";
  const wallStarted = Date.now();
  while (Date.now() - wallStarted < timeoutMs) {
    const snapshot = await page.evaluate(() => {
      const p = window.__liveAppAudioProbe;
      return { now: performance.now(), firstSoundAt: p.firstSoundAt, state: p.current?.state, sourceHeader: p.streams.find(stream => stream.header)?.header || null };
    });
    if (requireHiRes && snapshot.sourceHeader && snapshot.sourceHeader.sampleRate <= 48000) { stopReason = "source-not-hires"; break; }
    if (snapshot.firstSoundAt !== null && snapshot.now - snapshot.firstSoundAt >= observeMs) { stopReason = "observation-complete"; break; }
    if (snapshot.state === "error") { stopReason = "playback-error"; break; }
    await page.waitForTimeout(500);
  }
  const probe = await page.evaluate(() => {
    const p = window.__liveAppAudioProbe;
    const relative = at => at === null || at === undefined ? null : Math.round(at - p.clickedAt);
    const relativeObject = object => Object.fromEntries(Object.entries(object).map(([key, value]) => [key, key === "at" || key.endsWith("At") ? relative(value) : value]));
    return {
      assets: [...document.scripts].map(script => script.src && new URL(script.src).pathname).filter(Boolean),
      navigationProtocol: performance.getEntriesByType("navigation")[0]?.nextHopProtocol || null,
      clickObserved: p.clickedAt !== null, firstSoundMs: relative(p.firstSoundAt), elapsedMs: relative(performance.now()), maxRms: p.maxRms,
      current: p.current ? relativeObject(p.current) : null,
      contexts: p.contexts.map(item => ({ ...relativeObject(item), states: item.states.map(relativeObject) })),
      requests: p.requests.map(relativeObject), taskPhases: p.taskPhases.map(relativeObject),
      streams: p.streams.map(item => ({ ...relativeObject(item), maxGapMs: Math.round(item.maxGapMs) })),
      states: p.states.map(relativeObject), samples: p.samples.map(relativeObject),
    };
  });
  const expectedPlayed = Math.max(0, probe.elapsedMs - (probe.firstSoundMs ?? probe.elapsedMs));
  const header = probe.streams.find(stream => stream.header)?.header;
  const observedBufferingTransitions = probe.states.filter(state => state.state === "buffering").length;
  const observedRebuffersAfterFirstSound = probe.firstSoundMs === null ? 0 : probe.states.filter(state => state.state === "buffering" && state.at > probe.firstSoundMs).length;
  const observedAdoptions = probe.streams.flatMap((stream, index) => {
    if (stream.status !== 206 || !stream.requestRange || stream.firstByteAt === null || stream.lastByteAt - stream.firstByteAt < 8000 || stream.cancelAt !== null || stream.bodyCancelAt !== null) return [];
    const original = probe.streams.slice(0, index).find(previous => previous.status === 200 && previous.cancelAt !== null);
    if (!original) return [];
    return [{ requestRange: stream.requestRange, originalCancelAt: original.cancelAt, responseAt: stream.responseAt, firstByteAt: stream.firstByteAt, lastByteAt: stream.lastByteAt, bytes: stream.bytes, sameIdentity: Boolean(original.responseHeaders["x-audio-stream-id"] && original.responseHeaders["x-audio-stream-id"] === stream.responseHeaders["x-audio-stream-id"]) }];
  });
  const positionRegressions = probe.samples.flatMap((sample, index) => index && sample.positionMs + 100 < probe.samples[index - 1].positionMs ? [{ at: sample.at, fromMs: probe.samples[index - 1].positionMs, toMs: sample.positionMs }] : []);
  const handoffVerified = observedAdoptions.some(adoption => adoption.sameIdentity);
  const completedStreams = probe.streams.filter(stream => stream.status === 206 || Number(stream.responseHeaders["content-length"]) > 0);
  const mediaHostVerified = !expectedMediaHost || (completedStreams.length > 0 && completedStreams.every(stream => stream.host === expectedMediaHost));
  const passed = stopReason === "observation-complete" && probe.firstSoundMs !== null && probe.maxRms > 1e-7 && header?.magic === "SPLALAC1" && (!requireHiRes || header.sampleRate > 48000) && (probe.current?.positionMs || 0) >= expectedPlayed * 0.95 && pageErrors.length === 0 && positionRegressions.length === 0 && observedAdoptions.every(adoption => adoption.sameIdentity) && (process.env.EXPECT_HANDOFF !== "1" || handoffVerified) && mediaHostVerified;
  result = { passed, engine, viewport: "390x844", app: appMode, coldChecks, selected, observeMs, timeoutMs, stopReason, expectedMediaHost, mediaHostVerified, expectedPlayedMs: expectedPlayed, observedBufferingTransitions, observedRebuffersAfterFirstSound, handoffVerified, observedAdoptions, positionRegressions, pageErrors, ...probe };
  if (!passed) process.exitCode = 1;
} catch (error) {
  result = { passed: false, engine, app: appMode, coldChecks, pageErrors, error: error.message };
  process.exitCode = 1;
} finally {
  if (result) console.log(JSON.stringify(redact(result), null, 2));
  await context?.close();
  await browser?.close();
  await server?.close();
}
