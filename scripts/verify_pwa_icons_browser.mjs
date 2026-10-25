// Verify decoded/re-encoded icons, not just the compressed download size.
// Browser/native resamplers differ: this is a regression budget, not a substitute
// for testing the real Android shortcut's persisted EXTRA_ICON string.
// Optional: PLAYWRIGHT_MODULE, BROWSER_EXECUTABLE, MUSIC_WEB_ORIGIN.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const publicDir = path.join(root, "frontend/public");
const manifest = JSON.parse(await readFile(path.join(publicDir, "manifest.webmanifest"), "utf8"));
const browser = await chromium.launch({ headless: true,
  ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
try {
  const page = await browser.newPage();
  const results = [];
  for (const icon of manifest.icons.filter((icon) => Number(icon.sizes.split("x")[0]) >= 192)) {
    const bytes = process.env.MUSIC_WEB_ORIGIN
      ? Buffer.from(await (await fetch(new URL(icon.src, process.env.MUSIC_WEB_ORIGIN))).arrayBuffer())
      : await readFile(path.join(publicDir, icon.src));
    const result = await page.evaluate(async ({ src, maskable }) => {
      const image = new Image(); image.src = src; await image.decode();
      // Hosts select the larger declared icon for high-density launchers.
      // Test downscaling each source, including fractional Android densities.
      const sizes = [192, 216, 240, 256, 270, 288, 384, 432, 512].filter((size) => size <= image.width);
      const samples = [];
      for (const quality of ["low", "high"]) for (const size of sizes) {
        const canvas = document.createElement("canvas"); canvas.width = canvas.height = size;
        const ctx = canvas.getContext("2d"); ctx.imageSmoothingQuality = quality;
        ctx.drawImage(image, 0, 0, size, size);
        const base64 = canvas.toDataURL("image/png").split(",")[1];
        // Android Base64.DEFAULT appends a newline to every 76-character line.
        samples.push({ size, quality, stringBytes: base64.length + Math.ceil(base64.length / 76) });
      }
      let unsafeGlyphPixels = 0;
      if (maskable) {
        const c = document.createElement("canvas"); c.width = c.height = image.width;
        const ctx = c.getContext("2d"); ctx.drawImage(image, 0, 0);
        const data = ctx.getImageData(0, 0, c.width, c.height).data;
        for (let y = 0; y < c.height; y++) for (let x = 0; x < c.width; x++) {
          const i = (y * c.width + x) * 4;
          if (data[i + 3] !== 255) throw new Error("Maskable icon background must be opaque");
          if (Math.min(data[i], data[i + 1], data[i + 2]) > 245
            && Math.hypot(x + .5 - c.width / 2, y + .5 - c.height / 2) > c.width * .4) unsafeGlyphPixels++;
        }
      }
      return { samples, unsafeGlyphPixels };
    }, { src: `data:image/png;base64,${bytes.toString("base64")}`, maskable: icon.purpose === "maskable" });
    for (const sample of result.samples) {
      assert.ok(sample.stringBytes < 60 * 1024, `${icon.src} at ${sample.size}/${sample.quality}: ${sample.stringBytes} bytes, approaching Android's 65535-byte string limit`);
    }
    assert.equal(result.unsafeGlyphPixels, 0, `${icon.src}: glyph must fit maskable safe zone`);
    results.push({ src: icon.src, checks: result.samples.length, maxEncodedBytes: Math.max(...result.samples.map((s) => s.stringBytes)), maskableSafeZone: result.unsafeGlyphPixels === 0 });
  }
  console.log(JSON.stringify({ passed: true, results }));
} finally { await browser.close(); }
