/** Real WebKit/Chromium trusted-click regression. No autoplay-policy bypass. */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const directory = await mkdtemp(path.join(tmpdir(), 'alac-gesture-'));
const { createServer } = await import(pathToFileURL(path.join(root, 'frontend/node_modules/vite/dist/node/index.js')));
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : 'playwright');
const files = new Map();
let server;
let releaseJitter;
let releaseRange;
let rangePrefix = 0;
const rangeRequests = [];
const rangeTask = 'applemusic--synthetic-gesture--hires';
const rangePath = `/api/v1/playbacks/${rangeTask}/audio/stream`;
const rangeRevision = 'a'.repeat(32);
try {
  execFileSync('cargo', ['build', '--quiet', '--example', 'alac_packet_stream'], { cwd: root, stdio: 'pipe' });
  for (const rate of [48000, 96000, 192000]) {
    const prefix = path.join(directory, String(rate));
    execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', `aevalsrc=0.3*sin(2*PI*440*t)|0.2*sin(2*PI*880*t):s=${rate}:d=8`, '-sample_fmt', 's32p', '-c:a', 'alac', `${prefix}.m4a`, '-y']);
    execFileSync(path.join(root, 'target/debug/examples/alac_packet_stream'), [`${prefix}.m4a`, `${prefix}.alacstream`]);
    files.set(`/__tone/${rate}`, await readFile(`${prefix}.alacstream`));
  }
  server = await createServer({ root: path.join(root, 'frontend'), server: { host: '127.0.0.1', port: 0 }, plugins: [{
    name: 'alac-gesture-fixture',
    resolveId(id) { if (id === '/__test_pinia') return '\0alac-gesture-pinia'; },
    load(id) { if (id === '\0alac-gesture-pinia') return 'export { createPinia, setActivePinia } from "pinia";'; },
    configureServer(vite) {
      vite.middlewares.use((req, res, next) => {
        if (req.url === '/__release-jitter') { releaseJitter?.(); res.end('ok'); return; }
        if (req.url === '/__release-range') { releaseRange?.(); res.end('ok'); return; }
        if (req.url === '/__gesture') {
          res.setHeader('content-type', 'text/html');
          res.end('<!doctype html><button id="start">Play</button><button id="retry">Retry</button><audio></audio>'); return;
        }
        if (files.has(req.url)) {
          res.setHeader('content-type', 'application/octet-stream'); res.setHeader('cache-control', 'no-store'); res.end(files.get(req.url)); return;
        }
        if (req.url.startsWith(`${rangePath}?`)) {
          const bytes = files.get('/__tone/192000');
          const delay = Number(new URL(req.url, 'http://localhost').searchParams.get('delay')) || 0;
          res.setHeader('content-type', 'application/x-splayer-alac');
          res.setHeader('cache-control', 'no-store, no-transform');
          res.setHeader('x-audio-stream-id', 'same-synthetic-file');
          if (req.headers.range) {
            const offset = Number(/^bytes=(\d+)-$/.exec(req.headers.range)?.[1]);
            rangeRequests.push(offset);
            res.statusCode = 206;
            res.setHeader('content-range', `bytes ${offset}-${bytes.length - 1}/${bytes.length}`);
            res.setHeader('content-length', bytes.length - offset);
            res.setHeader('accept-ranges', 'bytes');
            const timer = setTimeout(() => res.end(bytes.subarray(offset)), delay);
            res.on('close', () => clearTimeout(timer)); return;
          }
          res.setHeader('accept-ranges', 'none');
          let initialOffset = 0;
          let releaseTimer;
          const timer = setInterval(() => {
            const end = Math.min(rangePrefix, initialOffset + 8192);
            res.write(bytes.subarray(initialOffset, end)); initialOffset = end;
            if (initialOffset === rangePrefix) clearInterval(timer);
          }, 1);
          // During delayed Range headers, keep the old response delivering
          // enough real PCM for continuous playback. The consumer must keep
          // reading it while the optional new connection is still pending.
          const release = () => {
            if (!delay) { releaseTimer = setTimeout(() => res.write(bytes.subarray(rangePrefix, rangePrefix + 3)), 60); return; }
            releaseTimer = setInterval(() => {
              const end = Math.min(bytes.length, initialOffset + 65536);
              res.write(bytes.subarray(initialOffset, end)); initialOffset = end;
              if (end === bytes.length) { clearInterval(releaseTimer); res.end(); }
            }, 100);
          };
          releaseRange = release;
          res.on('close', () => { clearInterval(timer); clearTimeout(releaseTimer); if (releaseRange === release) releaseRange = null; }); return;
        }
        if (req.url === '/__tone/192000?jitter=1') {
          const bytes = files.get('/__tone/192000');
          const headerEnd = 24 + bytes.readUInt16BE(18);
          let offset = headerEnd;
          let frames = 0;
          const offsets = [];
          while (offset < bytes.length) {
            frames += bytes.readUInt32BE(offset + 4);
            offset += 8 + bytes.readUInt32BE(offset);
            offsets.push({ frames, offset });
          }
          const boundary = (seconds) => offsets.find((entry) => entry.frames >= seconds * 192000)?.offset || bytes.length;
          let sent = boundary(0.6);
          res.setHeader('content-type', 'application/octet-stream'); res.setHeader('cache-control', 'no-store');
          // Small independent writes avoid WebKit coalescing one unfinished
          // large HTTP chunk while the fixture waits for its render signal.
          let initialOffset = 0;
          const initialTimer = setInterval(() => {
            const end = Math.min(sent, initialOffset + 8192);
            res.write(bytes.subarray(initialOffset, end)); initialOffset = end;
            if (initialOffset === sent) clearInterval(initialTimer);
          }, 1);
          const timers = [];
          const release = () => {
            if (timers.length) return;
            timers.push(...[0.8, 1, 1.2, 8].map((seconds, index) => setTimeout(() => {
              const end = boundary(seconds); res.write(bytes.subarray(sent, end)); sent = end;
              if (index === 3) res.end();
            }, 1100 + index * 160)));
          };
          releaseJitter = release;
          res.on('close', () => { clearInterval(initialTimer); timers.forEach(clearTimeout); if (releaseJitter === release) releaseJitter = null; }); return;
        }
        next();
      });
    },
  }] });
  await server.listen();
  const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
  const rangeBytes = files.get('/__tone/192000');
  let rangeFrames = 0;
  rangePrefix = 24 + rangeBytes.readUInt16BE(18);
  while (rangeFrames < 0.7 * 192000) { rangeFrames += rangeBytes.readUInt32BE(rangePrefix + 4); rangePrefix += 8 + rangeBytes.readUInt32BE(rangePrefix); }
  rangePrefix += 7; // End in the next packet's framing, before its payload.
  const rangeHash = createHash('sha256').update(rangeBytes).digest('hex');
  for (const engine of (process.env.ALAC_GESTURE_ENGINE ? [process.env.ALAC_GESTURE_ENGINE] : ['webkit', 'chromium'])) {
    const browser = await playwright[engine].launch({ headless: true, ...(engine === 'chromium' && process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
    try {
      const cases = [{ rate: 96000, quality: 'hires' }, { rate: 192000, quality: 'hires' }, { rate: 48000, quality: 'lossless' },
        ...(engine === 'webkit' ? [{ rate: 96000, quality: 'hires', blocked: true }, { rate: 192000, quality: 'hires', jitter: true }] : []),
        ...[0, 3200].map((rangeDelay) => ({ rate: 192000, quality: 'hires', range: true, rangePrefix, rangeDelay, rangeSource: `${rangePath}?v=${rangeTask}&rev=${rangeRevision}&delay=${rangeDelay}` }))];
      assert(cases.some(fixture => !process.env.ALAC_GESTURE_CASE || fixture[process.env.ALAC_GESTURE_CASE]), 'Requested gesture scenario must run at least one case');
      for (const fixture of cases) {
        if (process.env.ALAC_GESTURE_CASE && !fixture[process.env.ALAC_GESTURE_CASE]) continue;
        rangeRequests.length = 0;
        const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
        try {
          const page = await context.newPage();
          await page.goto(`${origin}/__gesture`);
          await page.evaluate(async (fixture) => {
            const { createPinia, setActivePinia } = await import('/__test_pinia');
            setActivePinia(createPinia());
            const core = await import('/src/core/player/index.js');
            const { AlacWorkletPlayer } = await import('/src/core/player/alac-worklet.js');
            const { useStatusStore } = await import('/src/stores/status.js');
            const status = useStatusStore(); status.quality = fixture.quality; status.repeatMode = 'off';
            window.events = []; window.renderEvents = []; window.bufferPositions = []; window.statusStore = status; window.core = core; window.prepareEnabled = !fixture.blocked; window.streamCompletions = 0;
            const nativeFetch = window.fetch.bind(window);
            window.fetch = async (...args) => {
              const started = performance.now();
              const response = await nativeFetch(...args);
              if (args[1]?.headers?.Range) window.rangeHeaders = { waitMs: performance.now() - started, oldBytes: window.worklet.readBytes };
              return response;
            };
            let calls = 0;
            const track = (id) => ({ id, platform: 'applemusic', source_id: id, quality: fixture.quality, title: 'Synthetic ALAC', duration: 8000, streaming_mode: 'wasm_alac' });
            core.initPlayer({
              audio: document.querySelector('audio'),
              alacPlayerFactory: {
                canRequest: (track) => window.prepareEnabled && AlacWorkletPlayer.canRequest(track),
                canHandle: (track) => AlacWorkletPlayer.canHandle(track),
                create: () => {
                  const worklet = new AlacWorkletPlayer();
                  const handle = worklet.handleWorkletMessage.bind(worklet);
                  worklet.handleWorkletMessage = (message) => { if (message.type === 'state') window.renderEvents.push({ state: message.state, time: performance.now() }); handle(message); };
                  const load = worklet.load.bind(worklet);
                  worklet.load = (...args) => {
                    const complete = worklet.onStreamComplete;
                    worklet.onStreamComplete = (data) => { window.streamCompletions += 1; complete(data); };
                    return load(...args);
                  };
                  return (window.worklet = worklet);
                },
              },
              resolveTrackSource: async (track) => {
                if (!calls++) await new Promise((resolve) => setTimeout(resolve, 7000));
                window.events.push({ type: 'resolved', time: performance.now(), activation: navigator.userActivation.isActive });
                return { source: fixture.range ? fixture.rangeSource : `/__tone/${fixture.rate}${fixture.jitter ? '?jitter=1' : ''}`, track };
              },
              waitForCompletedSource: fixture.range ? (_track, { signal }) => new Promise((resolve) => {
                window.monitorSignal = signal;
                window.completeBackend = () => resolve(fixture.rangeSource);
              }) : undefined,
            });
            document.querySelector('#start').onclick = () => {
              window.started = performance.now();
              void core.playFrom([track('first'), track('second')]).then(() => { window.firstSettled = true; });
            };
            document.querySelector('#retry').onclick = () => { window.prepareEnabled = true; void core.play(); };
            const timer = setInterval(() => {
              if (status.state === 'playing' && !window.firstPlayed) {
                window.firstPlayed = performance.now(); window.firstContext = worklet.context; window.firstNode = worklet.node; window.firstDecoder = worklet.decoder;
                // Start the gap after actual render, not after fetch. Slow
                // device initialization must not absorb the entire fixture.
                if (fixture.jitter) void fetch('/__release-jitter');
                const analyser = worklet.context.createAnalyser(); worklet.gain.connect(analyser); window.analyser = analyser;
                // An automatic queue transition after the gesture has expired
                // must retain the already-authorized context.
                if (!fixture.blocked && !fixture.jitter && !fixture.range) setTimeout(() => { void core.nextTrack(false); }, 700);
              }
              if (fixture.range && window.firstPlayed && worklet.readBytes === fixture.rangePrefix && !window.rangeReleased) {
                window.rangeReleased = true; window.completeBackend(); void fetch('/__release-range');
              }
              if (window.firstPlayed && status.state === 'buffering') window.bufferPositions.push(status.position);
              if (window.firstPlayed && (fixture.blocked || fixture.jitter || (fixture.range && worklet.eof) || status.playIndex === 1) && status.position > (fixture.jitter ? 2000 : 350)) {
                const pcm = new Float32Array(window.analyser.fftSize); window.analyser.getFloatTimeDomainData(pcm);
                const rms = Math.sqrt(pcm.reduce((sum, sample) => sum + sample * sample, 0) / pcm.length);
                // The first analyser was attached to the previous GainNode.
                if (status.playIndex === 1) { worklet.gain.connect(window.analyser); }
                if (rms > 0.01) {
                  window.result = { userAgent: navigator.userAgent, ...fixture, firstPlaybackMs: window.firstPlayed - window.started, state: status.state, trackLoading: status.trackLoading, position: status.position, sampleRate: worklet.track.sampleRate, outputSampleRate: worklet.outputSampleRate, sameContext: window.firstContext === worklet.context, sameNode: window.firstNode === worklet.node, sameDecoder: window.firstDecoder === worklet.decoder, rms, events: window.events, renderEvents: window.renderEvents, bufferPositions: window.bufferPositions, streamCompletions: window.streamCompletions };
                  clearInterval(timer);
                }
              }
            }, 100);
          }, fixture);
          await page.click('#start');
          if (fixture.blocked) {
            await page.waitForFunction(() => window.firstSettled && !window.statusStore.trackLoading, null, { timeout: 15000 });
            const blocked = await page.evaluate(() => ({ state: statusStore.state, loading: statusStore.trackLoading, loaded: worklet.loaded, eof: worklet.eof, notice: statusStore.notice }));
            assert.equal(blocked.state, 'paused'); assert.equal(blocked.loading, false); assert.equal(blocked.loaded, true); assert.equal(blocked.eof, true);
            await page.click('#retry');
          }
          try { await page.waitForFunction(() => window.result, null, { timeout: 20000 }); }
          catch (error) {
            console.error(JSON.stringify(await page.evaluate(() => ({ state: statusStore.state, loading: statusStore.trackLoading, notice: statusStore.notice, events: window.events, renderEvents: window.renderEvents, firstPlayed: window.firstPlayed, loaded: window.worklet?.loaded, eof: window.worklet?.eof, bytes: window.worklet?.readBytes, queuedFrames: window.worklet?.queuedFrames, context: window.worklet?.context?.state }))));
            throw error;
          }
          const result = await page.evaluate(() => window.result);
          assert.equal(result.trackLoading, false); assert.equal(result.sameContext, true); assert.equal(result.sampleRate, fixture.rate);
          assert.equal(result.outputSampleRate, fixture.blocked ? fixture.rate : fixture.quality === 'hires' ? 192000 : 48000);
          assert.equal(result.events[0].activation, false); assert.ok(result.rms > 0.01);
          if (fixture.jitter) {
            assert.deepEqual(result.renderEvents.map(({ state }) => state), ['playing', 'buffering', 'playing']);
            assert.ok(result.bufferPositions.length >= 2);
            assert.equal(new Set(result.bufferPositions).size, 1, 'progress must freeze during a network gap');
          }
          if (fixture.range) {
            const complete = await page.evaluate(async () => ({
              hash: [...new Uint8Array(await crypto.subtle.digest('SHA-256', await worklet.completeBlob.arrayBuffer()))].map((byte) => byte.toString(16).padStart(2, '0')).join(''),
              bytes: worklet.readBytes, stoppedMonitoring: window.monitorSignal.aborted, headers: window.rangeHeaders,
            }));
            assert.deepEqual(rangeRequests, [rangePrefix]);
            assert.equal(complete.hash, rangeHash); assert.equal(complete.bytes, rangeBytes.length); assert.equal(complete.stoppedMonitoring, true);
            assert.equal(result.streamCompletions, 1); assert.equal(result.sameNode, true); assert.equal(result.sameDecoder, true);
            assert.deepEqual(result.renderEvents.map(({ state }) => state), ['playing']);
            if (fixture.rangeDelay) {
              assert.ok(complete.headers.waitMs >= 3000);
              assert.ok(complete.headers.oldBytes >= rangePrefix + 1000000, 'old reader must progress throughout the delayed Range handshake');
            }
            result.rangeCompletion = { ...complete, offset: rangeRequests[0] };
          }
          console.log(JSON.stringify({ engine, ...result }));
        } finally { await context.close(); }
      }
    } finally { await browser.close(); }
  }
} finally { await server?.close(); await rm(directory, { recursive: true, force: true }); }
