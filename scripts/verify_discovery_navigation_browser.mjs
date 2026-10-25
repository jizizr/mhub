/** Static-production discovery navigation regression; all API data is synthetic. */
import fs from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const web = path.resolve(process.env.MUSIC_WEB_DIR || path.join(root, "web"));
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const engine = process.env.BROWSER || "chromium";
const checks = [];
const check = (name, passed, details) => checks.push({ name, passed: Boolean(passed), ...(!passed && details ? { details } : {}) });
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
const server = http.createServer(async (req, res) => {
  const pathname = decodeURIComponent(new URL(req.url, "http://localhost").pathname);
  let file = path.resolve(web, `.${pathname === "/" ? "/index.html" : pathname}`);
  if (!file.startsWith(`${web}/`)) { res.writeHead(403).end(); return; }
  let body;
  try { body = await fs.readFile(file); } catch { file = path.join(web, "index.html"); body = await fs.readFile(file); }
  const mime = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".wasm": "application/wasm" }[path.extname(file)] || "application/octet-stream";
  res.writeHead(200, { "content-type": mime, "cache-control": "no-store" }).end(body);
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const base = process.env.MUSIC_WEB_ORIGIN || `http://127.0.0.1:${server.address().port}`;
const browser = await playwright[engine].launch({ headless: true, ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
const pool = Array.from({ length: 400 }, (_, index) => ({ id: String(10000 + index), platform: "applemusic", song: `推荐样本 ${index + 1}`, singer: `测试艺人 ${index % 13}`, cover: "" }));

try {
  for (const viewport of [{ width: 390, height: 844 }, { width: 1280, height: 900 }]) {
    const context = await browser.newContext({ viewport, hasTouch: viewport.width < 880, serviceWorkers: "block" });
    const page = await context.newPage();
    const state = { homes: [], searches: [], unexpected: [], errors: [], failedHome: false, emptyHome: false, holdSearch: null };
    const name = text => `${viewport.width}x${viewport.height}: ${text}`;
    page.on("pageerror", error => state.errors.push(error.message));
    await page.route("**/*", async route => {
      const url = new URL(route.request().url());
      const json = (body, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) }).catch(() => {});
      if (url.pathname.startsWith("/api/")) {
        if (url.pathname.endsWith("/platforms")) return json([{ id: "applemusic", label: "Apple Music" }, { id: "tencent", label: "QQ音乐" }]);
        if (url.pathname.endsWith("/recommendations/home")) {
          state.homes.push(Object.fromEntries(url.searchParams));
          if (state.failedHome) return json({ detail: "合成推荐服务暂时不可用" }, 503);
          if (state.emptyHome) return json({ tracks: [], playlists: [], tags: [] });
          const excluded = new Set((url.searchParams.get("exclude") || "").split(","));
          const playlists = Array.from({ length: 8 }, (_, index) => ({ id: `pl.synthetic${index}`, name: `公开推荐歌单 ${index + 1}`, resource_type: "playlists", cover: "", curator: "Apple Music" }));
          playlists.push({ id: "pl.u-fixture", name: "不应显示的合成私人列表", resource_type: "playlists" });
          return json({ tracks: pool.filter(track => !excluded.has(track.id)).slice(0, 40), playlists, tags: [{ id: "focus", label: "专注", playlist_ids: ["pl.synthetic0"] }] });
        }
        if (url.pathname.endsWith("/search")) {
          const params = Object.fromEntries(url.searchParams); state.searches.push(params);
          if (params.q === "slow" && params.platform === "applemusic" && state.holdSearch) await state.holdSearch.promise;
          return json([{ id: `${params.platform}-synthetic`, platform: params.platform, song: `搜索结果 ${params.q}`, singer: params.platform, cover: "" }]);
        }
        if (url.pathname.endsWith("/collections/resolve")) return json({ items: pool.slice(0, 16) });
        if (url.pathname === "/api/v1/auth/config") return json({ enabled: false });
        state.unexpected.push(url.pathname); return json({ detail: "Unstubbed API blocked" }, 500);
      }
      if (url.origin === new URL(base).origin || ["data:", "blob:"].includes(url.protocol)) return route.continue();
      state.unexpected.push(url.origin + url.pathname); return route.abort();
    });
    await page.addInitScript(() => {
      if (!localStorage.getItem("discovery-fixture-ready")) {
        localStorage.setItem("music-web-splayer-status-v3", JSON.stringify({ activeView: "discover", quality: "high", searchPlatform: "applemusic", repeatMode: "off" }));
        localStorage.setItem("discovery-fixture-ready", "true");
      }
    });
    const feed = page.locator(".recommend-feed");
    const input = page.getByRole("searchbox", { name: "搜索歌曲、艺人或音乐链接" });
    const titles = () => page.locator(".recommend-list-row__text strong").allTextContents();
    const settlePage = () => page.waitForFunction(() => {
      const section = document.querySelector(".content");
      return section && getComputedStyle(section).opacity === "1" && getComputedStyle(section).transform === "none" && !section.className.includes("page-enter");
    });
    const home = async () => { await feed.waitFor({ state: "visible" }); await page.waitForFunction(() => document.querySelectorAll(".recommend-list-row").length === 12); };
    const openSearch = async () => { await page.locator(".nav-item--search").click(); await input.waitFor(); };
    const goHome = async () => { await page.locator(".nav-item--discover").click(); await home(); };
    const openQueue = async () => {
      if (viewport.width >= 880) await page.locator(".sidebar__section").getByRole("button", { name: "播放队列" }).click();
      else { await page.locator(".nav-item--library").click(); await page.locator(".library-links").getByRole("button", { name: "播放队列" }).click(); }
      await page.locator(".queue-panel--embedded").waitFor();
    };
    const search = async (term = "导航测试", { wait = true } = {}) => { await openSearch(); await input.fill(term); await input.press("Enter"); if (wait) await page.locator(".song-row").waitFor(); };
    const refresh = async () => { const before = state.homes.length; await page.locator(".home-actions").getByRole("button", { name: "换一批" }).click(); await page.waitForFunction(() => !document.querySelector(".home-actions button")?.disabled); check(name("refresh makes a new request"), state.homes.length === before + 1); };
    try {
      await page.goto(base, { waitUntil: "domcontentloaded" }); await home();
      const original = await titles();
      check(name("homepage has 12 songs and all public cards"), original.length === 12 && await page.locator(".artwork-rail--featured .artwork-card").count() === 8);
      check(name("private playlists are filtered and no keyword search runs"), !(await page.locator("body").textContent()).includes("不应显示的合成私人列表") && state.searches.length === 0);
      await refresh();
      let changed = await titles();
      check(name("refresh excludes the 12 displayed songs and changes the batch"), state.homes.at(-1).refresh === "true" && state.homes.at(-1).exclude?.split(",").length === 12 && !changed.some(title => original.includes(title)));
      state.emptyHome = true; await refresh();
      check(name("empty refresh preserves songs and cards"), JSON.stringify(await titles()) === JSON.stringify(changed) && await page.locator(".artwork-rail--featured .artwork-card").count() === 8 && await feed.locator(".error-banner").count() === 1);
      state.emptyHome = false; state.failedHome = true; await refresh();
      check(name("failed refresh preserves content without keyword fallback"), JSON.stringify(await titles()) === JSON.stringify(changed) && state.searches.length === 0 && (await feed.locator(".error-banner").textContent()).includes("合成推荐服务"));
      check(name("only the next explicit refresh narrows exhausted exclusions"), state.homes.at(-1).exclude?.split(",").length === 12);
      state.failedHome = false;
      await refresh();
      const recovered = await titles();
      check(name("exhausted pool recovers older songs without repeating current batch"), recovered.length === 12 && !recovered.some(title => changed.includes(title)) && recovered.some(title => original.includes(title)));
      changed = recovered;
      const homeCount = state.homes.length;
      await page.locator(".listening-settings__trigger:visible").first().click();
      await page.getByLabel("音质").selectOption("hires");
      await page.keyboard.press("Escape");
      check(name("changing audio quality does not replace recommendation batch"), state.homes.length === homeCount && JSON.stringify(await titles()) === JSON.stringify(changed));
      await search();
      check(name("search tab shows results without touching the feed"), await page.locator(".song-row").count() === 1 && !await feed.count());
      await settlePage();
      await page.screenshot({ path: `/tmp/music-discovery-${engine}-${viewport.width}-search.png`, fullPage: true });
      await goHome();
      check(name("home tab restores the feed without refetch"), state.homes.length === homeCount && JSON.stringify(await titles()) === JSON.stringify(changed));
      await openSearch();
      check(name("search tab keeps its query and results"), await input.inputValue() === "导航测试" && await page.locator(".song-row").count() === 1);
      await page.getByRole("button", { name: "清空搜索" }).click();
      check(name("clear button empties query and results"), await input.inputValue() === "" && !await page.locator(".song-row").count());
      await search(); await input.fill(""); await page.locator(".search-hint").waitFor();
      check(name("deleting search input clears results"), !await page.locator(".song-row").count());
      await search(); await input.press("Escape"); await page.locator(".search-hint").waitFor();
      check(name("Escape clears the search"), await input.inputValue() === "");
      state.holdSearch = deferred(); await search("slow", { wait: false });
      await page.locator(".nav-item--discover").click(); state.holdSearch.resolve(); state.holdSearch = null; await home();
      await page.waitForTimeout(300);
      check(name("late search does not pull the view away from home"), await feed.isVisible());
      state.holdSearch = deferred(); await search("slow", { wait: false });
      await openQueue(); state.holdSearch.resolve(); state.holdSearch = null;
      await page.waitForTimeout(300);
      check(name("late search does not leave the queue page"), (await page.locator(".content h1").textContent()) === "播放队列" && !await feed.count());
      const toggle = page.locator(".queue-panel--embedded .queue-autoplay__toggle");
      await toggle.click();
      check(name("autoplay switch works with empty queue without music requests"), await toggle.getAttribute("aria-pressed") === "true");
      await goHome();
      check(name("home navigation always opens recommendations"), await feed.isVisible());
      if (viewport.width >= 880) {
        state.holdSearch = deferred(); await search("slow", { wait: false });
        await page.locator(".search-scopes").getByRole("radio", { name: "QQ音乐" }).click();
        await page.waitForFunction(() => document.querySelector(".song-row__artist")?.textContent === "tencent");
        state.holdSearch.resolve(); state.holdSearch = null;
        check(name("platform switch replaces pending search without stale results"), await page.locator(".song-row__artist").textContent() === "tencent");
        await page.locator(".search-scopes").getByRole("radio", { name: "Apple Music" }).click();
        await goHome();
      }
      await page.locator(".artwork-rail--featured .artwork-card").first().click();
      await page.waitForFunction(() => document.querySelectorAll(".song-row").length === 16);
      check(name("recommendation card still opens full detail without playback"), (await page.locator(".content h1").textContent()).includes("公开推荐歌单") && state.unexpected.length === 0);
      await page.locator(".page-back").click(); await home();
      await page.reload({ waitUntil: "domcontentloaded" }); await home();
      check(name("reload reuses seen-song exclusions"), (state.homes.at(-1).exclude || "").split(",").filter(Boolean).length >= 24);
      await openQueue();
      check(name("autoplay preference survives navigation and reload"), await page.locator(".queue-panel--embedded .queue-autoplay__toggle").getAttribute("aria-pressed") === "true");
      check(name("no uncaught errors or unexpected API/account requests"), state.errors.length === 0 && state.unexpected.length === 0, { errors: state.errors, unexpected: state.unexpected });
      await settlePage();
      await page.screenshot({ path: `/tmp/music-discovery-${engine}-${viewport.width}.png`, fullPage: true });
    } catch (error) {
      check(name("scenario completes"), false, { message: error.message, errors: state.errors, unexpected: state.unexpected });
    } finally {
      state.holdSearch?.resolve();
      await context.close();
    }
  }
} finally {
  await browser.close(); await new Promise(resolve => server.close(resolve));
}
const report = { engine, passed: checks.filter(item => item.passed).length, total: checks.length, checks };
console.log(JSON.stringify(report, null, 2));
if (checks.some(item => !item.passed)) process.exitCode = 1;
