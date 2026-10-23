/**
 * Real browser regression for the Rust packet transport + WASM decoder +
 * AudioWorklet. Uses generated tones, no account or private audio data.
 *
 * Requires ffmpeg/ffprobe, cargo and Playwright. For a shared Playwright install:
 * PLAYWRIGHT_MODULE=/absolute/path/to/playwright/index.mjs \
 * BROWSER_EXECUTABLE=/absolute/path/to/chromium node scripts/verify_alac_browser.mjs
 */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const directory = await mkdtemp(path.join(tmpdir(), "splayer-alac-"));
const { createServer } = await import(pathToFileURL(path.join(root, "frontend/node_modules/vite/dist/node/index.js")));
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
let server;
let browser;
const controlledStreams = new Map();
const controlledRequests = new Map();
const run = (binary, args) => execFileSync(binary, args, { cwd: root, stdio: "pipe" });
const fixtures = [
  { name: "stereo16", rate: 44100, bits: 16, channels: 2 },
  { name: "stereo24", rate: 96000, bits: 24, channels: 2 },
  { name: "mono16", rate: 48000, bits: 16, channels: 1 },
];
try {
  run("cargo", ["build", "--quiet", "--example", "alac_packet_stream"]);
  for (const fixture of fixtures) {
    const prefix = path.join(directory, fixture.name);
    const tone = fixture.channels === 1 ? "0.3*sin(2*PI*440*t)" : "0.3*sin(2*PI*440*t)|0.2*sin(2*PI*880*t)";
    run("ffmpeg", ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", `aevalsrc=${tone}:s=${fixture.rate}:d=4.25`, "-sample_fmt", fixture.bits === 16 ? "s16p" : "s32p", "-c:a", "alac", `${prefix}.m4a`, "-y"]);
    run(path.join(root, "target/debug/examples/alac_packet_stream"), [`${prefix}.m4a`, `${prefix}.alacstream`]);
    run("ffmpeg", ["-hide_banner", "-loglevel", "error", "-i", `${prefix}.m4a`, "-f", "s32le", "-acodec", "pcm_s32le", `${prefix}.pcm`, "-y"]);
    fixture.bytes = (await readFile(`${prefix}.alacstream`)).length;
  }
  server = await createServer({
    root: path.join(root, "frontend"),
    server: { host: "127.0.0.1", port: 0 },
    plugins: [{
      name: "alac-offline-fixtures",
      configureServer(vite) {
        vite.middlewares.use(async (req, res, next) => {
          const url = new URL(req.url, "http://127.0.0.1");
          if (url.pathname === "/__alac_test") { res.setHeader("content-type", "text/html"); res.end("<!doctype html><title>ALAC synthetic playback regression</title>"); return; }
          if (url.pathname === "/__stream_release") {
            const stream = controlledStreams.get(url.searchParams.get("id"));
            if (!stream) { res.statusCode = 404; res.end(); return; }
            stream.advance(Number(url.searchParams.get("packets")), url.searchParams.has("finish"));
            res.end("ok"); return;
          }
          const match = /^\/__fixtures\/(stereo16|stereo24|mono16)\.(alacstream|pcm)$/.exec(url.pathname);
          if (!match) return next();
          const bytes = await readFile(path.join(directory, `${match[1]}.${match[2]}`));
          res.setHeader("content-type", "application/octet-stream");
          res.setHeader("cache-control", "no-store");
          if (url.searchParams.has("controlled")) {
            const id = url.searchParams.get("controlled");
            controlledRequests.set(id, (controlledRequests.get(id) || 0) + 1);
            const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
            const ends = [24 + view.getUint16(18)];
            while (ends.at(-1) < bytes.length) ends.push(ends.at(-1) + 8 + view.getUint32(ends.at(-1)));
            let sent = ends[20];
            res.setHeader("content-length", bytes.length);
            res.write(bytes.subarray(0, sent));
            controlledStreams.set(id, { advance(packetCount, finish) {
              const end = finish ? bytes.length : ends[Math.min(packetCount, ends.length - 1)];
              if (end > sent) { res.write(bytes.subarray(sent, end)); sent = end; }
              if (finish) res.end();
            } });
            res.on("close", () => controlledStreams.delete(id));
            return;
          }
          if (url.searchParams.has("fail")) {
            // Abrupt failure exactly at a complete packet boundary must not
            // become a valid full-track cache entry. Advertise this known
            // representation's size: WebKit may surface a socket close as EOF
            // rather than a reader error, so packet boundaries alone are not
            // enough to prove the complete representation was received.
            res.setHeader("content-length", bytes.length);
            const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
            let end = 24 + view.getUint16(18);
            for (let packet = 0; packet < 10; packet += 1) end += 8 + view.getUint32(end);
            res.write(bytes.subarray(0, end));
            setTimeout(() => res.destroy(), 150);
            return;
          }
          if (!url.searchParams.has("slow")) { res.setHeader("content-length", bytes.length); res.end(bytes); return; }
          let offset = 0;
          const timer = setInterval(() => {
            if (offset >= bytes.length) { clearInterval(timer); res.end(); return; }
            res.write(bytes.subarray(offset, offset + 4093)); offset += 4093;
          }, 5);
          res.on("close", () => clearInterval(timer));
        });
      },
    }],
  });
  await server.listen();
  const port = server.httpServer.address().port;
  const engine = process.env.BROWSER_ENGINE || "chromium";
  browser = await playwright[engine].launch({
    headless: true,
    ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}),
    ...(engine === "chromium" ? { args: ["--autoplay-policy=no-user-gesture-required"] } : {}),
  });
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${port}/__alac_test`);
  await page.evaluate(async () => {
    const { prepareOutput } = await import("/src/core/player/audio-output.js");
    const button = document.createElement("button");
    button.textContent = "Enable test audio";
    button.onclick = () => { prepareOutput(48000); prepareOutput(192000); };
    document.body.append(button);
  });
  await page.getByRole("button", { name: "Enable test audio" }).click();
  const results = [];
  for (const fixture of fixtures) {
    const result = await page.evaluate(async (fixture) => {
      const { default: init, AlacDecoder } = await import("/src/wasm/alac-decoder/alac_decoder.js");
      const { AlacWorkletPlayer } = await import("/src/core/player/alac-worklet.js");
      await init();
      const source = new Uint8Array(await (await fetch(`/__fixtures/${fixture.name}.alacstream`)).arrayBuffer());
      const expected = new Int32Array(await (await fetch(`/__fixtures/${fixture.name}.pcm`)).arrayBuffer());
      const view = new DataView(source.buffer);
      const headerLength = 24 + view.getUint16(18);
      const decoder = new AlacDecoder(source.subarray(24, headerLength));
      let offset = headerLength;
      let samples = 0;
      let maxPcmError = 0;
      let packets = 0;
      while (offset < source.length) {
        const length = view.getUint32(offset);
        const buffer = new Float32Array(decoder.maxSamplesPerPacket());
        const count = decoder.decodePacketF32(source.subarray(offset + 8, offset + 8 + length), buffer);
        // Float32 PCM is the left-aligned i32 sample * 2^-31, exact for <=24-bit ALAC.
        for (let i = 0; i < count; i += 1) maxPcmError = Math.max(maxPcmError, Math.abs(Math.round(buffer[i] * 2147483648) - expected[samples + i]));
        samples += count;
        offset += 8 + length;
        packets += 1;
      }
      decoder.free();
      let finish;
      let fail;
      const ended = new Promise((resolve, reject) => { finish = resolve; fail = reject; });
      let timeMs = 0;
      let cacheBlobBytes = 0;
      const errors = [];
      const player = new AlacWorkletPlayer({
        onTime: (value) => { timeMs = value; },
        onEnded: finish,
        onError: (error) => { errors.push(String(error)); fail(error); },
        onStreamComplete: ({ blob }) => { cacheBlobBytes = blob?.size || 0; },
      });
      let rms = 0;
      let meter;
      const started = performance.now();
      try {
        await player.load(`/__fixtures/${fixture.name}.alacstream?slow=1`);
        const startupMs = performance.now() - started;
        const analyser = player.context.createAnalyser();
        analyser.fftSize = 2048;
        player.gain.connect(analyser);
        const readings = new Float32Array(analyser.fftSize);
        meter = setInterval(() => {
          analyser.getFloatTimeDomainData(readings);
          rms = Math.max(rms, Math.sqrt(readings.reduce((sum, value) => sum + value * value, 0) / readings.length));
        }, 20);
        await player.play();
        let timeout;
        await Promise.race([ended, new Promise((_, reject) => { timeout = setTimeout(() => reject(new Error("AudioWorklet did not end")), 15000); })]);
        clearTimeout(timeout);
        return { ...fixture, samples, expectedSamples: expected.length, packets, maxPcmError, startupMs, rms, timeMs, cacheBlobBytes, errors };
      } finally { clearInterval(meter); await player.destroy(); }
    }, fixture);
    assert.equal(result.maxPcmError, 0, `${fixture.name} differs from ffmpeg PCM`);
    assert.equal(result.samples, result.expectedSamples);
    assert(result.rms > 0.05, `${fixture.name} worklet rendered silence`);
    assert(result.timeMs > 4100 && result.timeMs <= 4300, `${fixture.name} did not render full duration: ${result.timeMs}`);
    assert.deepEqual(result.errors, []);
    results.push(result);
  }
  const controls = await page.evaluate(async () => {
    const { AlacWorkletPlayer } = await import("/src/core/player/alac-worklet.js");
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const until = async (predicate, label) => {
      const start = performance.now();
      while (!predicate()) {
        if (performance.now() - start > 10000) throw new Error(`timed out: ${label}`);
        await sleep(15);
      }
    };
    let timeMs = 0;
    let ended = false;
    const errors = [];
    const player = new AlacWorkletPlayer({
      onTime: (value) => { timeMs = value; },
      onEnded: () => { ended = true; },
      onError: (error) => errors.push(String(error)),
    });
    try {
      await player.load("/__fixtures/stereo16.alacstream?slow=1");
      await until(() => player.eof && player.completeBlob, "complete download while paused");
      const pausedDownloadBytes = player.completeBlob.size;
      const beforeSeekRequests = performance.getEntriesByType("resource").length;
      const seekWhilePaused = await player.seek(2500);
      await sleep(100);
      const staysPaused = !player.playing && timeMs === seekWhilePaused;
      await player.play();
      await until(() => timeMs > 2750, "seeked playback");
      player.pause();
      await sleep(50);
      const pausedAt = timeMs;
      await sleep(150);
      const pauseStoppedClock = timeMs === pausedAt;
      const rewindWhilePaused = await player.seek(1000);
      await player.play();
      await until(() => ended, "remainder after rewind");
      const seekEndMs = timeMs;
      const afterSeekRequests = performance.getEntriesByType("resource").length;
      ended = false;
      const cancelled = player.load("/__fixtures/stereo24.alacstream?slow=1").then(() => "resolved", (error) => error.name);
      await player.load("/__fixtures/mono16.alacstream");
      const staleLoadResult = await cancelled;
      const finalChannels = player.track.channels;
      const finalSampleRate = player.track.sampleRate;
      timeMs = 0;
      await player.play();
      await until(() => timeMs > 50, "play after fast switch");
      return { pausedDownloadBytes, seekWhilePaused, staysPaused, pauseStoppedClock, rewindWhilePaused, seekEndMs, additionalSeekRequests: afterSeekRequests - beforeSeekRequests, staleLoadResult, finalChannels, finalSampleRate, errors };
    } finally { await player.destroy(); }
  });
  assert.equal(controls.pausedDownloadBytes, fixtures[0].bytes);
  assert.equal(controls.seekWhilePaused, 2500);
  assert.equal(controls.staysPaused, true);
  assert.equal(controls.pauseStoppedClock, true);
  assert.equal(controls.rewindWhilePaused, 1000);
  assert.equal(controls.seekEndMs, 4250);
  assert.equal(controls.additionalSeekRequests, 0);
  assert.equal(controls.staleLoadResult, "AbortError");
  assert.equal(controls.finalChannels, 1);
  assert.equal(controls.finalSampleRate, 48000);
  assert.deepEqual(controls.errors, []);
  const streamedSeek = await page.evaluate(async () => {
    const { AlacWorkletPlayer } = await import("/src/core/player/alac-worklet.js");
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const until = async (predicate, label) => {
      const start = performance.now();
      while (!predicate()) {
        if (performance.now() - start > 8000) throw new Error(`timed out: ${label}`);
        await sleep(15);
      }
    };
    let timeMs = 0;
    let state = "paused";
    let completed = 0;
    let peakRms = 0;
    let meter;
    const errors = [];
    const player = new AlacWorkletPlayer({
      onTime: (value) => { timeMs = value; },
      onState: (value) => { state = value; },
      onError: (error) => errors.push(String(error)),
      onStreamComplete: () => { completed += 1; },
    });
    const advance = (packets, finish = false) => fetch(`/__stream_release?id=streamed-seek&packets=${packets}${finish ? "&finish=1" : ""}`);
    try {
      await player.load("/__fixtures/stereo24.alacstream?controlled=streamed-seek");
      const seekableBeforeEof = player.canSeek && !player.eof && !player.completeBlob;
      const prefixDuration = player.seekableDuration;
      const analyser = player.context.createAnalyser();
      analyser.fftSize = 2048;
      player.gain.connect(analyser);
      const readings = new Float32Array(analyser.fftSize);
      meter = setInterval(() => {
        analyser.getFloatTimeDomainData(readings);
        peakRms = Math.max(peakRms, Math.sqrt(readings.reduce((sum, value) => sum + value * value, 0) / readings.length));
      }, 20);
      await player.seek(200);
      await player.play();
      await until(() => timeMs > 350 && peakRms > 0.05, "prefix seek real audio before EOF");
      const prefixAudibleBeforeEof = !player.eof;
      await player.seek(3000);
      await sleep(160);
      const pendingFrozen = player.seekPending && state === "buffering" && timeMs === 3000;
      await advance(35);
      await sleep(120);
      const partialFrozen = player.seekPending && timeMs === 3000;
      // A newer backward seek must replace the unreceived forward target.
      await player.seek(100);
      await until(() => timeMs > 250 && timeMs < 1000, "latest backward seek wins");
      await player.seek(3000);
      player.pause();
      await advance(89);
      await until(() => !player.seekPending, "target packets received");
      await sleep(120);
      const pausedTargetHeld = !player.playing && timeMs === 3000 && !player.eof;
      peakRms = 0;
      await player.play();
      await until(() => timeMs > 3200 && peakRms > 0.05, "forward seek real Hi-Res PCM before EOF");
      const forwardAudibleBeforeEof = !player.eof && completed === 0;
      player.pause();
      await advance(0, true);
      await until(() => player.eof && completed === 1, "one complete cache after seeks");
      const source = new Uint8Array(await (await fetch("/__fixtures/stereo24.alacstream")).arrayBuffer());
      const cached = new Uint8Array(await player.completeBlob.arrayBuffer());
      const exactCache = source.length === cached.length && source.every((value, index) => value === cached[index]);
      return { seekableBeforeEof, prefixDuration, prefixAudibleBeforeEof, pendingFrozen, partialFrozen,
        pausedTargetHeld, forwardAudibleBeforeEof, exactCache, completed, rms: peakRms, errors };
    } finally { clearInterval(meter); await player.destroy(); }
  });
  for (const key of ["seekableBeforeEof", "prefixAudibleBeforeEof", "pendingFrozen", "partialFrozen", "pausedTargetHeld", "forwardAudibleBeforeEof", "exactCache"]) assert.equal(streamedSeek[key], true, key);
  assert.equal(streamedSeek.completed, 1);
  assert.equal(controlledRequests.get("streamed-seek"), 1, "seeking must not refetch the stream");
  assert.deepEqual(streamedSeek.errors, []);
  const interrupted = await page.evaluate(async () => {
    const { AlacWorkletPlayer } = await import("/src/core/player/alac-worklet.js");
    let errorReceived = false;
    let completeReceived = false;
    const player = new AlacWorkletPlayer({
      onError: () => { errorReceived = true; },
      onStreamComplete: () => { completeReceived = true; },
    });
    try {
      await player.load("/__fixtures/stereo16.alacstream?fail=1").catch(() => {});
      const start = performance.now();
      while (!errorReceived && performance.now() - start < 5000) await new Promise((resolve) => setTimeout(resolve, 10));
      return { errorReceived, completeReceived, cacheBlob: Boolean(player.completeBlob), playing: player.playing };
    } finally { await player.destroy(); }
  });
  assert.deepEqual(interrupted, { errorReceived: true, completeReceived: false, cacheBlob: false, playing: false });
  console.log(JSON.stringify({ browser: await browser.version(), results, controls, streamedSeek, interrupted }, null, 2));
} finally {
  await browser?.close();
  await server?.close();
  await rm(directory, { recursive: true, force: true });
}
