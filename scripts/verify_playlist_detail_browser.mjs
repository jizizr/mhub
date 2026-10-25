/**
 * Real Vue UI regression with an isolated profile, synthetic playlists and WAV.
 * Every user action is a real button click. Stores are read only for assertions;
 * fixtures are seeded into localStorage before the application starts.
 * All provider/API requests are stubbed, including when MUSIC_WEB_ORIGIN points
 * at a deployment. No account, private playlist or real music is requested.
 * The local default serves the production web/ build, never Vite development.
 * Full recommendation URLs are required by the collection fixture; unknown
 * links/platforms return 400 rather than hiding request-contract regressions.
 *
 * PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs BROWSER=webkit node scripts/verify_playlist_detail_browser.mjs
 * BROWSER_EXECUTABLE optionally selects an installed Chromium/Edge executable.
 */
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const web = path.resolve(process.env.MUSIC_WEB_DIR || path.join(root, "web"));
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const engine = process.env.BROWSER || "chromium";
assert.ok(["chromium", "webkit"].includes(engine));
const checks = [];
const screenshots = [];
let server;
let browser;

function check(scenario, name, passed, details) {
  checks.push({ scenario, name, passed: Boolean(passed), ...(!passed ? { details } : {}) });
}

const cover = `data:image/svg+xml,${encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="400" height="400"><defs><linearGradient id="g" x2="1" y2="1"><stop stop-color="#dd3f75"/><stop offset="1" stop-color="#4465ac"/></linearGradient></defs><rect width="400" height="400" fill="url(#g)"/><circle cx="200" cy="200" r="110" fill="none" stroke="white" stroke-opacity=".4" stroke-width="30"/></svg>')}`;
const track = id => ({ id: `applemusic--${id}--high`, source_id: id, selected_id: id, platform: "applemusic", title: `测试歌曲 ${id}`, query: `测试歌曲 ${id}`, artist: "合成测试艺人", quality: "high", duration: 90000, cover });
const recommendationIds = Array.from({ length: 57 }, (_, i) => i === 2 ? "current" : `r-${i + 1}`);
const originalIds = ["r-2", "current", "keep", "r-4", "tail"];
const localIds = ["local-1", "local-2", "local-3"];
const searchTrack = id => ({ platform: "applemusic", id, song: `测试歌曲 ${id}`, singer: "合成测试艺人", cover });
const cards = [
  { id: "pl.synthetic-long", name: "测试专属歌单", resource_type: "playlists", url: "https://music.apple.com/cn/playlist/pl.synthetic-long", cover, curator: "Apple Music", description: "完整浏览 57 首测试曲目，再选择播放方式。" },
  { id: "1624000713", name: "测试推荐专辑", resource_type: "albums", url: "https://music.apple.com/cn/album/1624000713", cover, curator: "合成测试艺人" },
  { id: "pl.synthetic-retry", name: "失败后重试", resource_type: "playlists", url: "https://music.apple.com/cn/playlist/pl.synthetic-retry", cover },
  { id: "pl.synthetic-slow", name: "延迟返回", resource_type: "playlists", url: "https://music.apple.com/cn/playlist/pl.synthetic-slow", cover },
];

function wav() {
  const sampleRate = 8000, frames = sampleRate * 90;
  const bytes = Buffer.alloc(44 + frames * 2);
  bytes.write("RIFF", 0); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write("WAVEfmt ", 8);
  bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22);
  bytes.writeUInt32LE(sampleRate, 24); bytes.writeUInt32LE(sampleRate * 2, 28);
  bytes.writeUInt16LE(2, 32); bytes.writeUInt16LE(16, 34); bytes.write("data", 36);
  bytes.writeUInt32LE(frames * 2, 40);
  for (let index = 0; index < frames; index++) bytes.writeInt16LE(Math.round(Math.sin(index / sampleRate * Math.PI * 2 * 220) * 500), 44 + index * 2);
  return bytes;
}
const audio = wav();

async function snapshot(page) {
  return page.evaluate(() => {
    const provides = document.querySelector("[data-v-app]")?.__vue_app__?._context?.provides;
    const pinia = provides && Reflect.ownKeys(provides).map(key => provides[key]).find(value => value?._s?.get);
    const status = pinia?._s.get("status"), queue = pinia?._s.get("queue"), media = pinia?._s.get("media");
    const audio = document.querySelector("audio");
    return {
      ids: queue?.queue.map(item => item.source_id), originalIds: queue?.originalQueue?.map(item => item.source_id),
      sourcePlaylist: queue?.sourcePlaylist, current: status?.currentTrack?.source_id, mediaTrack: media?.track?.source_id,
      currentQuality: status?.currentTrack?.quality, mediaQuality: media?.track?.quality,
      playIndex: status?.playIndex, state: status?.state, activeView: status?.activeView, shuffle: status?.shuffleMode,
      audioSrc: audio?.currentSrc, audioTime: audio?.currentTime, audioPaused: audio?.paused,
      events: [...(window.__playlistAudioEvents || [])],
    };
  });
}

async function settle(page) { await page.waitForTimeout(360); }

async function createFixture(base, viewport, { homeIds = ["current"], searchIds = ["current"] } = {}) {
  const context = await browser.newContext({ viewport, deviceScaleFactor: 1, serviceWorkers: "block", hasTouch: viewport.width < 880 });
  const page = await context.newPage();
  const log = { posts: [], resolves: [], rejectedResolves: [], errors: [], unexpected: [] };
  let retryCount = 0, slowRelease;
  const slow = new Promise(resolve => { slowRelease = resolve; });
  const tasks = new Map();
  page.on("pageerror", error => log.errors.push(error.message));
  await page.route("**/*", async route => {
    const request = route.request(), url = new URL(request.url());
    const json = (body, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
    if (url.pathname.startsWith("/api/")) {
      if (url.pathname.endsWith("/platforms")) return json([{ id: "applemusic", label: "Apple Music" }]);
      if (url.pathname.endsWith("/recommendations/home")) return json({ tracks: homeIds.map(searchTrack), playlists: cards, tags: [] });
      if (url.pathname.endsWith("/recommendations")) return json(homeIds.map(searchTrack));
      if (url.pathname.endsWith("/search")) return json(searchIds.map(searchTrack));
      if (url.pathname.endsWith("/collections/resolve")) {
        const collection = url.searchParams.get("url");
        const resolved = { method: request.method(), platform: url.searchParams.get("platform"), collection, limit: url.searchParams.get("limit") };
        log.resolves.push(resolved);
        const card = cards.find(item => item.url === collection);
        if (resolved.method !== "GET" || resolved.platform !== "applemusic" || resolved.limit !== null || !card) {
          log.rejectedResolves.push(resolved);
          return json({ detail: "Collection fixture requires the exact full Apple Music card URL, platform, and unlimited detail request" }, 400);
        }
        if (card.id === "pl.synthetic-retry" && retryCount++ === 0) return json({ detail: "合成的临时加载错误" }, 503);
        if (card.id === "pl.synthetic-slow") await slow;
        const ids = card.id === "pl.synthetic-long" ? recommendationIds : card.resource_type === "albums" ? ["album-1", "album-2", "album-3"] : ["retry-1", "retry-2"];
        return json({ platform: "applemusic", id: card.id, source_url: collection, title: card.name, items: ids.map(searchTrack) }).catch(() => {});
      }
      if (url.pathname === "/api/v1/playbacks" && request.method() === "POST") {
        const body = request.postDataJSON(); log.posts.push(body);
        const id = `applemusic--${body.selected_id}--${body.quality}`;
        const task = { id, platform: "applemusic", track_id: body.selected_id, selected_id: body.selected_id, query: body.query, title: `测试歌曲 ${body.selected_id}`, artist: "合成测试艺人", quality: body.quality, status: "completed", format: "wav", content_type: "audio/wav", duration: 90, byte_size: audio.length, audio_url: `/api/v1/playbacks/${id}/audio` };
        tasks.set(id, task); return json({ playback: task });
      }
      if (/\/playbacks\/[^/]+\/audio$/.test(url.pathname)) {
        const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers().range || "");
        if (range) {
          const start = Number(range[1]), end = Math.min(Number(range[2] || audio.length - 1), audio.length - 1);
          return route.fulfill({ status: 206, contentType: "audio/wav", headers: { "accept-ranges": "bytes", "content-range": `bytes ${start}-${end}/${audio.length}`, "content-length": String(end - start + 1) }, body: audio.subarray(start, end + 1) });
        }
        return route.fulfill({ status: 200, contentType: "audio/wav", headers: { "content-length": String(audio.length), "accept-ranges": "bytes" }, body: audio });
      }
      if (url.pathname.endsWith("/lyrics")) return json({ lines: [] });
      if (/\/playbacks\/[^/]+$/.test(url.pathname)) return json(tasks.get(decodeURIComponent(url.pathname.split("/").at(-1))) || { detail: "Synthetic task not found" }, tasks.has(decodeURIComponent(url.pathname.split("/").at(-1))) ? 200 : 404);
      if (url.pathname === "/api/v1/auth/config") return json({ enabled: false });
      log.unexpected.push(url.pathname); return json({ detail: "Unstubbed API request blocked" }, 500);
    }
    if (url.pathname === "/health") return json({ status: "ok" });
    if (["data:", "blob:"].includes(url.protocol) || url.origin === new URL(base).origin) return route.continue();
    log.unexpected.push(url.origin + url.pathname); return route.abort();
  });
  await page.addInitScript(({ queue, local, shuffle }) => {
    localStorage.setItem("music-web-splayer-queue-v3", JSON.stringify({ queue, sourcePlaylist: { id: "old-queue", name: "合成原队列" } }));
    localStorage.setItem("music-web-splayer-status-v3", JSON.stringify({ quality: "high", activeView: "discover", playIndex: 1, shuffleMode: shuffle ? "on" : "off", repeatMode: "list" }));
    localStorage.setItem("music-web-splayer-playlists-v1", JSON.stringify([local]));
    window.__playlistAudioEvents = [];
    for (const type of ["loadstart", "pause", "playing", "emptied"]) document.addEventListener(type, event => {
      if (event.target instanceof HTMLAudioElement) window.__playlistAudioEvents.push({ type, at: performance.now(), time: event.target.currentTime });
    }, true);
    // Reproducible shuffle selection, without changing player/store state.
    Math.random = () => .73;
  }, { queue: originalIds.map(track), local: { id: "synthetic-local", name: "普通测试歌单", platform: "applemusic", cover, tracks: localIds.map(track) }, shuffle: viewport.width < 880 });
  await page.goto(base, { waitUntil: "domcontentloaded" });
  await page.locator(".artwork-rail--featured .artwork-card").first().waitFor();
  return { page, context, log, slowRelease };
}

async function openCard(page, name) {
  const card = page.locator(".artwork-rail--featured .artwork-card").filter({ hasText: name });
  await card.scrollIntoViewIfNeeded();
  // click() can still scroll a large mobile card into its action point after
  // scrollIntoViewIfNeeded(). Capture the actual entry position at dispatch,
  // which is the same position the application records for Back restoration.
  await card.evaluate(element => element.addEventListener("click", () => {
    window.__playlistEntryScroll = window.scrollY;
  }, { capture: true, once: true }));
  await card.click();
  const scroll = await page.evaluate(() => window.__playlistEntryScroll);
  await page.locator(".playlist-detail").waitFor();
  return scroll;
}

async function inspectLayout(page, scenario) {
  await page.evaluate(() => window.scrollTo(0, 0)); await settle(page);
  const top = await page.evaluate(() => ({ width: innerWidth, scrollWidth: document.documentElement.scrollWidth }));
  check(scenario, "detail has no horizontal overflow", top.scrollWidth <= top.width + 1, top);
  for (const button of await page.locator(".playlist-detail__actions > button").all()) {
    await button.scrollIntoViewIfNeeded();
    const hit = await button.evaluate(element => {
      const rect = element.getBoundingClientRect();
      return element.contains(document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2));
    });
    check(scenario, `action is reachable: ${(await button.textContent()).trim()}`, hit);
  }
  await page.evaluate(() => window.scrollTo(0, document.scrollingElement.scrollHeight)); await settle(page);
  const last = await page.locator(".playlist-detail .song-row").last().boundingBox();
  const dock = await page.locator(".player-bar").boundingBox();
  check(scenario, "final track scrolls fully above the player dock", last.y >= 0 && last.y + last.height <= dock.y - 6, { last, dock });
  await page.evaluate(() => window.scrollTo(0, 0)); await settle(page);
  const screenshot = `/tmp/music-playlist-detail-${engine}-${scenario}.png`;
  await page.screenshot({ path: screenshot }); screenshots.push(screenshot);
}

async function verifySingleTrackAppend(base, viewport) {
  const scenario = `${viewport.width}x${viewport.height}-single-append`;
  const { page, context, log, slowRelease } = await createFixture(base, viewport, {
    homeIds: ["current", "recommend-append", "recommend-batch"], searchIds: ["search-append"],
  });
  const interruptions = state => state.events.filter(event => ["pause", "emptied", "loadstart"].includes(event.type)).length;
  try {
    await page.locator(".player-bar .player-controls button[title='播放']").click();
    await page.waitForFunction(() => { const audio = document.querySelector("audio"); return audio && !audio.paused && audio.currentTime > .2; });
    const savedPlaylists = await page.evaluate(() => localStorage.getItem("music-web-splayer-playlists-v1"));

    async function appendFrom(label, button, id, { existing = false } = {}) {
      const before = await snapshot(page);
      await button.click();
      await settle(page);
      const after = await snapshot(page);
      const expected = existing ? before.ids : [...before.ids, id];
      check(scenario, `${label}: adds at the end without changing prior queue order`, JSON.stringify(after.ids) === JSON.stringify(expected), { expected, actual: after.ids });
      check(scenario, `${label}: preserves active song, quality and uninterrupted playback`,
        after.current === before.current && after.mediaTrack === before.mediaTrack && after.playIndex === before.playIndex &&
        after.currentQuality === before.currentQuality && after.mediaQuality === before.mediaQuality &&
        after.audioSrc === before.audioSrc && !after.audioPaused && after.audioTime >= before.audioTime &&
        interruptions(after) === interruptions(before), { before, after });
      const persisted = await page.evaluate(() => ({
        ids: JSON.parse(localStorage.getItem("music-web-splayer-queue-v3") || "{}").queue?.map(item => item.source_id),
        playlists: localStorage.getItem("music-web-splayer-playlists-v1"),
      }));
      check(scenario, `${label}: persists only queue changes`, JSON.stringify(persisted.ids) === JSON.stringify(expected) && persisted.playlists === savedPlaylists, persisted.ids);
    }

    // Feed rows keep per-song actions in their ⋯ menu.
    const viaMenu = (row, item, more = "更多操作") => ({ click: async () => { await row.getByRole("button", { name: more }).click(); await page.getByRole("menuitem", { name: item, exact: true }).click(); } });
    const feedButton = viaMenu(page.locator(".recommend-list-row").filter({ hasText: "测试歌曲 recommend-append" }), "加入播放队列");
    await appendFrom("recommendation song plus", feedButton, "recommend-append");
    await appendFrom("repeated recommendation plus", feedButton, "recommend-append", { existing: true });
    const currentButton = viaMenu(page.locator(".recommend-list-row").filter({ hasText: "测试歌曲 current" }), "加入播放队列");
    await appendFrom("currently playing song plus", currentButton, "current", { existing: true });
    await appendFrom("recommendation batch append", viaMenu(page.locator(".home-actions"), "全部加入播放队列", "更多推荐操作"), "recommend-batch");

    await openCard(page, "测试专属歌单");
    await page.locator(".playlist-detail .song-row").last().waitFor();
    const recommendationButton = page.locator(".playlist-detail .song-row").filter({ hasText: "测试歌曲 r-1" }).filter({ has: page.locator(".song-row__title", { hasText: /^测试歌曲 r-1$/ }) }).getByRole("button", { name: "加入播放队列", exact: true });
    await appendFrom("recommended playlist song plus", recommendationButton, "r-1");

    await page.locator(".sidebar .nav-item").filter({ hasText: /^资料库$|^歌单$/ }).filter({ visible: true }).first().click();
    await page.locator(".playlist-card").filter({ hasText: "普通测试歌单" }).click();
    await page.locator(".playlist-detail .song-row").last().waitFor();
    const localButton = page.locator(".playlist-detail .song-row").filter({ hasText: "测试歌曲 local-1" }).getByRole("button", { name: "加入播放队列", exact: true });
    await appendFrom("local playlist song plus", localButton, "local-1");

    await page.locator(".nav-item--search").click();
    await page.getByRole("searchbox", { name: "搜索歌曲、艺人或音乐链接" }).fill("测试追加歌曲");
    await page.getByRole("searchbox", { name: "搜索歌曲、艺人或音乐链接" }).press("Enter");
    await page.locator(".song-list .song-row").filter({ hasText: "测试歌曲 search-append" }).waitFor();
    const searchButton = page.locator(".song-list .song-row").filter({ hasText: "测试歌曲 search-append" }).getByRole("button", { name: "加入播放队列", exact: true });
    await appendFrom("search result song plus", searchButton, "search-append");
    check(scenario, "all append surfaces block unstubbed remote requests and have no browser errors", log.unexpected.length === 0 && log.errors.length === 0, log);
  } catch (error) {
    check(scenario, "single-song append scenario completes", false, { error: error.stack, snapshot: await snapshot(page), log });
    const screenshot = `/tmp/music-playlist-detail-${engine}-${scenario}-failure.png`;
    await page.screenshot({ path: screenshot }).catch(() => {}); screenshots.push(screenshot);
  } finally { slowRelease(); await context.close(); }
}

try {
  let base = process.env.MUSIC_WEB_ORIGIN;
  if (!base) {
    server = http.createServer(async (request, response) => {
      const pathname = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
      const file = path.resolve(web, `.${pathname === "/" ? "/index.html" : pathname}`);
      if (!file.startsWith(`${web}/`)) { response.writeHead(403).end(); return; }
      try {
        const body = await fs.readFile(file);
        const type = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".png": "image/png", ".ico": "image/x-icon", ".webmanifest": "application/manifest+json" }[path.extname(file)] || "application/octet-stream";
        response.writeHead(200, { "content-type": type, "cache-control": "no-store" }).end(body);
      } catch { response.writeHead(404).end(); }
    });
    await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
    base = `http://127.0.0.1:${server.address().port}`;
  }
  browser = await playwright[engine].launch({ headless: true, ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
  for (const viewport of [{ width: 390, height: 844 }, { width: 1280, height: 900 }]) {
    const scenario = `${viewport.width}x${viewport.height}`;
    const { page, context, log, slowRelease } = await createFixture(base, viewport);
    try {
      const initial = await snapshot(page);
      const returnScroll = await openCard(page, "测试专属歌单");
      await page.locator(".playlist-detail .song-row").last().waitFor();
      const opened = await snapshot(page);
      check(scenario, "opening a recommendation only enters its detail", opened.activeView === "recommendation" && JSON.stringify(opened.ids) === JSON.stringify(initial.ids) && opened.current === initial.current && log.posts.length === 0, { initial, opened, posts: log.posts });
      check(scenario, "all 57 collection tracks are loaded without a 50-track limit", await page.locator(".playlist-detail .song-row").count() === 57 && log.resolves[0]?.limit === null, log.resolves);
      check(scenario, "playlist detail sends the exact privacy-safe canonical URL", log.resolves[0]?.platform === "applemusic" && log.resolves[0]?.collection === "https://music.apple.com/cn/playlist/pl.synthetic-long" && log.resolves[0]?.method === "GET", log.resolves[0]);
      check(scenario, "three explicit playlist actions are shown", await page.locator(".playlist-detail__actions > button").count() === 3);
      await inspectLayout(page, scenario);
      // A trusted user click starts the synthetic current queue item.
      await page.locator(".player-bar .player-controls button[title='播放']").click();
      await page.waitForFunction(() => { const audio = document.querySelector("audio"); return audio && !audio.paused && audio.currentTime > .2; });
      await page.waitForTimeout(350);
      await page.locator(".player-bar button[title='队列']").click();
      await page.locator(".queue-panel:not(.queue-panel--embedded) button[title='关闭']").click();
      await settle(page);
      check(scenario, "closing the queue drawer retains recommendation detail", (await snapshot(page)).activeView === "recommendation" && await page.locator(".playlist-detail").count() === 1);
      await page.locator(".player-bar .track-info").click();
      await page.locator(".full-player").waitFor();
      await page.locator(".full-player button[title='队列']").click();
      await page.locator(".queue-panel--full button[title='关闭']").click();
      await settle(page);
      check(scenario, "closing full-player queue retains the full player", await page.locator(".full-player").count() === 1);
      await page.goBack();
      await page.locator(".full-player").waitFor({ state: "detached" });
      await settle(page);
      check(scenario, "browser Back closes the full player without leaving detail", await page.locator(".full-player").count() === 0 && (await snapshot(page)).activeView === "recommendation");
      const playing = await snapshot(page);
      await page.locator(".playlist-detail__actions > button").filter({ hasText: "加入队列" }).click();
      await settle(page);
      const appended = await snapshot(page);
      const expectedAppend = [...originalIds, ...recommendationIds.filter(id => !originalIds.includes(id))];
      check(scenario, "append retains the old queue and adds all new songs at its end", JSON.stringify(appended.ids) === JSON.stringify(expectedAppend), appended.ids);
      check(scenario, "append does not interrupt the active audio", appended.current === "current" && appended.audioSrc === playing.audioSrc && !appended.audioPaused && appended.audioTime >= playing.audioTime && appended.events.filter(event => ["pause", "emptied", "loadstart"].includes(event.type)).length === playing.events.filter(event => ["pause", "emptied", "loadstart"].includes(event.type)).length, { playing, appended });
      await page.locator(".playlist-detail__actions > button").filter({ hasText: "下一首" }).click();
      await settle(page);
      const next = await snapshot(page);
      const expectedNext = ["current", ...recommendationIds.filter(id => id !== "current"), "keep", "tail"];
      check(scenario, "next moves every overlap behind the active song in collection order", JSON.stringify(next.ids) === JSON.stringify(expectedNext) && next.playIndex === 0, next.ids);
      check(scenario, "next preserves the current transport and advancing position", next.audioSrc === appended.audioSrc && !next.audioPaused && next.audioTime >= appended.audioTime && next.events.filter(event => ["pause", "emptied", "loadstart"].includes(event.type)).length === appended.events.filter(event => ["pause", "emptied", "loadstart"].includes(event.type)).length, { appended, next });
      if (viewport.width >= 880) {
        await page.locator(".player-bar button[title^='播放顺序：']").click();
        await page.locator(".player-bar .play-mode-option").filter({ hasText: "随机播放" }).click();
      }
      await page.locator(".playlist-detail__action--primary").click();
      await page.waitForFunction(() => { const audio = document.querySelector("audio"); return audio && !audio.paused && audio.currentTime > .15; });
      await settle(page);
      const replaced = await snapshot(page);
      check(scenario, "replace removes old songs and starts the shuffled collection", replaced.ids.length === 57 && [...replaced.ids].sort().join() === [...recommendationIds].sort().join() && replaced.shuffle === "on" && replaced.current !== recommendationIds[0] && replaced.playIndex === 0 && replaced.state === "playing", replaced);
      check(scenario, "shuffle retains the collection's original order", JSON.stringify(replaced.originalIds) === JSON.stringify(recommendationIds), replaced.originalIds);
      await page.locator(".playlist-detail .page-back").click();
      await page.locator(".artwork-rail--featured .artwork-card").first().waitFor();
      await settle(page);
      const returned = await page.evaluate(() => ({ scroll: window.scrollY, focus: document.activeElement?.dataset?.recommendationId }));
      check(scenario, "return restores the original recommendation position and focus", Math.abs(returned.scroll - returnScroll) <= 3 && returned.focus === "pl.synthetic-long", { returned, returnScroll });
      await openCard(page, "测试推荐专辑");
      await page.locator(".playlist-detail .song-row").last().waitFor();
      check(scenario, "recommended albums use the same detail and actions", await page.locator(".playlist-detail .song-row").count() === 3 && await page.locator(".playlist-detail__actions > button").count() === 3);
      check(scenario, "album detail preserves the full canonical URL and numeric catalog ID", log.resolves.at(-1)?.collection === "https://music.apple.com/cn/album/1624000713" && log.resolves.at(-1)?.platform === "applemusic" && log.resolves.at(-1)?.limit === null, log.resolves.at(-1));
      await page.locator(".sidebar .nav-item").filter({ hasText: /^资料库$|^歌单$/ }).filter({ visible: true }).first().click();
      await page.locator(".playlist-card").filter({ hasText: "普通测试歌单" }).click();
      await page.locator(".playlist-detail .song-row").last().waitFor();
      check(scenario, "ordinary playlists share all three explicit actions", await page.locator(".playlist-detail__actions > button").count() === 3 && await page.locator(".playlist-detail .song-row").count() === 3);
      await page.locator(".sidebar .nav-item").filter({ hasText: "主页" }).click();
      await openCard(page, "失败后重试");
      await page.getByRole("alert").filter({ hasText: "暂时无法打开" }).waitFor();
      check(scenario, "failed collection load has disabled actions and an explicit retry", await page.locator(".playlist-detail__actions > button:disabled").count() === 3);
      await page.getByRole("button", { name: "重新加载" }).click();
      await page.locator(".playlist-detail .song-row").last().waitFor();
      check(scenario, "retry loads the collection and enables actions", await page.locator(".playlist-detail .song-row").count() === 2 && await page.locator(".playlist-detail__actions > button:disabled").count() === 0);
      await page.locator(".playlist-detail .page-back").click();
      await page.locator(".artwork-rail--featured .artwork-card").first().waitFor();
      await openCard(page, "延迟返回");
      await page.getByRole("status", { name: "正在加载歌单曲目" }).waitFor();
      const pending = await snapshot(page);
      await page.locator(".playlist-detail .page-back").click();
      await page.locator(".artwork-rail--featured .artwork-card").first().waitFor();
      await openCard(page, "测试推荐专辑");
      await page.locator(".playlist-detail .song-row").last().waitFor();
      slowRelease(); await page.waitForTimeout(450);
      const afterLate = await snapshot(page);
      check(scenario, "late response cannot overwrite a newer detail or mutate the queue", (await page.locator(".playlist-detail h1").textContent()) === "测试推荐专辑" && await page.locator(".playlist-detail .song-row").count() === 3 && JSON.stringify(afterLate.ids) === JSON.stringify(pending.ids));
      check(scenario, "all collection requests satisfy the strict canonical URL contract", log.resolves.length >= 6 && log.rejectedResolves.length === 0 && log.resolves.every(item => item.platform === "applemusic" && item.limit === null && cards.some(card => card.url === item.collection)), log.resolves);
      check(scenario, "no account/provider traffic or uncaught browser errors", log.unexpected.length === 0 && log.errors.length === 0, log);
    } catch (error) {
      check(scenario, "scenario completes", false, { error: error.stack, snapshot: await snapshot(page), log });
      const screenshot = `/tmp/music-playlist-detail-${engine}-${scenario}-failure.png`;
      await page.screenshot({ path: screenshot }).catch(() => {}); screenshots.push(screenshot);
    } finally { slowRelease(); await context.close(); }
    await verifySingleTrackAppend(base, viewport);
  }
} finally { await browser?.close(); if (server) await new Promise(resolve => server.close(resolve)); }
const failed = checks.filter(check => !check.passed);
console.log(JSON.stringify({ browser: engine, passed: checks.length - failed.length, failed: failed.length, screenshots, checks }, null, 2));
if (failed.length) process.exitCode = 1;
