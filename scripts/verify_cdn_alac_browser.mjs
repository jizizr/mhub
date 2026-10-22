/**
 * Real Edge/WebKit dual-origin ALAC transport, PCM rendering and IndexedDB test.
 * Uses a generated tone, the existing packet example binary and real frontend
 * modules. Does not contact production, download music or rebuild Rust.
 *
 * PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs BROWSER=both \
 *   node scripts/verify_cdn_alac_browser.mjs
 */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { access, mkdtemp, readFile, rm } from "node:fs/promises";
import http from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const { createServer } = await import(pathToFileURL(path.join(root, "frontend/node_modules/vite/dist/node/index.js")));
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const engines = process.env.BROWSER && process.env.BROWSER !== "both" ? [process.env.BROWSER] : ["chromium", "webkit"];
const directory = await mkdtemp(path.join(tmpdir(), "music-cdn-alac-"));
const revision = "a".repeat(32);
const cases = new Map();
const packetPath = /^\/api\/v1\/playbacks\/([^/]+)\/audio\/stream$/;
let app;
let cdn;

try {
  const converter = process.env.ALAC_PACKET_CONVERTER || path.join(root, "target/debug/examples/alac_packet_stream");
  await access(converter);
  const input = path.join(directory, "tone.m4a"), output = path.join(directory, "tone.alacstream");
  execFileSync(process.env.FFMPEG || "ffmpeg", ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
    "aevalsrc=0.15*sin(2*PI*440*t)|0.12*sin(2*PI*660*t):s=96000:d=6", "-sample_fmt", "s32p", "-c:a", "alac", input, "-y"], { stdio: "pipe" });
  execFileSync(converter, [input, output], { stdio: "pipe" });
  const bytes = await readFile(output);
  const hash = createHash("sha256").update(bytes).digest("hex");
  let prefix = 24 + bytes.readUInt16BE(18), frames = 0;
  while (frames < 96000 * 0.8) { frames += bytes.readUInt32BE(prefix + 4); prefix += 8 + bytes.readUInt32BE(prefix); }
  prefix += 7; // Deliberately split packet framing at the eventual Range boundary.
  const common = {
    "content-type": "application/x-splayer-alac", "access-control-allow-origin": "*",
    "access-control-expose-headers": "Content-Type, Content-Length, Content-Range, Content-Encoding, Accept-Ranges, X-Audio-Stream-Id",
    "x-audio-stream-id": "synthetic-same-open-packet-file",
  };
  const findCase = (request) => {
    const url = new URL(request.url, "http://localhost"), match = packetPath.exec(url.pathname);
    return match && url.searchParams.get("rev") === revision ? cases.get(match[1]) : null;
  };
  cdn = http.createServer((request, response) => {
    if (request.method === "OPTIONS") {
      response.writeHead(204, { "access-control-allow-origin": "*", "access-control-allow-methods": "GET, HEAD, OPTIONS", "access-control-allow-headers": "Range", "access-control-max-age": "600" }).end(); return;
    }
    const state = findCase(request);
    if (!state) { response.writeHead(404, { "cache-control": "no-store" }).end(); return; }
    const match = /^bytes=(\d+)-$/.exec(request.headers.range || "");
    state.requests.push({ origin: "cdn", range: request.headers.range || null, cookie: request.headers.cookie || null });
    if (!match) {
      response.writeHead(200, { ...common, "cache-control": "public, max-age=86400, no-transform", "accept-ranges": "bytes", "content-length": String(bytes.length) }).end(bytes); return;
    }
    const offset = Number(match[1]);
    state.rangeOffset = offset;
    // Old delivery continues during this handshake. The candidate must skip
    // the overlap itself; all assertions compare the final Blob byte-for-byte.
    const timer = setTimeout(() => {
      response.writeHead(206, { ...common, "cache-control": "public, max-age=86400, no-transform", "accept-ranges": "bytes",
        "content-range": `bytes ${offset}-${bytes.length - 1}/${bytes.length}`, "content-length": String(bytes.length - offset),
        "x-audio-stream-id": state.mode === "wrong-identity" ? "different-open-packet-file" : common["x-audio-stream-id"],
      }).end(bytes.subarray(offset));
    }, 220);
    response.on("close", () => clearTimeout(timer));
    state.advanceOriginal?.(8192);
    const next = setTimeout(() => state.advanceOriginal?.(8192), 100);
    if (state.mode === "wrong-identity") {
      const finish = setTimeout(() => state.finishOriginal?.(), 450);
      state.timers.push(finish);
    }
    state.timers.push(next);
  });
  await new Promise((resolve) => cdn.listen(0, "127.0.0.1", resolve));
  const mediaOrigin = `http://127.0.0.1:${cdn.address().port}`;
  app = await createServer({ root: path.join(root, "frontend"), server: { host: "127.0.0.1", port: 0 }, plugins: [{
    name: "cdn-alac-fixture",
    configureServer(vite) {
      vite.middlewares.use((request, response, next) => {
        if (request.url === "/__cdn_alac") {
          response.writeHead(200, { "content-type": "text/html", "cache-control": "no-store" }).end('<!doctype html><title>Dual-origin synthetic ALAC</title><button id="play">Play generated tone</button>'); return;
        }
        if (!packetPath.test(new URL(request.url, "http://localhost").pathname)) { next(); return; }
        const state = findCase(request);
        if (!state) { response.writeHead(404, { "cache-control": "no-store" }).end(); return; }
        state.requests.push({ origin: "api", range: request.headers.range || null, cookie: request.headers.cookie || null });
        response.writeHead(200, { ...common, "cache-control": "no-store, no-transform", "accept-ranges": "none" });
        let sent = 0, ended = false;
        const send = (end) => {
          if (response.destroyed || ended || end <= sent) return;
          response.write(bytes.subarray(sent, end)); sent = end; state.originalSent = sent;
        };
        const initial = setInterval(() => {
          send(Math.min(prefix, sent + 8192));
          if (sent >= prefix) clearInterval(initial);
        }, 1);
        state.advanceOriginal = (count) => send(Math.min(bytes.length, sent + count));
        state.finishOriginal = () => { send(bytes.length); ended = true; response.end(); };
        response.on("close", () => { clearInterval(initial); state.originalCancelled = !ended; });
      });
    },
  }] });
  await app.listen();
  const origin = `http://127.0.0.1:${app.httpServer.address().port}`;
  for (const engine of engines) {
    assert.ok(["chromium", "webkit"].includes(engine));
    let executablePath = engine === "chromium" ? process.env.BROWSER_EXECUTABLE : undefined;
    if (!executablePath && engine === "chromium" && process.platform === "darwin") {
      const edge = "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge";
      try { await access(edge); executablePath = edge; } catch { /* Use Playwright's configured Chromium elsewhere. */ }
    }
    const browser = engine === "webkit" ? null : await playwright[engine].launch({ headless: true, ...(executablePath ? { executablePath } : {}) });
    try {
      for (const mode of ["handoff", "wrong-identity", "completed"]) {
        const id = `applemusic--synthetic-${engine}-${mode}--hires`;
        const state = { mode, requests: [], timers: [], originalSent: 0, originalCancelled: false };
        cases.set(id, state);
        const sourcePath = `/api/v1/playbacks/${id}/audio/stream?v=${id}&rev=${revision}`;
        // Playwright's ephemeral macOS WebKit profiles cannot persist Blob
        // records. A disposable persistent profile exercises normal IDB.
        const context = engine === "webkit"
          ? await playwright.webkit.launchPersistentContext(path.join(directory, `webkit-${mode}`), { headless: true, serviceWorkers: "block" })
          : await browser.newContext({ serviceWorkers: "block" });
        try {
          const page = await context.newPage(), errors = [];
          page.on("pageerror", (error) => errors.push(error.message));
          await page.goto(`${origin}/__cdn_alac`);
          await page.evaluate(async ({ sourcePath, origin, mediaOrigin, id, mode }) => {
            const { AlacWorkletPlayer } = await import("/src/core/player/alac-worklet.js");
            const cache = await import("/src/services/localCache.js");
            const record = { mode, state: "idle", position: 0, completed: 0, requests: [], errors: [] };
            const nativeFetch = window.fetch.bind(window);
            window.fetch = async (url, options) => {
              if (String(url).includes("/api/v1/playbacks/")) record.requests.push({ url: String(url), cache: options?.cache, credentials: options?.credentials, range: options?.headers?.Range });
              return nativeFetch(url, options);
            };
            const player = new AlacWorkletPlayer({ onTime: (time) => { record.position = time; }, onState: (state) => { record.state = state; },
              onStreamComplete: () => { record.completed += 1; }, onError: (error) => record.errors.push(error.message) });
            const track = { id, platform: "applemusic", track_id: id, quality: "hires", streaming_mode: "wasm_alac", status: "completed" };
            let notify, analyser;
            window.fixture = { player, record, complete: () => notify?.(), snapshot: () => {
              const samples = new Float32Array(analyser?.fftSize || 0); analyser?.getFloatTimeDomainData(samples);
              const rms = samples.length ? Math.sqrt(samples.reduce((sum, sample) => sum + sample * sample, 0) / samples.length) : 0;
              return { ...record, eof: player.eof, bytes: player.readBytes, queuedFrames: player.queuedFrames, rms };
            }, async verifyAndCache() {
              const digest = async (blob) => [...new Uint8Array(await crypto.subtle.digest("SHA-256", await blob.arrayBuffer()))].map((b) => b.toString(16).padStart(2, "0")).join("");
              const hash = await digest(player.completeBlob), bytes = player.readBytes;
              const firstKey = cache.cacheKey({ ...track, streaming_url: origin + sourcePath });
              const cdnKey = cache.cacheKey({ ...track, streaming_url: mediaOrigin + sourcePath });
              const nativeKey = cache.cacheKey({ ...track, streaming_mode: null });
              await cache.cacheAudioBlob(firstKey, origin + sourcePath, track, track, { blob: player.completeBlob });
              const requestsBeforeCacheHit = record.requests.length;
              const cached = await cache.cacheAudioBlob(cdnKey, mediaOrigin + sourcePath, track, track);
              const local = URL.createObjectURL(cached.blob);
              try { await player.load(local, { completed: true }); await player.play(); }
              finally { URL.revokeObjectURL(local); }
              return { hash, cachedHash: await digest(cached.blob), bytes, firstKey, cdnKey, nativeKey,
                requestsBeforeCacheHit, requestsAfterCacheHit: record.requests.length };
            } };
            document.querySelector("#play").onclick = () => {
              player.prepare(96000);
              void player.load((mode === "completed" ? mediaOrigin : origin) + sourcePath, {
                completed: mode === "completed", trustedMediaOrigins: [mediaOrigin],
                waitForCompletedSource: () => new Promise((resolve) => { notify = () => resolve(mediaOrigin + sourcePath); }),
              }).then(async () => {
                analyser = player.context.createAnalyser(); analyser.fftSize = 2048; player.gain.connect(analyser);
                await player.play();
              }).catch((error) => record.errors.push(error.message));
            };
          }, { sourcePath, origin, mediaOrigin, id, mode });
          await page.click("#play");
          const waitFor = async (check) => {
            const end = Date.now() + 15000;
            while (Date.now() < end) {
              const snapshot = await page.evaluate(() => fixture.snapshot());
              assert.deepEqual(snapshot.errors, []);
              if (check(snapshot)) return snapshot;
              await page.waitForTimeout(50);
            }
            throw new Error(`Browser state timeout: ${JSON.stringify(await page.evaluate(() => fixture.snapshot()))}`);
          };
          const playing = await waitFor((s) => s.state === "playing" && s.position > 100 && s.rms > 0.01);
          let handoffOffset;
          if (mode !== "completed") {
            // WebKit can retain the last network chunk until more arrives.
            // The committed reader count, never the server's sent count, is
            // the only correct Range offset under that buffering behavior.
            handoffOffset = await page.evaluate(() => { const offset = fixture.player.readBytes; fixture.complete(); return offset; });
          }
          const finished = await waitFor((s) => s.eof && s.position > 400 && s.rms > 0.01);
          const cached = await page.evaluate(() => fixture.verifyAndCache());
          await page.waitForFunction(() => fixture.player.consumedFrames > 2400, null, { timeout: 5000 });
          assert.equal(finished.completed, 1);
          assert.equal(finished.bytes, bytes.length);
          assert.equal(cached.hash, hash); assert.equal(cached.cachedHash, hash);
          assert.equal(cached.firstKey, cached.cdnKey); assert.notEqual(cached.firstKey, cached.nativeKey);
          assert.equal(cached.requestsBeforeCacheHit, cached.requestsAfterCacheHit);
          assert.equal(state.requests.filter((r) => r.origin === "cdn" && !r.range).length, mode === "completed" ? 1 : 0);
          assert.equal(state.requests.filter((r) => r.origin === "cdn" && r.range).length, mode === "completed" ? 0 : 1);
          assert.ok(state.requests.every((r) => r.cookie === null));
          assert.ok(finished.requests.every((r) => r.credentials === "omit"));
          if (mode !== "completed") {
            assert.equal(state.rangeOffset, handoffOffset);
            assert.equal(state.originalCancelled, mode === "handoff");
            assert.equal(finished.requests[0].cache, "no-store");
            assert.equal(finished.requests[1].cache, "default");
          } else assert.equal(finished.requests[0].cache, "default");
          assert.deepEqual(errors, []);
          console.log(JSON.stringify({ engine, mode, actualPcmRms: finished.rms, playbackProgressMs: finished.position - playing.position,
            bytes: cached.bytes, sha256: cached.hash, rangeOffset: state.rangeOffset ?? null, originalSent: state.originalSent,
            originalCancelled: state.originalCancelled, mediaRequests: state.requests.length, localReplayExtraRequests: cached.requestsAfterCacheHit - cached.requestsBeforeCacheHit,
            sameCacheKeyAcrossOrigins: cached.firstKey === cached.cdnKey, nativeRepresentationIsolated: cached.firstKey !== cached.nativeKey }));
        } finally {
          state.timers.forEach(clearTimeout);
          await context.close(); cases.delete(id);
        }
      }
    } finally { await browser?.close(); }
  }
} finally {
  for (const state of cases.values()) state.timers.forEach(clearTimeout);
  await app?.close();
  cdn?.closeAllConnections();
  if (cdn) await new Promise((resolve) => cdn.close(resolve));
  await rm(directory, { recursive: true, force: true });
}
