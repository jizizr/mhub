/** Real built App + native Hi-Res fixture; APIs are isolated from personal data.
 * MUSIC_WEB_ORIGIN optionally loads published static assets. Mobile UA emulation
 * verifies routing and controls, not an actual phone's lock-screen lifecycle.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';
import { tmpdir } from 'node:os';
import { execFileSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const pw = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : 'playwright');
const engines = process.env.BROWSER && process.env.BROWSER !== 'both' ? [process.env.BROWSER] : ['chromium', 'webkit'];
const directory = await fs.mkdtemp(path.join(tmpdir(), 'music-media-controls-'));
const reports = [];
let server;
try {
  execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'aevalsrc=0.03*sin(2*PI*440*t)|0.03*sin(2*PI*660*t):s=96000:d=40', '-sample_fmt', 's32', '-c:a', 'flac', path.join(directory, 'fixture.flac'), '-y'], { stdio: 'pipe' });
  const audio = await fs.readFile(path.join(directory, 'fixture.flac'));
  server = http.createServer(async (req, res) => {
    const pathname = new URL(req.url, 'http://localhost').pathname;
    const file = path.resolve(root, 'web', `.${pathname === '/' ? '/index.html' : pathname}`);
    if (!file.startsWith(path.join(root, 'web') + path.sep)) return res.writeHead(403).end();
    try {
      const bytes = await fs.readFile(file);
      res.writeHead(200, { 'content-type': ({ '.js': 'text/javascript', '.css': 'text/css', '.html': 'text/html', '.wasm': 'application/wasm' })[path.extname(file)] || 'application/octet-stream' }).end(bytes);
    } catch { res.writeHead(404).end(); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const base = process.env.MUSIC_WEB_ORIGIN || `http://127.0.0.1:${server.address().port}`;
  const tracks = ['alpha', 'beta'].map(id => ({ id: `applemusic--${id}--hires`, source_id: id, selected_id: id, platform: 'applemusic', title: `Synthetic ${id}`, artist: 'Public test tone', album: 'Synthetic fixtures', quality: 'hires', duration: 40000 }));
  for (const engine of engines) {
    const android = engine === 'chromium';
    let executablePath = process.env.BROWSER_EXECUTABLE;
    if (!executablePath && android && process.platform === 'darwin') executablePath = '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge';
    const browser = await pw[engine].launch({ headless: true, ...(executablePath ? { executablePath } : {}) });
    const context = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, serviceWorkers: 'block', ...(android ? { userAgent: 'Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Mobile Safari/537.36' } : {}) });
    const page = await context.newPage(), requests = [], errors = [], unexpected = [], results = [], tasks = new Map();
    const check = (name, passed, details) => { results.push({ name, passed: Boolean(passed), ...(!passed ? { details } : {}) }); assert.ok(passed, name); };
    let failure;
    try {
      page.on('pageerror', error => errors.push(error.message));
      await page.route('**/*', async route => {
        const request = route.request(), url = new URL(request.url());
        const json = body => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
        if (url.pathname === '/health') return json({ status: 'ok' });
        if (url.pathname.startsWith('/api/')) {
          if (url.pathname.endsWith('/platforms')) return json([{ id: 'applemusic', label: 'Apple Music' }]);
          if (url.pathname.endsWith('/recommendations/home')) return json({ tracks: [], playlists: [], tags: [] });
          if (url.pathname.endsWith('/recommendations') || url.pathname.endsWith('/search')) return json([]);
          if (url.pathname.endsWith('/lyrics')) return json({ available: true, plain: '[00:00.00]Public test lyrics' });
          if (url.pathname === '/api/v1/playbacks' && request.method() === 'POST') {
            const body = request.postDataJSON(); requests.push(body);
            const id = `applemusic--${body.selected_id}--${body.quality}`;
            const task = { id, platform: 'applemusic', track_id: body.selected_id, selected_id: body.selected_id, query: body.query, title: `Synthetic ${body.selected_id}`, artist: 'Public test tone', album: 'Synthetic fixtures', quality: body.quality, status: 'completed', format: 'flac', content_type: 'audio/flac', duration: 40, sample_rate: 96000, bit_depth: 24, byte_size: audio.length, audio_url: `/api/v1/playbacks/${id}/audio` };
            tasks.set(id, task); return json({ playback: task });
          }
          if (/\/playbacks\/[^/]+\/audio$/.test(url.pathname)) {
            const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers().range || '');
            if (range) {
              const start = Number(range[1]), end = Math.min(Number(range[2] || audio.length - 1), audio.length - 1);
              return route.fulfill({ status: 206, contentType: 'audio/flac', headers: { 'accept-ranges': 'bytes', 'content-range': `bytes ${start}-${end}/${audio.length}`, 'content-length': String(end - start + 1) }, body: audio.subarray(start, end + 1) });
            }
            return route.fulfill({ status: 200, contentType: 'audio/flac', headers: { 'accept-ranges': 'bytes', 'content-length': String(audio.length) }, body: audio });
          }
          if (/\/playbacks\/[^/]+$/.test(url.pathname)) return json(tasks.get(decodeURIComponent(url.pathname.split('/').at(-1))) || {});
          if (url.pathname === '/api/v1/auth/config') return json({ enabled: false });
          unexpected.push(url.pathname); return route.abort();
        }
        if (['data:', 'blob:'].includes(url.protocol) || url.origin === new URL(base).origin) return route.continue();
        unexpected.push(url.origin + url.pathname); return route.abort();
      });
      await page.addInitScript(({ tracks, android }) => {
        localStorage.setItem('music-web-splayer-queue-v3', JSON.stringify({ queue: tracks }));
        localStorage.setItem('music-web-splayer-status-v3', JSON.stringify({ quality: 'hires', activeView: 'discover', playIndex: 0, lyricScript: 'original', repeatMode: 'off', autoplayEnabled: false }));
        if (android) Object.defineProperty(navigator, 'userAgentData', { configurable: true, value: { platform: 'Android' } });
        window.mediaActions = {};
        const session = navigator.mediaSession, original = session.setActionHandler.bind(session);
        session.setActionHandler = (name, handler) => { const result = original(name, handler); if (handler) window.mediaActions[name] = handler; else delete window.mediaActions[name]; return result; };
      }, { tracks, android });
      await page.goto(base, { waitUntil: 'domcontentloaded' });
      await page.locator('.player-bar .player-controls button[title="播放"]').click();
      await page.waitForFunction(() => { const audio = document.querySelector('audio'); return audio && !audio.paused && audio.currentTime > .2 && navigator.mediaSession.playbackState === 'playing'; });
      check('native Hi-Res output advances', await page.locator('audio').evaluate(audio => audio.currentTime > .2 && audio.readyState >= 2));
      check('playback request retains selected hires quality', requests.length > 0 && requests.every(r => r.quality === 'hires'), requests);
      if (android) check('Android uses native lossless session without WASM request', requests.every(r => !r.streaming_mode), requests);
      check('metadata shows current song and artist', await page.evaluate(() => navigator.mediaSession.metadata.title === 'Synthetic alpha' && navigator.mediaSession.metadata.artist === 'Public test tone'));
      check('system controls registered by real App', await page.evaluate(() => ['play', 'pause', 'previoustrack', 'nexttrack', 'seekto', 'seekbackward', 'seekforward'].every(key => typeof mediaActions[key] === 'function')));
      const audioSession = await page.evaluate(() => navigator.audioSession?.type);
      if (audioSession !== undefined) check('WebKit uses playback AudioSession', audioSession === 'playback', audioSession);
      await page.evaluate(() => mediaActions.seekto({ seekTime: 12 }));
      await page.waitForFunction(() => { const audio = document.querySelector('audio'); return audio.currentTime >= 12 && audio.currentTime < 15 && !audio.paused; });
      check('system seek changes real audio position', true);
      await page.evaluate(() => mediaActions.pause());
      await page.waitForFunction(() => document.querySelector('audio').paused && navigator.mediaSession.playbackState === 'paused');
      await page.evaluate(() => { window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })); document.dispatchEvent(new Event('visibilitychange')); });
      await page.waitForTimeout(300);
      check('system pause survives lifecycle events', await page.evaluate(() => document.querySelector('audio').paused && navigator.mediaSession.playbackState === 'paused'));
      await page.evaluate(() => mediaActions.play());
      await page.waitForFunction(() => !document.querySelector('audio').paused && navigator.mediaSession.playbackState === 'playing');
      check('system play resumes audio', true);
      await page.evaluate(() => mediaActions.nexttrack());
      await page.waitForFunction(() => navigator.mediaSession.metadata?.title === 'Synthetic beta' && !document.querySelector('audio').paused && document.querySelector('audio').currentTime > .2);
      check('system next updates actual playback and metadata', true);
      if (await page.evaluate(() => typeof mediaActions.stop === 'function')) {
        await page.evaluate(() => mediaActions.stop());
        await page.waitForFunction(() => navigator.mediaSession.metadata === null && navigator.mediaSession.playbackState === 'none');
        await page.waitForTimeout(200);
        check('stop clears media controls after delayed native events', await page.evaluate(() => navigator.mediaSession.metadata === null && document.querySelector('audio').paused));
      }
      check('no page errors or unstubbed private API requests', !errors.length && !unexpected.length, { errors, unexpected });
    } catch (error) { failure = error.stack || error.message; }
    reports.push({ engine, version: browser.version(), androidUserAgent: android, origin: new URL(base).origin, results, failure, requests, errors, unexpected });
    await context.close(); await browser.close();
  }
  process.stdout.write(JSON.stringify({ scope: 'Built App, synthetic native FLAC, emulated mobile viewport/UA; not physical lock-screen verification.', reports }, null, 2) + '\n');
  if (reports.some(r => r.failure)) process.exitCode = 1;
} finally {
  if (server) { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
  await fs.rm(directory, { recursive: true, force: true });
}
