/** Production-build UI regression; all API/audio responses are synthetic. */
import assert from 'node:assert/strict';
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

// Quality and lyric preferences live in the settings popover (player bar on
// desktop, page header on phones); open it once and return the select.
async function settingsSelect(page, title) {
  if (!await page.locator(".listening-settings.is-open").count()) await page.locator(".listening-settings__trigger:visible").first().click();
  const select = page.locator(`.listening-settings.is-open select[title='${title}']`).first();
  // Close the popover after a change so it never covers later clicks.
  return {
    selectOption: async (value) => { await select.selectOption(value); await page.keyboard.press("Escape"); },
    inputValue: async () => { const value = await select.inputValue(); await page.keyboard.press("Escape"); return value; },
  };
}
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const web = path.join(root, 'web');
const pw = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : 'playwright');
const engine = process.env.BROWSER || 'chromium';
const results = [];
const check = (name, passed, details) => results.push({ name, passed: Boolean(passed), ...(!passed ? { details } : {}) });
const server = http.createServer(async (req, res) => {
  const pathname = new URL(req.url, 'http://localhost').pathname;
  let file = path.resolve(web, `.${pathname === '/' ? '/index.html' : pathname}`);
  if (!file.startsWith(web + '/')) return res.writeHead(403).end();
  let body;
  try { body = await fs.readFile(file); } catch { file = path.join(web, 'index.html'); body = await fs.readFile(file); }
  res.writeHead(200, { 'content-type': ({ '.js': 'text/javascript', '.html': 'text/html', '.css': 'text/css', '.wasm': 'application/wasm' })[path.extname(file)] || 'application/octet-stream' }).end(body);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const base = process.env.MUSIC_WEB_ORIGIN || `http://127.0.0.1:${server.address().port}`;
const track = id => ({ id: `applemusic--${id}--high`, source_id: id, selected_id: id, platform: 'applemusic', title: `合成歌曲 ${id}`, query: `合成歌曲 ${id}`, artist: '测试艺人', quality: 'high', duration: 6000 });
const search = id => ({ platform: 'applemusic', id, song: `合成歌曲 ${id}`, singer: '测试艺人' });
function wav(duration) {
  const rate = 8000, frames = rate * duration, bytes = Buffer.alloc(44 + frames * 2);
  bytes.write('RIFF', 0); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8);
  bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22);
  bytes.writeUInt32LE(rate, 24); bytes.writeUInt32LE(rate * 2, 28); bytes.writeUInt16LE(2, 32); bytes.writeUInt16LE(16, 34);
  bytes.write('data', 36); bytes.writeUInt32LE(frames * 2, 40);
  for (let i = 0; i < frames; i++) bytes.writeInt16LE(Math.round(Math.sin(i / rate * Math.PI * 440) * 500), 44 + i * 2);
  return bytes;
}
const short = wav(6), long = wav(40);
let browser;
async function fixture(width = 390, options = {}) {
  const context = await browser.newContext({ viewport: { width, height: 844 }, hasTouch: width < 880, serviceWorkers: 'block' });
  const page = await context.newPage();
  const log = { requests: [], playbacks: [], errors: [], unexpected: [] };
  const tasks = new Map();
  let release;
  const hold = new Promise(resolve => { release = resolve; });
  let count = 0;
  page.on('pageerror', e => log.errors.push(e.message));
  await page.route('**/*', async route => {
    const req = route.request(), url = new URL(req.url());
    const json = (body, status = 200) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
    if (url.pathname.startsWith('/api/')) {
      if (url.pathname.endsWith('/platforms')) return json([{ id: 'applemusic', label: 'Apple Music' }]);
      if (url.pathname.endsWith('/recommendations/home')) return json({ source: 'applemusic-personal', tracks: [search('101')], tags: [], playlists: [] });
      if (url.pathname.endsWith('/recommendations/continue')) {
        const body = req.postDataJSON(); log.requests.push(body); count++;
        if (options.hold) await hold;
        if (options.fail && count === 1) return json({ detail: '推荐暂时不可用' }, 503);
        return json({ source: 'applemusic-related', tracks: ['101', '202', '202', '303'].map(search) }).catch(() => {});
      }
      if (url.pathname.endsWith('/lyrics')) return json({ available: false });
      if (url.pathname === '/api/v1/playbacks' && req.method() === 'POST') {
        const body = req.postDataJSON(); log.playbacks.push(body);
        const id = `applemusic--${body.selected_id}--${body.quality}`, duration = body.selected_id === '101' ? 6 : 40;
        const task = { id, track_id: body.selected_id, selected_id: body.selected_id, platform: 'applemusic', quality: body.quality, title: `合成歌曲 ${body.selected_id}`, artist: '测试艺人', status: 'completed', format: 'wav', content_type: 'audio/wav', duration, byte_size: duration === 6 ? short.length : long.length, audio_url: `/api/v1/playbacks/${id}/audio` };
        tasks.set(id, task); return json({ playback: task });
      }
      if (/\/playbacks\/[^/]+\/audio$/.test(url.pathname)) {
        const id = decodeURIComponent(url.pathname.split('/').at(-2));
        const bytes = id.includes('--101--') ? short : long;
        const range = /^bytes=(\d+)-(\d*)$/.exec(req.headers().range || '');
        const headers = { 'accept-ranges': 'bytes', 'access-control-allow-origin': '*' };
        if (range) {
          const start = Number(range[1]), end = Math.min(Number(range[2] || bytes.length - 1), bytes.length - 1);
          return route.fulfill({ status: 206, contentType: 'audio/wav', headers: { ...headers, 'content-range': `bytes ${start}-${end}/${bytes.length}`, 'content-length': String(end - start + 1) }, body: bytes.subarray(start, end + 1) });
        }
        return route.fulfill({ status: 200, contentType: 'audio/wav', headers, body: bytes });
      }
      if (/\/playbacks\/[^/]+$/.test(url.pathname)) return json(tasks.get(decodeURIComponent(url.pathname.split('/').at(-1))) || {}, 200);
      if (url.pathname === "/api/v1/auth/config") return json({ enabled: false });
      log.unexpected.push(url.pathname); return json({ detail: 'Unstubbed API blocked' }, 500);
    }
    if (url.pathname === '/health') return json({ status: 'ok' });
    if (url.origin === new URL(base).origin || ['data:', 'blob:'].includes(url.protocol)) return route.continue();
    log.unexpected.push(url.pathname); return route.abort();
  });
  await page.addInitScript(seed => {
    localStorage.setItem('music-web-splayer-queue-v3', JSON.stringify({ queue: [seed] }));
    localStorage.setItem('music-web-splayer-status-v3', JSON.stringify({ quality: 'high', activeView: 'queue', playIndex: 0, repeatMode: 'list', autoplayEnabled: false }));
  }, track('101'));
  await page.goto(base, { waitUntil: 'domcontentloaded' });
  await page.locator('.queue-autoplay__toggle').waitFor();
  return { page, context, log, release };
}
async function snapshot(page) {
  return page.evaluate(() => {
    const provides = document.querySelector('[data-v-app]').__vue_app__._context.provides;
    const pinia = Reflect.ownKeys(provides).map(key => provides[key]).find(v => v?._s?.get);
    const status = pinia._s.get('status'), queue = pinia._s.get('queue'), audio = document.querySelector('audio');
    return { ids: queue.queue.map(t => t.source_id), recommended: queue.queue.filter(t => t.autoplayRecommended).map(t => t.source_id), current: status.currentTrack?.source_id, state: status.state, enabled: status.autoplayEnabled, mode: status.repeatMode, autoState: status.autoplayState, src: audio.currentSrc, time: audio.currentTime, paused: audio.paused };
  });
}
async function until(page, predicate, timeout = 12000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const value = await snapshot(page); if (predicate(value)) return value; await page.waitForTimeout(100); }
  throw new Error('State timeout: ' + JSON.stringify(await snapshot(page)));
}
async function play(page) {
  await page.locator('.player-bar button[title="播放"]').click();
  return until(page, v => v.current === '101' && v.time > .1 && !v.paused);
}
try {
  browser = await pw[engine].launch({ headless: true, ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
  for (const width of [390, 1280]) {
    const { page, context, log, release } = await fixture(width, { hold: true });
    try {
      check(`${width}: autoplay starts off`, !(await snapshot(page)).enabled);
      await play(page);
      await page.locator('.queue-autoplay__toggle').click();
      await until(page, v => v.autoState === 'loading');
      await (await settingsSelect(page, "音质")).selectOption('hires');
      const before = await snapshot(page);
      release();
      const ready = await until(page, v => v.ids.length === 3);
      check(`${width}: metadata appends unique songs and preserves current audio`, ready.current === '101' && ready.src === before.src && ready.ids.join(',') === '101,202,303', ready);
      check(`${width}: recommendation request includes seed and current queue`, log.requests[0]?.seed_id === '101' && log.requests[0]?.exclude.includes('101'), log.requests);
      check(`${width}: enabling infinity exits repeat mode`, ready.mode === 'off');
      const next = await until(page, v => v.current === '202' && v.time > .1 && !v.paused);
      check(`${width}: actual ended audio continues into recommendation`, next.current === '202', next);
      check(`${width}: next playback uses current Hi-Res setting`, log.playbacks.some(v => v.selected_id === '202' && v.quality === 'hires'), log.playbacks);
      check(`${width}: generated rows are labelled`, await page.locator('.queue-item__recommended').count() === 2);
      await page.screenshot({ path: `/tmp/music-autoplay-${engine}-${width}.png` });
      await page.locator('.queue-autoplay__toggle').click();
      const disabled = await snapshot(page);
      check(`${width}: disabling keeps current song and removes unplayed auto songs`, disabled.ids.join(',') === '101,202' && disabled.current === '202' && !disabled.paused, disabled);
      check(`${width}: no browser errors or external account requests`, !log.errors.length && !log.unexpected.length, log);
    } finally { release(); await context.close(); }
  }
  {
    const { page, context, log, release } = await fixture(390, { hold: true });
    try {
      await play(page);
      await page.locator('.queue-autoplay__toggle').click();
      await until(page, v => v.autoState === 'loading');
      await page.locator('.queue-autoplay__toggle').click(); release();
      await page.waitForTimeout(500);
      check('late recommendation cannot append after disabling', (await snapshot(page)).ids.join(',') === '101');
      const stopped = await until(page, v => v.state === 'stopped');
      check('disabled infinity stops at end instead of looping', stopped.current === '101' && stopped.paused, stopped);
      check('cancelled request has no uncaught error', !log.errors.length, log.errors);
    } finally { release(); await context.close(); }
  }
  {
    const { page, context, log } = await fixture(390, { fail: true });
    try {
      await play(page);
      await page.locator('.queue-autoplay__toggle').click();
      await until(page, v => v.autoState === 'error');
      check('metadata failure retains current playback', !(await snapshot(page)).paused);
      await until(page, v => v.state === 'stopped');
      check('failure at end does not silently loop old songs', log.requests.length === 1, log.requests);
      await page.locator('.queue-autoplay__retry').click();
      const recovered = await until(page, v => v.current === '202' && v.time > .1 && !v.paused);
      check('explicit retry resumes with fresh recommendation', recovered.current === '202' && log.requests.filter(request => request.seed_id === '101').length === 2, recovered);
      check('retry has no uncaught errors or unstubbed calls', !log.errors.length && !log.unexpected.length, log);
    } finally { await context.close(); }
  }
  console.log(JSON.stringify({ engine, origin: base, results }, null, 2));
  assert.ok(results.every(r => r.passed), `${results.filter(r => !r.passed).length} autoplay checks failed`);
} finally { await browser?.close(); await new Promise(resolve => server.close(resolve)); }
