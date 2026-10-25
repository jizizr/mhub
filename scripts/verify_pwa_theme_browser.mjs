// Checks page-side PWA theme signals, including before Vue executes. Native
// Android system-bar rendering still requires a physical device test.
// Optional: MUSIC_WEB_ORIGIN, PLAYWRIGHT_MODULE, BROWSER, BROWSER_EXECUTABLE.
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
let server;
let browser;
let checks = 0;
try {
  let origin = process.env.MUSIC_WEB_ORIGIN;
  if (!origin) {
    const { createServer } = await import(pathToFileURL(path.join(root, "frontend/node_modules/vite/dist/node/index.js")));
    server = await createServer({ root: path.join(root, "frontend"), server: { host: "127.0.0.1", port: 0 } });
    await server.listen();
    origin = `http://127.0.0.1:${server.httpServer.address().port}`;
  }
  const engine = process.env.BROWSER || "chromium";
  browser = await playwright[engine].launch({ headless: true,
    ...(engine === "chromium" && process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
  for (const [theme, os] of [["dark", "light"], ["light", "dark"], ["system", "dark"], ["system", "light"]]) {
    const context = await browser.newContext({ viewport: { width: 390, height: 844 }, colorScheme: os, serviceWorkers: "block" });
    try {
      const page = await context.newPage();
      await page.addInitScript((theme) => localStorage.setItem("music-web-splayer-status-v3", JSON.stringify({ theme })), theme);
      let releaseScripts;
      const gate = new Promise((resolve) => { releaseScripts = resolve; });
      await page.route("**/*", async (route) => {
        const request = route.request();
        if (new URL(request.url()).pathname.startsWith("/api/")) {
          return route.fulfill({ json: new URL(request.url()).pathname.endsWith("/platforms") ? [{ id: "test", label: "Test" }] : { tracks: [], playlists: [] } });
        }
        if (request.resourceType() === "script") await gate;
        return route.continue();
      });
      const isDark = theme === "dark" || (theme === "system" && os === "dark");
      async function verify(dark, phase) {
        const scheme = dark ? "dark" : "light";
        await page.waitForFunction((scheme) => document.documentElement.style.colorScheme === scheme, scheme);
        const state = await page.evaluate(() => ({
          theme: document.querySelector('meta[name="theme-color"]').content,
          count: document.querySelectorAll('meta[name="theme-color"]').length,
          scheme: document.querySelector('meta[name="color-scheme"]').content,
          canvas: document.documentElement.style.backgroundColor,
          manifest: document.querySelector('link[rel="manifest"]').getAttribute("href"),
        }));
        assert.deepEqual(state, { theme: dark ? "#141416" : "#f6f5f3", count: 1, scheme,
          canvas: dark ? "rgb(20, 20, 22)" : "rgb(246, 245, 243)",
          manifest: dark ? "/manifest-dark.webmanifest?v=29e8713a10" : "/manifest.webmanifest?v=29e8713a10" }, `${theme}/${os} ${phase}`);
        checks += 1;
      }
      try {
        await page.goto(origin, { waitUntil: "commit" });
        await verify(isDark, "before application scripts");
      } finally { releaseScripts(); }
      await page.locator(".app").waitFor();
      await verify(isDark, "mounted");
      await page.emulateMedia({ colorScheme: os === "dark" ? "light" : "dark" });
      await verify(theme === "system" ? !isDark : isDark, "OS change");
      await page.setViewportSize({ width: 1280, height: 900 });
      await page.locator(".theme-toggle").click();
      await verify(theme === "light" || (theme === "dark" && os === "light"), "app theme toggle");
    } finally { await context.close(); }
  }
  console.log(JSON.stringify({ checks, passed: true, nativeAndroidSystemBar: "requires physical device verification" }));
} finally {
  await browser?.close();
  await server?.close();
}
