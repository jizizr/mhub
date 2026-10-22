/**
 * Cold Hi-Res status polling against the actual built App and real ALAC audio.
 * Only public synthetic audio/API fixtures are used. MUSIC_WEB_ORIGIN serves
 * deployed static assets through the local fixture server; its API is never
 * contacted. EXPECT_REDUNDANT=1 reproduces the former third status request.
 *
 * PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs BROWSER=both node scripts/verify_playback_status_browser.mjs
 */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs/promises";
import http from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const web = path.resolve(process.env.MUSIC_WEB_DIR || path.join(root, "web"));
const staticOrigin = process.env.MUSIC_WEB_ORIGIN || null;
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const engines = process.env.BROWSER && process.env.BROWSER !== "both" ? [process.env.BROWSER] : ["chromium", "webkit"];
const before = process.env.EXPECT_REDUNDANT === "1";
const directory = await fs.mkdtemp(path.join(tmpdir(), "playback-status-"));
const id = "applemusic--synthetic-status-fixture--hires";
const track = { id, source_id: "synthetic-status-fixture", selected_id: "synthetic-status-fixture", platform: "applemusic", title: "Synthetic status fixture", artist: "Public test signal", query: "Synthetic status fixture", quality: "hires", duration: 30000 };
const reports = [], staticAssets = new Map();
let server, fixture;

try {
  const command = (binary, args) => execFileSync(binary, args, { cwd: root, stdio: "pipe" });
  command("cargo", ["build", "--quiet", "--example", "alac_packet_stream"]);
  command("ffmpeg", ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "aevalsrc=0.08*sin(2*PI*440*t)|0.06*sin(2*PI*660*t):s=96000:d=30", "-sample_fmt", "s32p", "-c:a", "alac", path.join(directory, "fixture.m4a"), "-y"]);
  command(path.join(root, "target/debug/examples/alac_packet_stream"), [path.join(directory, "fixture.m4a"), path.join(directory, "fixture.alacstream")]);
  const audio = await fs.readFile(path.join(directory, "fixture.alacstream"));
  const audioHash = createHash("sha256").update(audio).digest("hex");
  let prefixLength = 24 + audio.readUInt16BE(18);
  for (let packet = 0; packet < 20; packet++) prefixLength += 8 + audio.readUInt32BE(prefixLength);
  assert.ok(prefixLength < audio.length, "fixture has a progressive prefix and later audio");
  const task = status => ({ ...track, track_id: track.source_id, status, format: "m4a", content_type: "application/x-splayer-alac", duration: 30, streaming_mode: "wasm_alac", streaming_codec: "alac", streaming_protocol: "splayer-alac-v2", streaming_url: `/api/v1/playbacks/${id}/audio/stream`, audio_url: `/api/v1/playbacks/${id}/audio` });

  if (staticOrigin) {
    // Keep this controlled song-transfer test independent of WAN latency when
    // obtaining the deployed decoder. These are the exact published assets,
    // cached only in the local fixture proxy before the browser clicks Play.
    const runtimeFiles = (await fs.readdir(path.join(web, "assets")))
      .filter(name => name.endsWith(".wasm") || name.startsWith("alac-processor-"));
    await Promise.all(runtimeFiles.map(async name => {
      const assetPath = `/assets/${name}`;
      const remote = await fetch(new URL(assetPath, staticOrigin));
      assert.ok(remote.ok, `static runtime ${assetPath}: HTTP ${remote.status}`);
      staticAssets.set(assetPath, { bytes: Buffer.from(await remote.arrayBuffer()), type: remote.headers.get("content-type") || "application/octet-stream" });
    }));
  }

  server = http.createServer(async (request, response) => {
    try {
      const url = new URL(request.url, "http://localhost");
      const json = (body, status = 200) => response.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" }).end(JSON.stringify(body));
      if (url.pathname === "/health") return json({ status: "ok" });
      if (url.pathname.startsWith("/api/")) {
        fixture.requests.push({ method: request.method, path: url.pathname, elapsed: Date.now() - fixture.started });
        if (url.pathname === "/api/v1/platforms") return json([{ id: "applemusic", label: "Apple Music" }]);
        if (url.pathname === "/api/v1/recommendations/home") return json({ tracks: [], playlists: [], tags: [] });
        if (url.pathname === "/api/v1/recommendations" || url.pathname === "/api/v1/search") return json([]);
        if (url.pathname.endsWith("/lyrics")) return json({ available: true, plain: "[00:00.00]Synthetic public lyric", translation: "" });
        if (url.pathname === "/api/v1/playbacks" && request.method === "POST") {
          let body = ""; for await (const chunk of request) body += chunk;
          fixture.creates.push(JSON.parse(body));
          return json({ playback: task("queued") });
        }
        if (url.pathname === `/api/v1/playbacks/${id}` && request.method === "GET") {
          fixture.polls += 1;
          return json(task(fixture.polls === 1 ? "queued" : fixture.eofAt ? "completed" : "streaming"));
        }
        if (url.pathname === `/api/v1/playbacks/${id}/audio/stream`) {
          fixture.streams += 1;
          fixture.streamAt = Date.now();
          // No Content-Length/Accept-Ranges: this must exercise the growing
          // transport completion observer, not the already completed path.
          response.writeHead(200, { "content-type": "application/x-splayer-alac", "cache-control": "no-store", "x-audio-stream-id": "synthetic-status-fixture-v1" });
          response.write(audio.subarray(0, prefixLength));
          const state = fixture;
          const timer = setTimeout(() => { state.eofAt = Date.now(); response.end(audio.subarray(prefixLength)); }, 400);
          response.on("close", () => clearTimeout(timer));
          return;
        }
        fixture.unexpected.push(`${request.method} ${url.pathname}`);
        return json({ detail: "Unstubbed synthetic API" }, 500);
      }
      if (url.pathname === "/favicon.ico") return response.writeHead(204).end();
      const assetPath = url.pathname === "/" ? "/index.html" : url.pathname;
      let asset = staticAssets.get(assetPath);
      if (!asset) {
        if (staticOrigin) {
          assert.ok(assetPath === "/index.html" || assetPath === "/manifest.webmanifest" || assetPath.startsWith("/assets/") || assetPath.startsWith("/icons/"), `only deployed static assets may be fetched: ${assetPath}`);
          const remote = await fetch(new URL(assetPath, staticOrigin));
          assert.ok(remote.ok, `static asset ${assetPath}: HTTP ${remote.status}`);
          asset = { bytes: Buffer.from(await remote.arrayBuffer()), type: remote.headers.get("content-type") || "application/octet-stream" };
        } else {
          const file = path.resolve(web, `.${decodeURIComponent(assetPath)}`);
          assert.ok(file.startsWith(`${web}/`), "static asset remains under build directory");
          asset = { bytes: await fs.readFile(file), type: { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".png": "image/png", ".svg": "image/svg+xml" }[path.extname(file)] || "application/octet-stream" };
        }
        staticAssets.set(assetPath, asset);
      }
      response.writeHead(200, { "content-type": asset.type, "cache-control": "no-store" }).end(asset.bytes);
    } catch (error) {
      fixture?.unexpected.push(error.message);
      if (!response.headersSent) response.writeHead(500);
      response.end("Synthetic fixture error");
    }
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;

  for (const engine of engines) {
    fixture = { started: Date.now(), requests: [], creates: [], polls: 0, streams: 0, eofAt: 0, unexpected: [] };
    let executablePath = process.env.BROWSER_EXECUTABLE;
    if (!executablePath && engine === "chromium" && process.platform === "darwin") {
      const edge = "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge";
      try { await fs.access(edge); executablePath = edge; } catch { /* use Playwright's installed browser */ }
    }
    // WebKit's ephemeral Playwright context cannot persist this Blob record
    // reliably. A temporary persistent profile exercises normal browser IDB.
    const launchOptions = { headless: true, ...(executablePath ? { executablePath } : {}) };
    let browser, context;
    if (engine === "webkit") {
      context = await playwright[engine].launchPersistentContext(path.join(directory, "webkit-profile"), { ...launchOptions, serviceWorkers: "block" });
      browser = context.browser();
    } else {
      browser = await playwright[engine].launch(launchOptions);
      context = await browser.newContext({ serviceWorkers: "block" });
    }
    const page = await context.newPage(), errors = [], external = [], results = [], consoleWarnings = [];
    const check = (name, condition, details) => { results.push({ name, passed: Boolean(condition), ...(!condition ? { details } : {}) }); assert.ok(condition, name); };
    let evidence, failure;
    page.on("pageerror", error => errors.push(error.message));
    page.on("console", message => { if (["warning", "error"].includes(message.type())) consoleWarnings.push(message.text()); });
    page.on("request", request => { const url = new URL(request.url()); if (url.origin !== origin && !["blob:", "data:"].includes(url.protocol)) external.push(url.origin + url.pathname); });
    await page.route("**/*", route => {
      const url = new URL(route.request().url());
      return url.origin === origin || ["blob:", "data:"].includes(url.protocol) ? route.continue() : route.abort();
    });
    await page.addInitScript(({ track }) => {
      localStorage.setItem("music-web-splayer-queue-v3", JSON.stringify({ queue: [track] }));
      localStorage.setItem("music-web-splayer-status-v3", JSON.stringify({ quality: "hires", volume: 0.25, activeView: "discover", playIndex: 0, repeatMode: "off", autoplayEnabled: false, soundCheckEnabled: false, crossfadeEnabled: false }));
      const outputs = [], seen = new WeakSet();
      const connect = AudioNode.prototype.connect;
      AudioNode.prototype.connect = function(destination, ...args) {
        const result = connect.call(this, destination, ...args);
        if (destination === this.context.destination && !seen.has(this)) {
          seen.add(this);
          const analyser = this.context.createAnalyser(); analyser.fftSize = 2048;
          const zero = this.context.createGain(); zero.gain.value = 0;
          connect.call(this, analyser); connect.call(analyser, zero); connect.call(zero, destination);
          outputs.push({ context: this.context, analyser, maxRms: 0 });
        }
        return result;
      };
      window.statusFixture = { outputs };
      setInterval(() => {
        for (const output of outputs) {
          const values = new Float32Array(output.analyser.fftSize);
          output.analyser.getFloatTimeDomainData(values);
          const rms = Math.sqrt(values.reduce((sum, value) => sum + value * value, 0) / values.length);
          output.maxRms = Math.max(output.maxRms, rms);
        }
      }, 30);
    }, { track });
    try {
      await page.goto(origin, { waitUntil: "domcontentloaded" });
      await page.locator(".player-bar .player-controls button[title='播放']").click();
      await page.waitForFunction(() => Number(document.querySelector(".player-bar .seek-slider")?.value) > 500 && window.statusFixture.outputs.some(output => output.maxRms > .001), null, { timeout: 20000 });
      const started = await page.evaluate(() => ({ position: Number(document.querySelector(".player-bar .seek-slider").value), maxRms: Math.max(...statusFixture.outputs.map(output => output.maxRms)) }));
      // Wait beyond the completion observer's interval: EOF must have cancelled
      // it, while playback and full original-byte caching continue normally.
      await page.waitForTimeout(1300);
      // Read and hash the same persisted record before closing its connection.
      // Poll from Node so the asserted evidence is exactly the completed sample.
      const cacheDeadline = Date.now() + 10000;
      do {
        evidence = await page.evaluate(async () => {
          const db = await new Promise((resolve, reject) => {
            const open = indexedDB.open("music-splayer-web-cache-v1", 1);
            open.onerror = () => reject(open.error);
            open.onsuccess = () => resolve(open.result);
          });
          let cache;
          try {
            const records = await new Promise((resolve, reject) => {
              const request = db.transaction("tracks", "readonly").objectStore("tracks").getAll();
              request.onsuccess = () => resolve(request.result);
              request.onerror = () => reject(request.error);
            });
            cache = await Promise.all(records.map(async record => ({ key: record.key, bytes: record.blob?.size || 0, sha256: record.blob ? [...new Uint8Array(await crypto.subtle.digest("SHA-256", await record.blob.arrayBuffer()))].map(value => value.toString(16).padStart(2, "0")).join("") : null })));
          } finally { db.close(); }
          return { position: Number(document.querySelector(".player-bar .seek-slider").value), mediaState: navigator.mediaSession?.playbackState, native: [...document.querySelectorAll("audio")].map(audio => ({ paused: audio.paused, source: audio.currentSrc })), outputs: statusFixture.outputs.map(output => ({ state: output.context.state, sampleRate: output.context.sampleRate, time: output.context.currentTime, maxRms: output.maxRms })), cache };
        });
        if (evidence.cache.length === 1 && evidence.cache[0].bytes === audio.length) break;
        await page.waitForTimeout(100);
      } while (Date.now() < cacheDeadline);
      check("cold Hi-Res uses the real ALAC renderer and advances audibly", evidence.position > started.position + 600 && evidence.outputs.some(output => output.state === "running" && output.maxRms > .001) && evidence.native.every(audio => audio.paused), evidence);
      check("one preparation preserves selected Hi-Res and ALAC transport", fixture.creates.length === 1 && fixture.creates[0].quality === "hires" && fixture.creates[0].streaming_mode === "wasm_alac", fixture.creates);
      check(before ? "saved build reproduces redundant third status request" : "EOF cancels completion observation without a redundant third status request", fixture.polls === (before ? 3 : 2), fixture.requests);
      check("original stream downloads once without duplicate audio or range handoff", fixture.streams === 1 && Boolean(fixture.eofAt), fixture);
      check("fully cached ALAC bytes match the original synthetic transport", evidence.cache.length === 1 && evidence.cache[0].bytes === audio.length && evidence.cache[0].sha256 === audioHash, evidence.cache);
      check("fixture has no uncaught errors or external account/provider traffic", errors.length === 0 && external.length === 0 && fixture.unexpected.length === 0, { errors, external, unexpected: fixture.unexpected });
    } catch (error) { failure = error.stack || error.message; }
    finally { await context.close(); await browser.close(); }
    reports.push({ engine, version: browser.version(), results, ...(failure ? { failure } : {}), evidence, network: fixture, errors, external, consoleWarnings });
  }
  process.stdout.write(JSON.stringify({ before, staticSource: staticOrigin || web, staticAssets: [...staticAssets].map(([assetPath, asset]) => ({ path: assetPath, sha256: createHash("sha256").update(asset.bytes).digest("hex") })), fixture: { sampleRate: 96000, channels: 2, bits: 24, bytes: audio.length, sha256: audioHash }, reports }, null, 2) + "\n");
  if (reports.some(report => report.failure || report.results.some(result => !result.passed))) process.exitCode = 1;
} finally {
  if (server) { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
  await fs.rm(directory, { recursive: true, force: true });
}
