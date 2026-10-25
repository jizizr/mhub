/** Local production-build startup/lazy-player regression; every API is synthetic. */
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const web = path.resolve(process.env.MUSIC_WEB_DIR || path.join(root, "web"));
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const checks = [];
const check = (name, value) => { assert.ok(value, name); checks.push({ name, passed: true }); };
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};
const server = http.createServer(async (req, res) => {
  const pathname = new URL(req.url, "http://localhost").pathname;
  const file = path.resolve(web, `.${pathname === "/" ? "/index.html" : pathname}`);
  if (!file.startsWith(`${web}/`)) { res.writeHead(403).end(); return; }
  try {
    const body = await fs.readFile(file);
    const mime = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".wasm": "application/wasm" }[path.extname(file)] || "application/octet-stream";
    res.writeHead(200, { "content-type": mime, "cache-control": "no-store" }).end(body);
  } catch { res.writeHead(404).end(); }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({ headless: true,
  ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
try {
  for (const viewport of [{ width: 1365, height: 900 }, { width: 390, height: 844 }]) {
    const context = await browser.newContext({ viewport, serviceWorkers: "block", reducedMotion: "no-preference" });
    const page = await context.newPage();
    const platformGate = deferred();
    const parserGate = deferred();
    const state = { platforms: 0, homes: 0, lyrics: 0, requests: [], errors: [], unexpected: [] };
    const name = (text) => `${viewport.width}: ${text}`;
    page.on("pageerror", (error) => state.errors.push(error.message));
    await page.route("**/*", async (route) => {
      const url = new URL(route.request().url());
      state.requests.push(url.pathname);
      const json = (body) => route.fulfill({ contentType: "application/json", body: JSON.stringify(body) }).catch(() => {});
      if (url.pathname === "/api/v1/platforms") {
        state.platforms += 1;
        await platformGate.promise;
        return json([{ id: "applemusic", label: "Apple Music" }]);
      }
      if (url.pathname === "/api/v1/recommendations/home") {
        state.homes += 1;
        return json({ tracks: [{ id: "100", platform: "applemusic", song: "合成首屏样本", singer: "合成艺人", cover: "" }], playlists: [], tags: [] });
      }
      if (url.pathname === "/api/v1/auth/config") return json({ enabled: false });
      if (url.pathname === "/api/v1/lyrics") {
        state.lyrics += 1;
        return json({ available: false, plain: "", translation: "" });
      }
      if (url.pathname.startsWith("/assets/lyrics-")) await parserGate.promise;
      if (url.origin === origin && !url.pathname.startsWith("/api/")) return route.continue();
      state.unexpected.push(url.origin + url.pathname);
      return route.abort();
    });
    await page.addInitScript(() => {
      const track = { id: "applemusic--100--high", source_id: "100", platform: "applemusic", quality: "high", title: "合成当前曲目", artist: "合成艺人", duration: 180000 };
      localStorage.setItem("music-web-splayer-status-v3", JSON.stringify({ activeView: "discover", quality: "high", searchPlatform: "applemusic", playIndex: 0 }));
      localStorage.setItem("music-web-splayer-queue-v3", JSON.stringify({ queue: [track] }));
      window.__playerListeners = { added: 0, removed: 0 };
      const matchMedia = window.matchMedia.bind(window);
      window.matchMedia = (query) => {
        const media = matchMedia(query);
        const add = media.addEventListener.bind(media);
        const remove = media.removeEventListener.bind(media);
        media.addEventListener = (...args) => { if (query === "(max-width: 880px)") window.__playerListeners.added += 1; return add(...args); };
        media.removeEventListener = (...args) => { if (query === "(max-width: 880px)") window.__playerListeners.removed += 1; return remove(...args); };
        return media;
      };
      window.__playerTransitions = { enter: false, leave: false };
      new MutationObserver((records) => {
        for (const record of records) {
          const classes = `${record.oldValue || ""} ${record.target.className || ""}`;
          if (classes.includes("full-player-enter-active")) window.__playerTransitions.enter = true;
          if (classes.includes("full-player-leave-active")) window.__playerTransitions.leave = true;
        }
      }).observe(document, { subtree: true, attributes: true, attributeOldValue: true, attributeFilter: ["class"] });
    });
    try {
      await page.goto(origin, { waitUntil: "domcontentloaded" });
      await page.locator(".recommend-list-row__text strong").waitFor({ timeout: 5000 });
      check(name("recommendations render while platform GET is unresolved"), state.homes === 1 && state.platforms === 1 && !(await page.locator(".nav-item--search").isDisabled()));
      check(name("first screen does not request FullPlayer or lyric parser chunks"), !state.requests.some((value) => /\/assets\/(FullPlayer|lyrics)-/.test(value)));
      check(name("first screen makes no lyric request"), state.lyrics === 0);
      platformGate.resolve();
      await page.locator(".nav-item--search").click();
      await page.locator(".search-scopes button").first().waitFor({ state: "attached" });
      await page.locator(".nav-item--discover").click();
      await page.locator(".track-info").click();
      await page.locator(".full-player").waitFor();
      await page.waitForFunction(() => window.__playerListeners.added === 1);
      await page.waitForFunction(() => window.__playerTransitions.enter);
      check(name("first opening has an enter transition"), await page.evaluate(() => window.__playerTransitions.enter));
      // The parser response is deliberately held. The lyrics API must already
      // be in flight, rather than start after this optional JavaScript arrives.
      await page.waitForFunction(() => performance.getEntriesByType("resource").some((item) => item.name.includes("/api/v1/lyrics?")));
      check(name("lyric API starts while parser download is held"), state.lyrics === 1 && state.requests.some((value) => value.startsWith("/assets/lyrics-")));
      parserGate.resolve();
      await page.getByText("暂无歌词", { exact: true }).waitFor();
      await page.locator(".full-player .collapse-button").click();
      await page.locator(".full-player").waitFor({ state: "detached" });
      check(name("closing runs the leave transition"), await page.evaluate(() => window.__playerTransitions.leave));
      check(name("closing retains the FullPlayer instance"), await page.evaluate(() => window.__playerListeners.added === 1 && window.__playerListeners.removed === 0));
      await page.locator(".track-info").click();
      await page.locator(".full-player").waitFor();
      check(name("reopening reuses the same instance, chunks and lyric result"), await page.evaluate(() => window.__playerListeners.added === 1) && state.requests.filter((value) => /\/assets\/FullPlayer-.*\.js$/.test(value)).length === 1 && state.lyrics === 1);
      check(name("no uncaught errors or unstubbed remote/API requests"), state.errors.length === 0 && state.unexpected.length === 0);
    } finally {
      platformGate.resolve(); parserGate.resolve();
      await context.close();
    }
  }
} finally {
  await browser.close();
  await new Promise((resolve) => server.close(resolve));
}
console.log(JSON.stringify({ browser: "installed Edge when BROWSER_EXECUTABLE is set", passed: checks.length, checks }, null, 2));
