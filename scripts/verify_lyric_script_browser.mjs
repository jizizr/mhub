/**
 * Production-build lyric script regression. Only static web files are served;
 * no Vite dev transforms, real accounts, playlists or music requests are used.
 * MUSIC_WEB_DIR selects a saved build; EXPECT_BROKEN=1 records its regression.
 * MUSIC_WEB_ORIGIN instead checks deployed static assets with all API stubbed.
 * PLAYWRIGHT_MODULE/BROWSER/BROWSER_EXECUTABLE select the installed runtime.
 */
import assert from "node:assert/strict";
import http from "node:http";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const web = path.resolve(process.env.MUSIC_WEB_DIR || path.join(root, "web"));
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const engine = process.env.BROWSER || "chromium";
const before = process.env.EXPECT_BROKEN === "1";
const prefetchOnly = process.env.LYRICS_PREFETCH_ONLY === "1";
const fallbackOnly = process.env.LYRICS_FALLBACK_ONLY === "1";
const conversionTimeout = process.env.MUSIC_WEB_ORIGIN ? 20000 : 5000;
const results = [];
const check = (name, passed, details) => results.push({ name, passed: Boolean(passed), ...(!passed && details !== undefined ? { details } : {}) });
const isConverterRequest = request => /opencc|\/assets\/(?:t2cn|cn2t)-/.test(request);
const isDictionaryRequest = (request, direction) => new RegExp(`/assets/(?:opencc-)?${direction}-[^/?]+\\.js(?:\\?|$)`).test(request);
const logs = [];
let browser;
const server = http.createServer(async (req, res) => {
  const pathname = decodeURIComponent(new URL(req.url, "http://localhost").pathname);
  let file = path.resolve(web, `.${pathname === "/" ? "/index.html" : pathname}`);
  if (!file.startsWith(`${web}/`)) { res.writeHead(403).end(); return; }
  let body;
  try { body = await fs.readFile(file); } catch { file = path.join(web, "index.html"); body = await fs.readFile(file); }
  const mime = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".ico": "image/x-icon", ".png": "image/png" }[path.extname(file)] || "application/octet-stream";
  res.writeHead(200, { "content-type": mime, "cache-control": "no-store" }); res.end(body);
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const base = process.env.MUSIC_WEB_ORIGIN || `http://127.0.0.1:${server.address().port}`;
const tracks = ["first", "second"].map(id => ({ id: `applemusic--${id}--high`, source_id: id, selected_id: id, platform: "applemusic", title: `合成歌曲 ${id}`, query: `合成歌曲 ${id}`, artist: "合成测试艺人", quality: "high", duration: 90000 }));
const lyric = {
  available: true,
  plain: "[00:00.00]繁體傳統與愛\n[00:10.00]願風帶來音樂\n[00:20.00]下一段測試歌詞\n[00:30.00]梦想与风再次相遇",
  translation: "[00:00.00]這裡聽見溫柔\n[00:10.00]歡迎回到這裡\n[00:20.00]時光從未離開\n[00:30.00]音乐会继续",
};
function wav() {
  const sampleRate = 8000, frames = sampleRate * 90;
  const bytes = Buffer.alloc(44 + frames * 2);
  bytes.write("RIFF", 0); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write("WAVEfmt ", 8);
  bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22);
  bytes.writeUInt32LE(sampleRate, 24); bytes.writeUInt32LE(sampleRate * 2, 28);
  bytes.writeUInt16LE(2, 32); bytes.writeUInt16LE(16, 34); bytes.write("data", 36); bytes.writeUInt32LE(frames * 2, 40);
  for (let index = 0; index < frames; index++) bytes.writeInt16LE(Math.round(Math.sin(index / sampleRate * Math.PI * 2 * 220) * 500), 44 + index * 2);
  return bytes;
}
const audio = wav();
async function createFixture({ width = 1280, height = 900 } = {}, moduleControl = {}) {
  const context = await browser.newContext({ viewport: { width, height }, hasTouch: width < 880, serviceWorkers: "block" });
  const page = await context.newPage();
  const log = { requests: [], errors: [], consoleErrors: [], unexpected: [], tasks: new Map() };
  logs.push(log);
  page.on("pageerror", error => log.errors.push(error.message));
  page.on("console", message => { if (message.type() === "error") log.consoleErrors.push(message.text()); });
  await page.route("**/*", async route => {
    const request = route.request(), url = new URL(request.url());
    log.requests.push(url.pathname + url.search);
    const cors = { "access-control-allow-origin": "*" };
    const json = (body, status = 200) => route.fulfill({ status, contentType: "application/json", headers: cors, body: JSON.stringify(body) });
    if (/\/assets\/SPlayerLyrics-[^/]+\.js$/.test(url.pathname)) {
      if (moduleControl.failRenderer) return route.abort("failed");
      if (moduleControl.crashRenderer) return route.fulfill({ status: 200, contentType: "text/javascript", body: 'export default { setup() { throw new Error("Synthetic renderer initialization failure") } }' });
      if (moduleControl.holdRenderer) await moduleControl.holdRenderer;
    }
    if (isDictionaryRequest(url.pathname, "t2cn")) {
      if (moduleControl.failSimplified) return route.abort("failed");
      if (moduleControl.failSimplifiedOnce) { moduleControl.failSimplifiedOnce = false; return route.abort("failed"); }
      if (moduleControl.holdSimplified) await moduleControl.holdSimplified;
    }
    if (url.pathname.startsWith("/api/")) {
      if (url.pathname.endsWith("/platforms")) return json([{ id: "applemusic", label: "Apple Music" }]);
      if (url.pathname.endsWith("/recommendations/home")) return json({ tracks: [], playlists: [], tags: [] });
      if (url.pathname.endsWith("/recommendations") || url.pathname.endsWith("/search")) return json([]);
      if (url.pathname.endsWith("/lyrics")) {
        if (moduleControl.lyricsResponse) return json(await moduleControl.lyricsResponse(url));
        return json(lyric);
      }
      if (url.pathname === "/api/v1/playbacks" && request.method() === "POST") {
        if (moduleControl.holdPlayback) await moduleControl.holdPlayback;
        const body = request.postDataJSON(), id = `applemusic--${body.selected_id}--${body.quality}`;
        const task = { id, platform: "applemusic", track_id: body.selected_id, selected_id: body.selected_id, query: body.query, title: `合成歌曲 ${body.selected_id}`, artist: "合成测试艺人", quality: body.quality, status: "completed", format: "wav", content_type: "audio/wav", duration: 90, byte_size: audio.length, audio_url: `/api/v1/playbacks/${id}/audio` };
        log.tasks.set(id, task); return json({ playback: task });
      }
      if (/\/playbacks\/[^/]+\/audio$/.test(url.pathname)) {
        const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers().range || "");
        if (range) {
          const start = Number(range[1]), end = Math.min(Number(range[2] || audio.length - 1), audio.length - 1);
          return route.fulfill({ status: 206, contentType: "audio/wav", headers: { ...cors, "accept-ranges": "bytes", "content-range": `bytes ${start}-${end}/${audio.length}`, "content-length": String(end - start + 1) }, body: audio.subarray(start, end + 1) });
        }
        return route.fulfill({ status: 200, contentType: "audio/wav", headers: { ...cors, "content-length": String(audio.length), "accept-ranges": "bytes" }, body: audio });
      }
      if (/\/playbacks\/[^/]+$/.test(url.pathname)) return json(log.tasks.get(decodeURIComponent(url.pathname.split("/").at(-1))) || {}, 200);
      log.unexpected.push(url.pathname); return json({ detail: "Unstubbed API blocked" }, 500);
    }
    if (url.pathname === "/health") return json({ status: "ok" });
    if (["data:", "blob:"].includes(url.protocol) || url.origin === new URL(base).origin) return route.continue();
    log.unexpected.push(url.origin + url.pathname); return route.abort();
  });
  await page.addInitScript(({ tracks }) => {
    if (!localStorage.getItem("lyric-fixture-initialized")) {
      localStorage.setItem("music-web-splayer-queue-v3", JSON.stringify({ queue: tracks }));
      localStorage.setItem("music-web-splayer-status-v3", JSON.stringify({ quality: "high", activeView: "discover", playIndex: 0, lyricScript: "original", repeatMode: "list" }));
      localStorage.setItem("lyric-fixture-initialized", "true");
    }
  }, { tracks });
  await page.goto(base, { waitUntil: "domcontentloaded" });
  await page.locator("select[title='歌词文字']").waitFor();
  return { context, page, log };
}
async function start(page) {
  await page.locator(".player-bar .player-controls button[title='播放']").click();
  await page.waitForFunction(() => { const audio = document.querySelector("audio"); return audio && !audio.paused && audio.currentTime > .1; });
  await page.locator(".player-bar .track-info").click();
  await page.locator(".splayer-lyrics").waitFor();
  await page.waitForFunction(() => document.querySelector(".splayer-lyrics")?.textContent?.includes("體") || document.querySelector(".splayer-lyrics")?.textContent?.includes("体"));
}
async function text(page) { return (await page.locator(".splayer-lyrics").textContent()).replace(/\s+/g, ""); }
async function expectText(page, expected) {
  try { await page.waitForFunction(expected => document.querySelector(".splayer-lyrics")?.textContent?.replace(/\s+/g, "").includes(expected), expected, { timeout: conversionTimeout }); return true; }
  catch { return false; }
}
async function visibleLyric(page, expected) {
  try {
    await page.waitForFunction(expected => {
      const player = document.querySelector(".full-player");
      const container = document.querySelector(".splayer-lyrics");
      const line = [...(container?.querySelectorAll("[class*='lyricMainLine']") || [])].find(element => element.textContent === expected);
      if (!player || !container || !line || getComputedStyle(player).opacity !== "1" || getComputedStyle(player).transform !== "none") return false;
      const rect = line.getBoundingClientRect(), clip = container.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0 && rect.top >= Math.max(0, clip.top) && rect.bottom <= Math.min(innerHeight, clip.bottom) && getComputedStyle(line).visibility === "visible";
    }, expected, { timeout: conversionTimeout });
    return true;
  } catch { return false; }
}

try {
  browser = await playwright[engine].launch({ headless: true, ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
  for (const viewport of prefetchOnly || fallbackOnly ? [] : before ? [{ width: 1280, height: 900 }] : [{ width: 390, height: 844 }, { width: 1280, height: 900 }]) {
    const { page, context, log } = await createFixture(viewport);
    const scenario = `${viewport.width}x${viewport.height}`;
    try {
      await page.locator("select[title='歌词文字']").selectOption("simplified");
      await start(page);
      const converted = await expectText(page, "繁体传统与爱");
      const observed = await text(page);
      if (before) {
        check("saved production build reproduces unconverted Traditional lyric", !converted && observed.includes("繁體傳統與愛"), { observed });
        check("saved build requests an unbundled OpenCC source path", log.requests.some(request => /\/utils\/opencc-t2cn/.test(request)), log.requests);
        console.log(JSON.stringify({ engine, before, web, origin: base, results, observed, log: { errors: log.errors, consoleErrors: log.consoleErrors, moduleRequests: log.requests.filter(isConverterRequest) } }, null, 2));
        continue;
      }
      check(`${scenario}: Simplified preference visibly renders converted lyric words`, converted && await visibleLyric(page, "繁体传统与爱"), { observed });
      check(`${scenario}: Simplified preference also converts rendered translation`, observed.includes("这里听见温柔"), { observed });
      check(`${scenario}: Simplified loads only its required dictionary`, log.requests.some(request => isDictionaryRequest(request, "t2cn")) && !log.requests.some(request => isDictionaryRequest(request, "cn2t")), log.requests.filter(isConverterRequest));
      await page.waitForTimeout(500);
      await page.screenshot({ path: `/tmp/music-lyrics-${engine}-${scenario}.png` });
      await page.locator("button.lyric-script-toggle[title='切换为繁体歌词']").click({ timeout: conversionTimeout });
      check(`${scenario}: per-song toggle restores Traditional lyric and translation`, await expectText(page, "繁體傳統與愛") && (await text(page)).includes("這裡聽見溫柔"), await text(page));
      // AMLL mounts distant rows as their layout settles; scroll the actual
      // lyric viewport before checking the mixed-source fourth line.
      await page.locator(".splayer-lyrics").hover();
      await page.mouse.wheel(0, 260);
      check(`${scenario}: Traditional toggle also converts Simplified source words`, await expectText(page, "夢想與風再次相遇") && (await text(page)).includes("音樂會繼續"), await text(page));
      await page.locator("button.lyric-script-toggle[title='切换为简体歌词']").click({ timeout: conversionTimeout });
      check(`${scenario}: per-song toggle converts back to Simplified`, await expectText(page, "繁体传统与爱"), await text(page));
      await page.locator("button.lyric-script-toggle[title='切换为繁体歌词']").click({ timeout: conversionTimeout });
      await page.locator(".full-player button[title='下一首']").click();
      check(`${scenario}: switching songs resets per-song override to Simplified preference`, await expectText(page, "繁体传统与爱"), await text(page));
      await page.locator(".full-player button[title='收起']").click();
      await page.locator(".full-player").waitFor({ state: "detached" });
      await page.locator("select[title='歌词文字']").selectOption("original");
      await page.locator(".player-bar .track-info").click();
      check(`${scenario}: Original setting preserves source text and translation`, await expectText(page, "繁體傳統與愛") && (await text(page)).includes("這裡聽見溫柔"), await text(page));
      check(`${scenario}: Original setting hides conversion toggle`, await page.locator(".lyric-script-toggle").count() === 0);
      await page.locator(".full-player button[title='收起']").click();
      await page.locator(".full-player").waitFor({ state: "detached" });
      await page.locator("select[title='歌词文字']").selectOption("simplified");
      await page.reload({ waitUntil: "domcontentloaded" });
      check(`${scenario}: Simplified preference persists after reload`, await page.locator("select[title='歌词文字']").inputValue() === "simplified");
      await start(page);
      check(`${scenario}: persisted preference converts after reload`, await expectText(page, "繁体传统与爱"), await text(page));
      check(`${scenario}: production requests only hashed OpenCC assets`, log.requests.filter(isConverterRequest).every(request => /^\/assets\/(?:opencc-|t2cn-|cn2t-)/.test(request)), log.requests.filter(isConverterRequest));
      check(`${scenario}: no uncaught browser errors`, log.errors.length === 0, log.errors);
      check(`${scenario}: no real account or provider requests`, log.unexpected.length === 0, log.unexpected);
    } finally { await context.close(); }
  }
  if (!before && !fallbackOnly) {
    {
      let release;
      const holdPlayback = new Promise(resolve => { release = resolve; });
      const { page, context, log } = await createFixture({}, { holdPlayback });
      const lyricRequests = () => log.requests.filter(request => /\/lyrics(?:\?|$)/.test(request));
      try {
        await page.locator(".player-bar .player-controls button[title='播放']").click();
        await page.waitForFunction(() => JSON.parse(localStorage.getItem("music-web-lyrics-v1") || "{}").entries?.length === 1);
        check("lyrics are requested and persisted before cold audio task responds", lyricRequests().length === 1 && !await page.locator("audio").evaluate(audio => Boolean(audio.currentSrc)), lyricRequests());
        check("prefetch uses song metadata without waiting for the playback ID", lyricRequests()[0].startsWith("/api/v1/lyrics?"), lyricRequests());
        check("background lyric loading keeps renderer and OpenCC modules lazy", !log.requests.some(request => /(?:SPlayerLyrics-|vendor-pixi|opencc-|t2cn-|cn2t-)/.test(request)), [...log.requests]);
        release();
        await page.waitForFunction(() => { const audio = document.querySelector("audio"); return audio && !audio.paused && audio.currentTime > .1; });
        await page.locator(".player-bar .track-info").click();
        check("opening lyrics renders the prefetched content without another request", await expectText(page, "繁體傳統與愛") && lyricRequests().length === 1, lyricRequests());
        await page.locator(".full-player button[title='收起']").click();
        await page.locator(".full-player").waitFor({ state: "detached" });
        await page.locator(".player-bar button[title='下一首']").click();
        await page.waitForFunction(() => JSON.parse(localStorage.getItem("music-web-lyrics-v1") || "{}").entries?.length === 2);
        check("next-track playback warms lyrics while the full player is closed", lyricRequests().length === 2 && await page.locator(".full-player").count() === 0, lyricRequests());
        await page.reload({ waitUntil: "domcontentloaded" });
        await start(page);
        check("reload restores cached lyrics with no network request", await expectText(page, "繁體傳統與愛") && lyricRequests().length === 2, lyricRequests());
        check("prefetch and reload have no uncaught errors or real provider traffic", !log.errors.length && !log.unexpected.length, { errors: log.errors, unexpected: log.unexpected });
      } finally { release(); await context.close(); }
    }
    {
      let release;
      const holdLyrics = new Promise(resolve => { release = resolve; });
      const firstLyric = { available: true, plain: "[00:00.00]第一首迟到的歌词", translation: "" };
      const secondLyric = { available: true, plain: "[00:00.00]第二首当前的歌词", translation: "" };
      const { page, context, log } = await createFixture({}, { lyricsResponse: async url => {
        const source = url.searchParams.get("track_id");
        if (source === "first") { await holdLyrics; return firstLyric; }
        return secondLyric;
      } });
      const lyricRequests = () => log.requests.filter(request => /\/lyrics(?:\?|$)/.test(request));
      try {
        await page.locator(".player-bar .player-controls button[title='播放']").click();
        await page.waitForFunction(() => { const audio = document.querySelector("audio"); return audio && !audio.paused && audio.currentTime > .1; });
        check("slow lyrics never delay audio start", lyricRequests().length === 1, lyricRequests());
        await page.locator(".player-bar .track-info").click();
        await page.locator(".lyric-empty").waitFor();
        check("opening during lyric prefetch joins the request and shows loading", lyricRequests().length === 1 && (await page.locator(".lyric-empty").textContent()).includes("正在加载歌词"), lyricRequests());
        await page.locator(".full-player button[title='下一首']").click();
        check("switching tracks displays the current song's ready lyric", await expectText(page, "第二首当前的歌词"), await text(page));
        release();
        await page.waitForFunction(() => JSON.parse(localStorage.getItem("music-web-lyrics-v1") || "{}").entries?.length === 2);
        check("late previous-song lyric only enters cache and cannot overwrite current song", (await text(page)).includes("第二首当前的歌词") && !(await text(page)).includes("第一首迟到的歌词"), await text(page));
        check("all playback and opened-player callers share one request per song", lyricRequests().length === 2, lyricRequests());
        check("slow and stale lyrics have no uncaught errors or real provider traffic", !log.errors.length && !log.unexpected.length, { errors: log.errors, unexpected: log.unexpected });
      } finally { release(); await context.close(); }
    }
  }
  if (!before && !prefetchOnly && !fallbackOnly) {
    {
      const { page, context, log } = await createFixture();
      try {
        await start(page);
        check("Original preference renders the unmodified source", (await text(page)).includes("繁體傳統與愛"), await text(page));
        check("Original preference downloads no OpenCC dictionaries or runtime", !log.requests.some(isConverterRequest), log.requests.filter(isConverterRequest));
      } finally { await context.close(); }
    }
    {
      const moduleControl = { failSimplified: true };
      const { page, context, log } = await createFixture({}, moduleControl);
      try {
        await page.locator("select[title='歌词文字']").selectOption("simplified");
        await start(page);
        const retry = page.getByRole("button", { name: "转换失败 · 重试" });
        await retry.waitFor({ timeout: 6000 });
        check("failed dictionary request retains readable original lyric", (await text(page)).includes("繁體傳統與愛"), await text(page));
        const failedRequests = log.requests.filter(request => isDictionaryRequest(request, "t2cn"));
        const audioBefore = await page.locator("audio").evaluate(audio => ({ source: audio.currentSrc, time: audio.currentTime }));
        moduleControl.failSimplified = false;
        await retry.click();
        check("retry recovers an actual failed module request without reloading", await expectText(page, "繁体传统与爱"), { text: await text(page), requests: log.requests.filter(isConverterRequest), errors: log.errors, consoleErrors: log.consoleErrors });
        const recoveredRequests = log.requests.filter(request => isDictionaryRequest(request, "t2cn"));
        check("retry makes a fresh dictionary request rather than reusing failed module cache", recoveredRequests.length > failedRequests.length && recoveredRequests.some(request => !failedRequests.includes(request)), recoveredRequests);
        const audioAfter = await page.locator("audio").evaluate(audio => ({ source: audio.currentSrc, time: audio.currentTime, paused: audio.paused }));
        check("retry preserves active audio source and advancing playback", audioAfter.source === audioBefore.source && audioAfter.time >= audioBefore.time && !audioAfter.paused, { audioBefore, audioAfter });
        check("retry failure is handled without uncaught application errors", log.errors.length === 0, log.errors);
        check("retry keeps all API and audio requests synthetic", log.unexpected.length === 0, log.unexpected);
      } finally { await context.close(); }
    }
    {
      let release;
      const holdSimplified = new Promise(resolve => { release = resolve; });
      const { page, context, log } = await createFixture({}, { holdSimplified });
      try {
        await page.locator("select[title='歌词文字']").selectOption("simplified");
        await start(page);
        await page.getByRole("status").filter({ hasText: "正在转换" }).waitFor({ timeout: 6000 });
        check("delayed dictionary leaves lyrics readable while loading", (await text(page)).includes("繁體傳統與愛"), await text(page));
        await page.locator(".full-player button[title='收起']").click();
        await page.locator(".full-player").waitFor({ state: "detached" });
        await page.locator("select[title='歌词文字']").selectOption("original");
        await page.locator(".player-bar .track-info").click();
        await expectText(page, "繁體傳統與愛");
        release();
        await page.waitForTimeout(1000);
        check("late Simplified module cannot overwrite a newer Original selection", (await text(page)).includes("繁體傳統與愛") && await page.locator(".lyric-script-toggle").count() === 0, await text(page));
        check("late conversion after component unmount produces no uncaught error", log.errors.length === 0, log.errors);
      } finally { release(); await context.close(); }
    }
  }
  if (!before && !prefetchOnly) {
    for (const viewport of [{ width: 390, height: 844 }, { width: 1280, height: 900 }]) {
      for (const failure of ["holdRenderer", "failRenderer", "crashRenderer"]) {
        let release;
        const gate = new Promise(resolve => { release = resolve; });
        const control = failure === "holdRenderer" ? { holdRenderer: gate } : { [failure]: true };
        const { page, context, log } = await createFixture(viewport, control);
        const label = `${viewport.width} ${failure}`;
        try {
          await page.locator("select[title='歌词文字']").selectOption("simplified");
          await page.locator(".player-bar .player-controls button[title='播放']").click();
          await page.locator(".player-bar .track-info").click();
          const fallback = page.locator(".lyrics-fallback");
          await fallback.waitFor();
          await page.waitForFunction(() => document.querySelector(".lyrics-fallback")?.textContent?.includes("繁体传统与爱"));
          const visible = await fallback.locator("button").first().evaluate(element => {
            const rect = element.getBoundingClientRect(), viewport = element.parentElement.getBoundingClientRect();
            return rect.width > 0 && rect.height > 0 && rect.bottom > viewport.top && rect.top < viewport.bottom && getComputedStyle(element).visibility === "visible";
          });
          check(`${label}: readable converted lyrics remain visible without the animated module`, visible);
          check(`${label}: original API payload is still cached once`, log.requests.filter(request => /\/lyrics(?:\?|$)/.test(request)).length === 1);
          if (failure === "holdRenderer") {
            await fallback.locator("button").nth(2).click();
            await page.waitForFunction(() => document.querySelector("audio")?.currentTime >= 20);
            release();
            await page.locator(".lyrics-view__animated.is-ready").waitFor();
            check(`${label}: delayed animation takes over at the current playback position`, await visibleLyric(page, "下一段测试歌词"));
            check(`${label}: fallback retires after animated readiness`, await fallback.count() === 0);
          } else {
            await page.waitForTimeout(350);
            await fallback.locator("button").nth(2).click();
            await page.waitForFunction(() => document.querySelector("audio")?.currentTime >= 20);
            check(`${label}: fallback lines can seek while audio remains usable`, await fallback.isVisible());
          }
          check(`${label}: renderer failure has no uncaught application error`, log.errors.length === 0, log.errors);
          check(`${label}: no real provider or account traffic`, log.unexpected.length === 0, log.unexpected);
        } finally { release(); await context.close(); }
      }
    }
  }
  if (!before) console.log(JSON.stringify({ engine, before, web, origin: base, results }, null, 2));
  assert.ok(results.every(result => result.passed), `${results.filter(result => !result.passed).length} lyric regression checks failed`);
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
