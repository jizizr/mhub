// Isolated real Vue app + Chrome/Edge touch input, with synthetic tracks and APIs.
// No live account, audio, browser profile, or production state is used.
// MUSIC_WEB_ORIGIN optionally verifies deployed assets with the same isolated
// fixtures. All API/health requests are stubbed; other origins are blocked.
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const { createServer } = await import(pathToFileURL(path.join(root, "frontend/node_modules/vite/dist/node/index.js")));
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const server = process.env.MUSIC_WEB_ORIGIN ? null : await createServer({ root: path.join(root, "frontend"), server: { host: "127.0.0.1", port: 0 } });
const tracks = Array.from({ length: 24 }, (_, i) => ({ id: `test--${i}--high`, source_id: String(i), platform: "test", title: `测试歌曲 ${i}`, artist: "测试艺人", quality: "high", duration: 180000 }));
const results = [];
let browser;
function check(name, passed, details) { results.push({ name, passed, ...(passed ? {} : { details }) }); assert.ok(passed, `${name}: ${JSON.stringify(details)}`); }
const settle = page => page.waitForTimeout(320);
const order = page => page.evaluate(() => JSON.parse(localStorage.getItem("music-web-splayer-queue-v3")).queue.map(track => track.source_id));
const current = page => page.evaluate(() => {
  const queue = JSON.parse(localStorage.getItem("music-web-splayer-queue-v3")).queue;
  const status = JSON.parse(localStorage.getItem("music-web-splayer-status-v3"));
  return queue[status.playIndex]?.source_id;
});
async function touch(client, type, x = 0, y = 0) {
  await client.send("Input.dispatchTouchEvent", { type, touchPoints: type === "touchEnd" || type === "touchCancel" ? [] : [{ x, y, id: 1 }] });
}
async function touchMove(client, page, x, y, endX, endY) {
  for (let step = 1; step <= 8; step++) {
    await touch(client, "touchMove", x + (endX - x) * step / 8, y + (endY - y) * step / 8);
    await page.waitForTimeout(20);
  }
}

try {
  await server?.listen();
  const base = process.env.MUSIC_WEB_ORIGIN || `http://127.0.0.1:${server.httpServer.address().port}`;
  const allowedOrigin = new URL(base).origin;
  browser = await chromium.launch({ headless: true, ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true, serviceWorkers: "block" });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.route("**/*", route => {
    const url = new URL(route.request().url());
    if (url.origin !== allowedOrigin) return route.abort("blockedbyclient");
    const { pathname } = url;
    if (pathname === "/health") return route.fulfill({ status: 200, contentType: "application/json", body: '{"status":"ok"}' });
    if (!pathname.startsWith("/api/")) return route.continue();
    const body = pathname.endsWith("/platforms") ? [{ id: "applemusic", label: "Apple Music" }]
      : pathname.endsWith("/recommendations/home") ? { tracks: [], playlists: [], tags: [] }
      : pathname.endsWith("/lyrics") ? { lines: [] } : [];
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.addInitScript(tracks => {
    localStorage.setItem("music-web-splayer-queue-v3", JSON.stringify({ queue: tracks }));
    localStorage.setItem("music-web-splayer-status-v3", JSON.stringify({ activeView: "queue", playIndex: 3, quality: "high" }));
  }, tracks);
  await page.goto(base);
  const selector = ".queue-panel--embedded";
  await page.locator(`${selector} .queue-item`).last().waitFor();
  await settle(page);
  const client = await context.newCDPSession(page);
  await page.locator(`${selector} .queue-item`).first().evaluate(el => el.scrollIntoView({ block: "start" }));
  const originalCurrent = await current(page);
  await page.evaluate(() => { window.queueNodes = new Map([...document.querySelectorAll(".queue-panel--embedded .queue-item")].map(el => [el.dataset.trackKey, el])); });
  const handle = await page.locator(`${selector} .queue-item__grip`).first().boundingBox();
  const fourth = await page.locator(`${selector} .queue-item`).nth(3).boundingBox();
  const x = handle.x + handle.width / 2, y = handle.y + handle.height / 2;
  const beforeTap = await order(page);
  await touch(client, "touchStart", x, y);
  await touch(client, "touchMove", x + 2, y + 2);
  await touch(client, "touchEnd");
  check("small handle motion does not accidentally reorder", JSON.stringify(await order(page)) === JSON.stringify(beforeTap));
  await touch(client, "touchStart", x, y);
  await touchMove(client, page, x, y, x, fourth.y + fourth.height / 2 + 8);
  check("real touch activates handle drag instead of browser scrolling", await page.locator(`${selector} .is-dragging`).count() === 1);
  await touch(client, "touchEnd");
  await settle(page);
  check("touch drag reorders queue", JSON.stringify((await order(page)).slice(0, 4)) === JSON.stringify(["1", "2", "3", "0"]), await order(page));
  check("reordering preserves current song identity", await current(page) === originalCurrent);
  check("reordering reuses every row DOM node", await page.evaluate(() => [...document.querySelectorAll(".queue-panel--embedded .queue-item")].every(el => window.queueNodes.get(el.dataset.trackKey) === el)));

  // Capture one synchronous frame during deletion, before the leave transition ends.
  const removal = await page.locator(`${selector} .queue-item`).nth(4).evaluate(async row => {
    const before = { top: row.offsetTop, left: row.offsetLeft, width: row.offsetWidth, height: row.offsetHeight };
    const first = row.parentElement.firstElementChild;
    row.querySelector(".queue-item__remove").click();
    await Promise.resolve(); await Promise.resolve();
    const pinned = { top: row.style.top, left: row.style.left, width: row.style.width, height: row.style.height };
    return { before, pinned, leaving: row.classList.contains("queue-leave-active"), firstRetained: row.parentElement.firstElementChild === first, firstEntering: first.classList.contains("queue-enter-active") };
  });
  check("deleted row is pinned at its former position during leave", removal.leaving && Object.entries(removal.before).every(([key, value]) => removal.pinned[key] === `${value}px`), removal);
  check("deleting later row neither replaces nor re-enters first row", removal.firstRetained && !removal.firstEntering, removal);
  await settle(page);
  check("surviving rows keep DOM identity after deletion", await page.evaluate(() => [...document.querySelectorAll(".queue-panel--embedded .queue-item")].every(el => window.queueNodes.get(el.dataset.trackKey) === el)));
  check("deleting another song preserves current song", await current(page) === originalCurrent);

  const consecutiveRemovals = await page.locator(`${selector} .queue-list__rows`).evaluate(async list => {
    const rows = [list.children[5], list.children[6]];
    const positions = rows.map(row => row.offsetTop);
    rows.forEach(row => row.querySelector(".queue-item__remove").click());
    await Promise.resolve(); await Promise.resolve();
    return rows.map((row, i) => ({ expectedTop: positions[i], top: row.style.top, leaving: row.classList.contains("queue-leave-active") }));
  });
  check("multiple removals in one update retain individual leave positions", consecutiveRemovals.every(row => row.leaving && row.top === `${row.expectedTop}px`), consecutiveRemovals);
  await settle(page);

  const beforeSwipe = await order(page);
  await page.locator(`${selector} .queue-item`).first().evaluate(el => el.scrollIntoView({ block: "start" }));
  const scrollStart = await page.evaluate(() => scrollY);
  await touch(client, "touchStart", 200, 300);
  await touchMove(client, page, 200, 300, 200, 140);
  await touch(client, "touchEnd");
  await settle(page);
  check("ordinary touch swipe on row content still scrolls", await page.evaluate(() => scrollY) > scrollStart + 50);
  check("ordinary swipe does not reorder queue", JSON.stringify(await order(page)) === JSON.stringify(beforeSwipe));

  await page.locator(`${selector} .queue-item`).first().evaluate(el => el.scrollIntoView({ block: "start" }));
  const autoHandle = await page.locator(`${selector} .queue-item__grip`).first().boundingBox();
  const dock = await page.locator(".player-bar").boundingBox();
  const beforeAutoScroll = await page.evaluate(() => scrollY);
  const autoX = autoHandle.x + autoHandle.width / 2, autoY = autoHandle.y + autoHandle.height / 2;
  await touch(client, "touchStart", autoX, autoY);
  await touchMove(client, page, autoX, autoY, autoX, dock.y - 8);
  await page.waitForTimeout(650);
  check("drag near lower player boundary auto-scrolls document", await page.evaluate(() => scrollY) > beforeAutoScroll + 100);
  await touch(client, "touchCancel");
  const cancelledOrder = await order(page);
  const cancelledScroll = await page.evaluate(() => scrollY);
  await page.waitForTimeout(120);
  check("cancel stops drag and automatic scrolling without committing", await page.locator(`${selector} .is-dragging`).count() === 0 && JSON.stringify(cancelledOrder) === JSON.stringify(beforeSwipe) && Math.abs(await page.evaluate(() => scrollY) - cancelledScroll) < 2);

  await page.locator(`${selector} .queue-item__grip`).first().focus();
  const keyboardSource = (await order(page))[0];
  await page.keyboard.press("ArrowDown");
  await settle(page);
  check("focused handle supports keyboard ordering and retains focus", (await order(page))[1] === keyboardSource && await page.locator(`${selector} .queue-item__grip`).nth(1).evaluate(el => el === document.activeElement));

  await page.locator(".sidebar .nav-item").first().click();
  await page.locator(".player-bar button[title='队列']").click();
  const drawer = ".queue-panel:not(.queue-panel--embedded)";
  await page.locator(drawer).waitFor();
  await settle(page);
  const drawerHandle = await page.locator(`${drawer} .queue-item__grip`).first().boundingBox();
  const body = page.locator(`${drawer} .queue-panel__body`);
  const bodyBox = await body.boundingBox();
  const dx = drawerHandle.x + drawerHandle.width / 2, dy = drawerHandle.y + drawerHandle.height / 2;
  await touch(client, "touchStart", dx, dy);
  await touchMove(client, page, dx, dy, dx, bodyBox.y + bodyBox.height - 8);
  await page.waitForTimeout(500);
  check("drawer handle drag auto-scrolls its internal body", await body.evaluate(el => el.scrollTop) > 100);
  await touch(client, "touchEnd");
  await settle(page);
  check("drawer drop clears capture and drag styling", await page.locator(`${drawer} .is-dragging`).count() === 0);

  await page.setViewportSize({ width: 1280, height: 900 });
  await body.evaluate(el => { el.scrollTop = 0; });
  await settle(page);
  const mouseOrder = await order(page);
  const mouseHandle = await page.locator(`${drawer} .queue-item__grip`).first().boundingBox();
  const mouseTarget = await page.locator(`${drawer} .queue-item`).nth(2).boundingBox();
  await page.mouse.move(mouseHandle.x + mouseHandle.width / 2, mouseHandle.y + mouseHandle.height / 2);
  await page.mouse.down();
  await page.mouse.move(mouseHandle.x + mouseHandle.width / 2, mouseTarget.y + mouseTarget.height / 2 + 8, { steps: 8 });
  await page.mouse.up();
  await settle(page);
  check("mouse handle drag still works", (await order(page))[2] === mouseOrder[0], await order(page));
  check("real app reports no uncaught errors", errors.length === 0, errors);
  await context.close();
} catch (error) {
  results.push({ name: "scenario completes", passed: false, error: error.stack });
  process.exitCode = 1;
} finally {
  await browser?.close();
  await server?.close();
}
console.log(JSON.stringify({ passed: results.filter(item => item.passed).length, failed: results.filter(item => !item.passed).length, checks: results }, null, 2));
