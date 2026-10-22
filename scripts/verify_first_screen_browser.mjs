/**
 * Real deployed first-screen measurements using isolated installed Edge contexts.
 * No playback, stored credentials, API response bodies, UI text or screenshots.
 *
 * PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs \
 * BROWSER_EXECUTABLE='/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge' \
 * OUTPUT=reports/first-screen.json node scripts/verify_first_screen_browser.mjs
 *
 * Defaults: unthrottled desktop/mobile, 3 fresh contexts and 1 warm reload each.
 * Fresh contexts clear browser storage/cache, not OS DNS or shared CDN caches.
 * Service workers are blocked consistently so their prior state cannot confound
 * the before/after comparison. Recommendation APIs and artwork are unmodified.
 */
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

const origin = new URL(process.env.MUSIC_WEB_ORIGIN || "http://127.0.0.1:8000");
assert(["http:", "https:"].includes(origin.protocol) && !origin.username && !origin.password);
assert(origin.pathname === "/" && !origin.search && !origin.hash, "Use only a public origin");
const output = path.resolve(process.env.OUTPUT || "reports/first-screen.json");
const repeats = Math.max(1, Math.min(5, Number(process.env.RUNS) || 3));
const observationMs = Math.max(5000, Math.min(30000, Number(process.env.OBSERVE_MS) || 15000));
const readyTimeoutMs = 45000;
const executablePath = process.env.BROWSER_EXECUTABLE || "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge";
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const profiles = [
  { name: "desktop", viewport: { width: 1365, height: 900 }, isMobile: false, hasTouch: false },
  { name: "mobile", viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true },
].filter(profile => !process.env.PROFILE || profile.name === process.env.PROFILE);

// Keep useful application routes/asset hashes but strip queries, resource IDs,
// and all third-party paths (artwork URLs may identify recommendation content).
function resourceIdentity(value) {
  try {
    const url = new URL(value, origin);
    if (url.origin !== origin.origin) return { host: url.host, path: "[external-resource]" };
    let route = url.pathname;
    if (!/^\/(?:assets\/[^/]+|api\/v1\/(?:platforms|recommendations(?:\/home)?|cache|playbacks)|manifest\.webmanifest|sw\.js|favicon\.[a-z]+|)$/.test(route)) {
      if (route.startsWith("/api/")) route = route.replace(/\/[A-Za-z0-9_-]{12,}(?=\/|$)/g, "/:id");
      else route = "[other-same-origin-resource]";
    }
    return { host: url.host, path: route };
  } catch { return { host: "[unknown]", path: "[invalid-url]" }; }
}
const round = value => typeof value === "number" && Number.isFinite(value) ? Math.round(value * 100) / 100 : null;
const median = values => {
  const filtered = values.filter(Number.isFinite).sort((a, b) => a - b);
  if (!filtered.length) return null;
  const mid = Math.floor(filtered.length / 2);
  return round(filtered.length % 2 ? filtered[mid] : (filtered[mid - 1] + filtered[mid]) / 2);
};

const report = {
  schema: 1,
  measuredAt: new Date().toISOString(),
  origin: origin.origin,
  conditions: {
    browser: "installed Microsoft Edge", headless: true, deviceScaleFactor: 1,
    networkThrottling: "none", cpuThrottling: "none", location: "local execution host (CDN POP recorded per response)",
    cold: "fresh isolated browser context; empty HTTP cache, cookies, local/session storage and IndexedDB",
    warm: "reload in the final cold context; browser HTTP cache retained; no interactions",
    serviceWorkers: "blocked", repeats, observationMs, readyTimeoutMs,
    privacy: "metadata only; no response bodies, page text, screenshots, credentials, query strings or external resource paths",
    ready: "recommendation play button enabled and at least one recommendation row/card rendered",
    limitations: "unthrottled emulated viewport, not physical phone; repeated CDN/API server cache may be warm; no geographic inference from viewport",
  },
  runs: [], summary: {},
};

const browser = await chromium.launch({ headless: true, executablePath });
report.browserVersion = browser.version();
try {
  for (const profile of profiles) {
    for (let iteration = 1; iteration <= repeats; iteration += 1) {
      const context = await browser.newContext({ ...profile, name: undefined, deviceScaleFactor: 1, serviceWorkers: "block", reducedMotion: "no-preference" });
      const page = await context.newPage();
      await page.addInitScript(() => {
        const probe = window.__firstScreenProbe = { lcp: [], longTasks: [], firstHeroAt: null, firstRecommendationAt: null, firstReadyAt: null, firstVisibleImageAt: null };
        new PerformanceObserver(list => {
          for (const entry of list.getEntries()) probe.lcp.push({ at: entry.startTime, renderTime: entry.renderTime, loadTime: entry.loadTime, size: entry.size, tag: entry.element?.tagName || null, class: typeof entry.element?.className === "string" ? entry.element.className : null });
        }).observe({ type: "largest-contentful-paint", buffered: true });
        new PerformanceObserver(list => {
          for (const entry of list.getEntries()) probe.longTasks.push({ at: entry.startTime, duration: entry.duration });
        }).observe({ type: "longtask", buffered: true });
        const visible = element => {
          const rect = element.getBoundingClientRect();
          return rect.width > 0 && rect.height > 0 && rect.top < innerHeight && rect.bottom > 0 && rect.left < innerWidth && rect.right > 0;
        };
        function check() {
          const now = performance.now();
          if (probe.firstHeroAt === null && document.querySelector(".recommend-hero")) probe.firstHeroAt = now;
          if (probe.firstRecommendationAt === null && document.querySelector(".recommend-list-row, .recommend-personal-card")) probe.firstRecommendationAt = now;
          if (probe.firstReadyAt === null && probe.firstRecommendationAt !== null && document.querySelector(".recommend-hero__actions .primary-action:not(:disabled)")) probe.firstReadyAt = now;
          if (probe.firstVisibleImageAt === null && [...document.querySelectorAll(".recommend-feed img")].some(img => img.complete && img.naturalWidth > 0 && visible(img))) probe.firstVisibleImageAt = now;
          requestAnimationFrame(check);
        }
        requestAnimationFrame(check);
      });
      for (const cacheMode of iteration === repeats ? ["cold", "warm"] : ["cold"]) {
        const run = { profile: profile.name, viewport: profile.viewport, iteration, cacheMode, measuredAt: new Date().toISOString(), requests: [], errors: [] };
        const requestRecords = new Map();
        const headerReads = [];
        const onRequest = request => {
          const record = { ...resourceIdentity(request.url()), method: request.method(), type: request.resourceType() };
          requestRecords.set(request, record);
          run.requests.push(record);
        };
        const onResponse = response => {
          const record = requestRecords.get(response.request());
          if (!record) return;
          record.status = response.status();
          record.fromServiceWorker = response.fromServiceWorker();
          headerReads.push(response.allHeaders().then(headers => {
            record.headers = Object.fromEntries(["cf-cache-status", "cf-ray", "age", "cache-control", "content-type", "content-length", "content-encoding", "server-timing"].filter(key => headers[key] !== undefined).map(key => [key, headers[key]]));
          }).catch(() => {}));
        };
        const onFinished = request => {
          const record = requestRecords.get(request);
          if (!record) return;
          record.timing = request.timing();
        };
        const onFailed = request => {
          const record = requestRecords.get(request);
          if (record) record.failure = request.failure()?.errorText || "request failed";
        };
        const onPageError = error => { run.errors.push({ type: "pageerror", name: error.name }); };
        page.on("request", onRequest); page.on("response", onResponse);
        page.on("requestfinished", onFinished); page.on("requestfailed", onFailed); page.on("pageerror", onPageError);
        try {
          if (cacheMode === "warm") await page.reload({ waitUntil: "commit", timeout: 60000 });
          else await page.goto(origin.href, { waitUntil: "commit", timeout: 60000 });
          await page.waitForFunction(({ observationMs, readyTimeoutMs }) => {
            const elapsed = performance.now();
            return (elapsed >= observationMs && window.__firstScreenProbe?.firstReadyAt !== null) || elapsed >= readyTimeoutMs;
          }, { observationMs, readyTimeoutMs }, { timeout: readyTimeoutMs + 5000 });
          const raw = await page.evaluate(() => {
            const nav = performance.getEntriesByType("navigation")[0];
            return {
              timeOrigin: performance.timeOrigin,
              observedUntilMs: performance.now(), navigation: nav?.toJSON(),
              paints: performance.getEntriesByType("paint").map(entry => ({ name: entry.name, at: entry.startTime })),
              probe: window.__firstScreenProbe,
              resources: performance.getEntriesByType("resource").map(entry => entry.toJSON()),
              counts: { cards: document.querySelectorAll(".recommend-personal-card").length, rows: document.querySelectorAll(".recommend-list-row").length, errors: document.querySelectorAll(".recommend-feed .error-banner").length },
            };
          });
          await Promise.allSettled(headerReads);
          const nav = raw.navigation;
          run.metrics = {
            ttfbMs: round(nav.responseStart - nav.startTime), requestTtfbMs: round(nav.responseStart - nav.requestStart),
            dnsMs: round(nav.domainLookupEnd - nav.domainLookupStart), connectMs: round(nav.connectEnd - nav.connectStart), tlsMs: round(nav.secureConnectionStart ? nav.connectEnd - nav.secureConnectionStart : 0),
            htmlDownloadMs: round(nav.responseEnd - nav.responseStart), htmlDecodedBytes: nav.decodedBodySize, htmlTransferBytes: nav.transferSize,
            domContentLoadedMs: round(nav.domContentLoadedEventEnd), loadMs: round(nav.loadEventEnd),
            fcpMs: round(raw.paints.find(entry => entry.name === "first-contentful-paint")?.at),
            lcpMs: round(raw.probe.lcp.at(-1)?.at),
            heroMs: round(raw.probe.firstHeroAt), recommendationsRenderedMs: round(raw.probe.firstRecommendationAt), recommendationsReadyMs: round(raw.probe.firstReadyAt), firstVisibleImageMs: round(raw.probe.firstVisibleImageAt),
            longTaskCount: raw.probe.longTasks.length, longTaskTotalMs: round(raw.probe.longTasks.reduce((sum, entry) => sum + entry.duration, 0)),
            totalBlockingTimeMs: round(raw.probe.longTasks.reduce((sum, entry) => sum + Math.max(entry.duration - 50, 0), 0)), observedUntilMs: round(raw.observedUntilMs),
          };
          run.lcpCandidates = raw.probe.lcp.map(entry => Object.fromEntries(Object.entries(entry).map(([key, value]) => [key, typeof value === "number" ? round(value) : value])));
          run.longTasks = raw.probe.longTasks;
          run.counts = raw.counts;
          run.resources = raw.resources.map(entry => ({
            ...resourceIdentity(entry.name), type: entry.initiatorType,
            startMs: round(entry.startTime), durationMs: round(entry.duration), responseStartMs: round(entry.responseStart), endMs: round(entry.responseEnd),
            dnsMs: round(entry.domainLookupEnd - entry.domainLookupStart), connectMs: round(entry.connectEnd - entry.connectStart), requestTtfbMs: round(entry.responseStart && entry.requestStart ? entry.responseStart - entry.requestStart : 0),
            transferBytes: entry.transferSize, encodedBytes: entry.encodedBodySize, decodedBytes: entry.decodedBodySize, protocol: entry.nextHopProtocol,
            deliveryType: entry.deliveryType || "", renderBlockingStatus: entry.renderBlockingStatus || "",
          }));
          for (const record of run.requests) {
            if (!record.timing) continue;
            const timing = record.timing;
            record.startMs = round(timing.startTime - raw.timeOrigin);
            record.ttfbMs = round(timing.responseStart - timing.requestStart);
            record.endMs = round(record.startMs + timing.responseEnd);
            delete timing.startTime;
            record.timing = Object.fromEntries(Object.entries(timing).map(([key, value]) => [key, round(value)]));
          }
          run.requests.sort((a, b) => (a.startMs ?? Infinity) - (b.startMs ?? Infinity));
          run.criticalChain = run.resources.filter(resource => ["script", "link", "css"].includes(resource.type));
          run.apiTimeline = run.requests.filter(request => request.path.startsWith("/api/"));
        } catch (error) { run.errors.push({ type: "measurement", name: error.name, message: String(error.message).split("\n")[0].replace(/https?:\/\/\S+/g, "[url]") }); }
        finally {
          page.off("request", onRequest); page.off("response", onResponse);
          page.off("requestfinished", onFinished); page.off("requestfailed", onFailed); page.off("pageerror", onPageError);
        }
        report.runs.push(run);
        await mkdir(path.dirname(output), { recursive: true });
        await writeFile(output, JSON.stringify(report, null, 2) + "\n");
        console.log(JSON.stringify({ profile: run.profile, iteration, cacheMode, metrics: run.metrics, errors: run.errors, api: run.apiTimeline?.map(request => ({ path: request.path, startMs: request.startMs, endMs: request.endMs, status: request.status, cache: request.headers?.["cf-cache-status"] })) }));
      }
      await context.close();
    }
  }
  for (const profile of profiles) {
    const runs = report.runs.filter(run => run.profile === profile.name && run.cacheMode === "cold");
    report.summary[profile.name] = Object.fromEntries(["ttfbMs", "fcpMs", "lcpMs", "domContentLoadedMs", "loadMs", "heroMs", "recommendationsReadyMs", "firstVisibleImageMs", "longTaskTotalMs", "totalBlockingTimeMs"].map(metric => [metric, { median: median(runs.map(run => run.metrics?.[metric])), samples: runs.map(run => run.metrics?.[metric] ?? null) }]));
  }
  report.finishedAt = new Date().toISOString();
  await writeFile(output, JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify({ output, summary: report.summary }));
} finally { await browser.close(); }
