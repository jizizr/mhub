// Optional asset-authoring tool; Sharp is not shipped to the browser.
// Install Sharp separately and use SHARP_MODULE=/absolute/path/to/sharp/index.mjs
// if it isn't resolvable from this script. Commit the generated PNGs with the SVG.
import { createHash } from "node:crypto";
import { readFile, writeFile, readdir, unlink } from "node:fs/promises";
import { pathToFileURL, fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const publicDir = path.join(root, "frontend/public");
const iconsDir = path.join(publicDir, "icons");
const { default: sharp } = await import(process.env.SHARP_MODULE ? pathToFileURL(process.env.SHARP_MODULE) : "sharp");
// The rounded icon carries its own continuous-corner shape with transparent
// corners. No "maskable" variant is declared: Edge on HyperOS pinned the
// full-bleed maskable bitmap as an unmasked square. iOS masks apple-touch
// icons itself and needs the full-bleed source.
const source = await readFile(path.join(iconsDir, "app-icon.svg"));
const square = await readFile(path.join(iconsDir, "app-icon-square.svg"));
const render = (svg, size) => sharp(svg, { density: 72 * size / 1024 * 4 }).resize(size, size)
  .png({ compressionLevel: 9, adaptiveFiltering: true, effort: 10 }).toBuffer();
const generated = [];
for (const [name, size, svg] of [
  ["icon", 192, source], ["icon", 512, source],
  ["apple-touch-icon", 180, square],
]) {
  // Flat shapes and smooth gradients only: Android shortcut hosts embed the
  // decoded, resized PNG in a string with a 65535-byte cap. Full-colour PNG
  // avoids palette banding in the gradients and stays far below that cap;
  // run verify_pwa_icons_browser.mjs and verify on Android.
  const data = await render(svg, size);
  const hash = createHash("sha256").update(data).digest("hex").slice(0, 10);
  const filename = `${name}${name === "apple-touch-icon" ? "" : `-${size}`}-${hash}.png`;
  await writeFile(path.join(iconsDir, filename), data);
  generated.push({ name, size, filename });
}
// Browser tab icons. favicon.ico embeds PNG images (supported since Vista).
const favicons = [];
for (const size of [16, 32, 48]) {
  const data = await render(source, size);
  if (size !== 48) await writeFile(path.join(iconsDir, `favicon-${size}.png`), data);
  favicons.push({ size, data });
}
const header = Buffer.alloc(6 + 16 * favicons.length);
header.writeUInt16LE(1, 2);
header.writeUInt16LE(favicons.length, 4);
let offset = header.length;
favicons.forEach(({ size, data }, index) => {
  const entry = 6 + index * 16;
  header.writeUInt8(size, entry);
  header.writeUInt8(size, entry + 1);
  header.writeUInt16LE(1, entry + 4);
  header.writeUInt16LE(32, entry + 6);
  header.writeUInt32LE(data.length, entry + 8);
  header.writeUInt32LE(offset, entry + 12);
  offset += data.length;
});
const ico = Buffer.concat([header, ...favicons.map(({ data }) => data)]);
await writeFile(path.join(publicDir, "favicon.ico"), ico);
// Tab icons keep fixed file names (browsers probe /favicon.ico), so pages
// reference them with a content version: CDN edges have shown stale fixed
// URLs after a purge, while a new query string is a new cache entry.
const contentVersion = (data) => createHash("sha256").update(data).digest("hex").slice(0, 10);
const faviconHrefs = {
  "/favicon.ico": `/favicon.ico?v=${contentVersion(ico)}`,
  "/icons/favicon-32.png": `/icons/favicon-32.png?v=${contentVersion(favicons.find(({ size }) => size === 32).data)}`,
  "/icons/favicon-16.png": `/icons/favicon-16.png?v=${contentVersion(favicons.find(({ size }) => size === 16).data)}`,
};
const versionFavicons = (text) => text.replace(/\/(?:favicon\.ico|icons\/favicon-(?:16|32)\.png)(?:\?v=[a-f0-9]+)?/g,
  (match) => faviconHrefs[match.split("?")[0]]);

for (const filename of await readdir(iconsDir)) {
  if (/^(?:icon|maskable)-(?:192|512)-[a-f0-9]+\.png$|^apple-touch-icon-[a-f0-9]+\.png$/.test(filename)
    && !generated.some((icon) => icon.filename === filename)) await unlink(path.join(iconsDir, filename));
}
const version = generated.find((icon) => icon.name === "icon" && icon.size === 512).filename.match(/-([a-f0-9]+)\.png$/)[1];
for (const name of ["manifest.webmanifest", "manifest-dark.webmanifest"]) {
  const manifestPath = path.join(publicDir, name);
  const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  // The manifest's own ?v= follows the install icon; keep its favicon entry
  // fixed so a tab-icon change never alters an already cached manifest URL.
  const favicon = manifest.icons.find((icon) => icon.sizes === "32x32");
  manifest.icons = [{ ...favicon, src: favicon.src.split("?")[0] }, ...generated
    .filter((icon) => icon.name !== "apple-touch-icon")
    .map((icon) => ({ src: `/icons/${icon.filename}`, sizes: `${icon.size}x${icon.size}`, type: "image/png", ...(icon.name === "maskable" ? { purpose: "maskable" } : {}) }))];
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
}
for (const filename of ["frontend/index.html", "frontend/src/utils/theme.js", "frontend/public/sw.js", "frontend/tests/theme.test.mjs", "scripts/verify_pwa_theme_browser.mjs"]) {
  const file = path.join(root, filename);
  let text = (await readFile(file, "utf8")).replace(/(manifest(?:-dark)?\.webmanifest\?v=)[a-f0-9]+/g, `$1${version}`);
  if (filename === "frontend/index.html") {
    text = versionFavicons(text.replace(/\/icons\/apple-touch-icon-[a-f0-9]+\.png/g, `/icons/${generated.find((icon) => icon.name === "apple-touch-icon").filename}`));
  }
  await writeFile(file, text);
}
console.log(JSON.stringify({ version, icons: generated.map((icon) => icon.filename) }));
