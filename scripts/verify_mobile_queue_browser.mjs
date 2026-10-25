// Exercise the real Vue app with synthetic queue data and stubbed provider APIs.
// PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs BROWSER=webkit node scripts/verify_mobile_queue_browser.mjs
// BROWSER_EXECUTABLE optionally selects Chromium/Edge. BASELINE_CSS can replay an
// earlier stylesheet against the same app to demonstrate the layout regression.
// MUSIC_WEB_ORIGIN optionally verifies a deployed build with the same isolated
// synthetic queue and stubbed APIs; it never changes the user's browser data.
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const { createServer } = await import(pathToFileURL(path.join(root, "frontend/node_modules/vite/dist/node/index.js")));
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const engine = process.env.BROWSER || "chromium";
assert.ok(["chromium", "webkit"].includes(engine), "BROWSER must be chromium or webkit");
const baselineCss = process.env.BASELINE_CSS ? await fs.readFile(process.env.BASELINE_CSS, "utf8") : null;
const server = await createServer({ root: path.join(root, "frontend"), server: { host: "127.0.0.1", port: 0 } });
const results = [];
let browser;

function check(scenario, name, passed, details) {
  results.push({ scenario, name, passed: Boolean(passed), ...(!passed ? { details } : {}) });
}

function syntheticTracks(count) {
  return Array.from({ length: count }, (_, index) => ({
    id: `test--queue-${index + 1}--high`, source_id: `queue-${index + 1}`,
    platform: "test", title: `测试歌曲 ${String(index + 1).padStart(2, "0")}`,
    artist: `测试艺人 ${index % 5 + 1}`, quality: "high", duration: 180000,
    cover: `data:image/svg+xml,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><rect width="64" height="64" rx="10" fill="hsl(${index * 37 % 360} 50% 68%)"/><text x="32" y="40" text-anchor="middle" font-size="24" fill="white">${index + 1}</text></svg>`)}`,
  }));
}

async function settle(page) {
  // Keep actual transitions enabled: the bug concerns painting/stacking too.
  await page.waitForTimeout(420);
}

async function inspectDock(page, scenario, full = false) {
  const state = await page.evaluate(({ full }) => {
    const footer = document.querySelector(full ? ".full-player__bottom" : ".player-bar");
    const nav = document.querySelector(".sidebar");
    const rect = footer.getBoundingClientRect();
    const progressRect = full ? footer.querySelector(".full-player__progress").getBoundingClientRect() : null;
    const footerHit = document.elementFromPoint(Math.max(1, rect.left + rect.width / 2), rect.top + rect.height / 2);
    const buttons = [...footer.querySelectorAll(".player-controls > button, .play-mode-control > button")];
    const controls = buttons.filter(button => button.getClientRects().length > 0).map(button => {
      const box = button.getBoundingClientRect();
      const x = box.left + box.width / 2, y = box.top + box.height / 2;
      const hit = document.elementFromPoint(x, y);
      return { title: button.title, pressed: button.getAttribute("aria-pressed"), visible: x >= 0 && x < innerWidth && y >= 0 && y < innerHeight, hit: button.contains(hit) };
    });
    const navBox = nav.getBoundingClientRect();
    const footerStyle = getComputedStyle(footer);
    const navStyle = getComputedStyle(nav);
    const navHits = full ? [] : [...nav.querySelectorAll(".nav-item")].map(button => {
      const box = button.getBoundingClientRect();
      return button.contains(document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2));
    });
    return {
      footerHit: footer.contains(footerHit), hitClass: footerHit?.className,
      controls, navHits, viewportWidth: innerWidth,
      titleWidth: document.querySelector(".player-bar .track-info__text").getBoundingClientRect().width,
      footerInside: rect.top >= 0 && rect.bottom <= innerHeight + 1,
      mobileSeparation: innerWidth > 880 || full || rect.bottom <= navBox.top + 1,
      noHorizontalOverflow: document.documentElement.scrollWidth <= innerWidth + 1,
      floating: rect.left >= 12 && rect.right <= innerWidth - 12 && navBox.left >= 12 && navBox.right <= innerWidth - 12
        && parseFloat(footerStyle.borderRadius) >= 20 && parseFloat(navStyle.borderRadius) >= 24,
      compact: Math.abs(rect.height - 56) < 1 && Math.abs(navBox.height - 52) < 1,
      dockGap: navBox.top - rect.bottom,
      navBottomGap: innerHeight - navBox.bottom,
      expectedBottomGap: parseFloat(getComputedStyle(document.querySelector('.app')).getPropertyValue('--mobile-dock-bottom')),
      visibleSeek: [...footer.querySelectorAll('input[type="range"]')].some(input => input.getClientRects().length > 0),
      progressInside: !progressRect || (progressRect.left >= 0 && progressRect.right <= innerWidth + 1 && progressRect.top >= rect.top && progressRect.bottom <= innerHeight),
    };
  }, { full });
  check(scenario, "dock owns its hit-test area", state.footerHit && state.footerInside, state);
  check(scenario, "transport buttons remain visible and clickable", state.controls.every(item => item.visible && item.hit), state.controls);
  if (full) {
    check(scenario, "full player progress is fully inside the visible toolbar", state.progressInside, state);
    const titles = state.controls.map(item => item.title);
    check(scenario, "full player retains playback mode, previous, play/pause, next, lyrics and queue controls", [
      /^播放顺序：/, /^上一首$/, /^(播放|暂停|正在缓冲，点击暂停)$/, /^下一首$/, /^歌词$/, /^队列$/,
    ].every(pattern => titles.some(title => pattern.test(title))), state.controls);
    check(scenario, "full player retains an accessible automatic continuation toggle", state.controls.some(item =>
      item.title.includes("自动续播") && ["true", "false"].includes(item.pressed) && item.visible && item.hit), state.controls);
  }
  if (!full && state.viewportWidth <= 390) {
    check(scenario, "compact player retains at least 80px for track text", state.titleWidth >= 80, { width: state.titleWidth });
    check(scenario, "compact player shows play and next; queue remains in navigation", [/^(播放|暂停|正在缓冲，点击暂停)$/, /^下一首$/].every(pattern => state.controls.some(item => pattern.test(item.title))), state.controls);
  }
  check(scenario, "navigation remains clickable; player and navigation do not overlap", state.navHits.every(Boolean) && state.mobileSeparation, state);
  check(scenario, "page has no horizontal overflow", state.noHorizontalOverflow, state);
  if (!full && state.viewportWidth <= 880) {
    check(scenario, "mobile player and navigation float as compact separate pills", state.floating && state.compact && state.dockGap >= 7, state);
    check(scenario, "bottom dock respects the safe area", state.navBottomGap >= 8 && (!Number.isFinite(state.expectedBottomGap) || state.navBottomGap >= state.expectedBottomGap - 1), state);
    check(scenario, "compact progress does not expose a native range over the feed", !state.visibleSeek, state);
  }
}

async function inspectQueuePage(page, scenario, { removeLast = false } = {}) {
  await page.evaluate(() => window.scrollTo(0, 0));
  await settle(page);
  const scroll = await page.locator(".queue-panel--embedded .queue-list").evaluate(list => ({
    nested: list.scrollHeight > list.clientHeight + 1 && /auto|scroll/.test(getComputedStyle(list).overflowY),
    docScrollable: document.scrollingElement.scrollHeight > innerHeight,
  }));
  check(scenario, "embedded queue uses document scrolling without a nested scroller", !scroll.nested && scroll.docScrollable, scroll);
  // A wheel over a row must move the document rather than trap scrolling in it.
  const listBounds = await page.locator(".queue-panel--embedded .queue-list").boundingBox();
  const footerBounds = await page.locator(".player-bar").boundingBox();
  const y = Math.min(Math.max(listBounds.y + 25, 30), footerBounds.y - 20);
  await page.mouse.move(Math.min(listBounds.x + 60, 140), y);
  await page.mouse.wheel(0, 240);
  await settle(page);
  check(scenario, "scrolling over the queue advances the document", await page.evaluate(() => scrollY > 0));
  await inspectDock(page, scenario);
  await page.evaluate(() => window.scrollTo(0, document.scrollingElement.scrollHeight));
  await settle(page);
  const last = await page.locator(".queue-panel--embedded .queue-item").last().boundingBox();
  const dock = await page.locator(".player-bar").boundingBox();
  check(scenario, "last row can be fully scrolled above the player", last.y >= 0 && last.y + last.height <= dock.y - 6, { last, dock });
  if (removeLast && last.y >= 0 && last.y + last.height <= dock.y - 6) {
    const before = await page.locator(".queue-panel--embedded .queue-item").count();
    await page.locator(".queue-panel--embedded .queue-item").last().locator(".queue-item__remove").click();
    // Inspect during the leave animation too, when a row is position:absolute.
    await inspectDock(page, `${scenario}:deleting`);
    await settle(page);
    check(scenario, "last row removal updates the queue", await page.locator(".queue-panel--embedded .queue-item").count() === before - 1);
    const remaining = await page.locator(".queue-panel--embedded .queue-item").last().boundingBox();
    const player = await page.locator(".player-bar").boundingBox();
    check(scenario, "remaining last row is reachable after removal", remaining.y >= 0 && remaining.y + remaining.height <= player.y - 6, { remaining, player });
  }
}

async function inspectOverlay(page, scenario, full = false) {
  const selector = full ? ".queue-panel--full" : ".queue-panel:not(.queue-panel--embedded)";
  await page.locator(selector).waitFor();
  await settle(page);
  const bounds = await page.locator(selector).boundingBox();
  const footer = await page.locator(full ? ".full-player__bottom" : ".player-bar").boundingBox();
  check(scenario, "queue overlay stays above the player controls", bounds.y >= 0 && bounds.y + bounds.height <= footer.y - 6, { bounds, footer });
  const body = page.locator(`${selector} .queue-panel__body`);
  await body.evaluate(element => { element.scrollTop = element.scrollHeight; });
  await settle(page);
  const last = await page.locator(`${selector} .queue-item`).last().boundingBox();
  const visible = await body.boundingBox();
  check(scenario, "overlay can scroll to its final track", last.y >= visible.y - 1 && last.y + last.height <= visible.y + visible.height + 1, { last, visible });
  const settings = page.locator(`${selector} .listening-settings__trigger`);
  await settings.click();
  check(scenario, "listening settings remain available inside the queue", await settings.getAttribute("aria-expanded") === "true");
  await body.evaluate(element => { element.scrollTop = element.scrollHeight; });
  await settle(page);
  const expandedLast = await page.locator(`${selector} .queue-item`).last().boundingBox();
  const expandedVisible = await body.boundingBox();
  check(scenario, "expanded listening settings do not prevent reaching the final track", expandedLast.y >= expandedVisible.y - 1 && expandedLast.y + expandedLast.height <= expandedVisible.y + expandedVisible.height + 1, { last: expandedLast, visible: expandedVisible });
  await inspectDock(page, scenario, full);
}

try {
  await server.listen();
  const base = process.env.MUSIC_WEB_ORIGIN || `http://127.0.0.1:${server.httpServer.address().port}`;
  browser = await playwright[engine].launch({ headless: true, ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
  for (const viewport of [{ width: 320, height: 568 }, { width: 390, height: 844 }, { width: 844, height: 390 }, { width: 1280, height: 900 }]) {
    const count = viewport.width === 390 ? 50 : 24;
    const scenario = `${viewport.width}x${viewport.height}/${count}`;
    const context = await browser.newContext({ viewport, deviceScaleFactor: 1, serviceWorkers: "block", hasTouch: viewport.width <= 880 });
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.route("**/api/**", route => {
      const url = new URL(route.request().url());
      let body = [];
      if (url.pathname.endsWith("/platforms")) body = [{ id: "applemusic", label: "Apple Music" }];
      else if (url.pathname.endsWith("/recommendations/home")) body = { tracks: [], playlists: [], tags: [] };
      else if (url.pathname.endsWith("/lyrics")) body = { lines: [] };
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
    });
    await page.addInitScript(({ tracks }) => {
      localStorage.setItem("music-web-splayer-queue-v3", JSON.stringify({ queue: tracks, sourcePlaylist: { id: "synthetic", name: "交互测试歌单" } }));
      localStorage.setItem("music-web-splayer-status-v3", JSON.stringify({ activeView: "queue", playIndex: 0, quality: "high" }));
    }, { tracks: syntheticTracks(count) });
    try {
      await page.goto(base);
      await page.locator(".queue-panel--embedded .queue-item").last().waitFor();
      if (baselineCss) await page.evaluate(css => {
        const style = [...document.querySelectorAll("style")].find(item => item.dataset.viteDevId?.endsWith("/src/styles.css"));
        if (!style) throw new Error("Vite application stylesheet was not found");
        style.textContent = css;
      }, baselineCss);
      await settle(page);
      await inspectQueuePage(page, scenario, { removeLast: true });
      if (viewport.width === 390) {
        await page.evaluate(() => window.scrollTo(0, 240));
        await settle(page);
        const screenshot = `/tmp/music-mobile-queue-${engine}-390${baselineCss ? "-before" : ""}.png`;
        await page.screenshot({ path: screenshot });
        console.log(JSON.stringify({ screenshot }));
        await page.setViewportSize({ width: 390, height: 640 });
        await inspectQueuePage(page, `${scenario}:shorter-viewport`);
        // Model the 34px bottom safe area without depending on host hardware.
        await page.evaluate(() => {
          document.documentElement.style.setProperty("--mobile-dock-bottom", "34px");
          document.documentElement.style.setProperty("--full-player-toolbar-height", "138px");
        });
        await inspectQueuePage(page, `${scenario}:safe-area-34`);
        await page.setViewportSize(viewport);
      }
      await page.locator(".sidebar .nav-item").first().click();
      await settle(page);
      // On phones the compact transport keeps just play/next. The queue tab
      // opens the queue page; its drawer shortcut is in the full player.
      if (viewport.width > 880) {
        await page.locator(".player-bar button[title='队列']").click();
        await inspectOverlay(page, `${scenario}:drawer`);
        await page.locator(".queue-panel:not(.queue-panel--embedded) button[title='关闭']").click();
        await settle(page);
      }
      await page.locator(".player-bar .track-info").click();
      await page.locator(".full-player").waitFor();
      await settle(page);
      await page.locator(".full-player button[title='队列']").click();
      await inspectOverlay(page, `${scenario}:full-player`, true);
      if (viewport.width === 320 || viewport.width === 844) {
        const screenshot = `/tmp/music-mobile-queue-${engine}-${viewport.width}x${viewport.height}-full.png`;
        await page.screenshot({ path: screenshot });
        console.log(JSON.stringify({ screenshot }));
      }
      if (viewport.width <= 880) {
        await page.locator(".full-player .collapse-button").click();
        await settle(page);
        await inspectOverlay(page, `${scenario}:collapsed-player-drawer`);
        await page.locator(".queue-panel:not(.queue-panel--embedded) button[title='关闭']").click();
        await settle(page);
        await page.locator(".sidebar .nav-item").nth(1).click();
        await page.locator(".queue-panel--embedded").waitFor();
        await settle(page);
        page.once("dialog", dialog => dialog.accept());
        await page.locator(".queue-panel--embedded button[title='清空队列']").click();
        await settle(page);
        const empty = await page.evaluate(() => {
          const app = document.querySelector('.app');
          const nav = document.querySelector('.sidebar').getBoundingClientRect();
          return {
            playerHidden: document.querySelector('.player-bar').getClientRects().length === 0,
            reserved: parseFloat(getComputedStyle(app).paddingBottom),
            actual: innerHeight - nav.top,
            navClickable: [...document.querySelectorAll('.sidebar .nav-item')].every(button => {
              const r = button.getBoundingClientRect();
              return button.contains(document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2));
            }),
          };
        });
        check(scenario, "clearing the last track removes the empty mini player and its reserved space", empty.playerHidden && empty.reserved <= empty.actual + 13 && empty.navClickable, empty);
      }
      check(scenario, "real app has no uncaught browser errors", errors.length === 0, errors);
    } catch (error) {
      check(scenario, "scenario completes", false, error.message);
    } finally {
      await context.close();
    }
  }
} finally {
  await browser?.close();
  await server.close();
}
const failed = results.filter(item => !item.passed);
console.log(JSON.stringify({ browser: engine, baseline: Boolean(baselineCss), passed: results.length - failed.length, failed: failed.length, checks: results }, null, 2));
if (failed.length) process.exitCode = 1;
