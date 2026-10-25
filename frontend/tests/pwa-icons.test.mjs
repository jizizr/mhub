import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import test from "node:test";

const publicDir = new URL("../public/", import.meta.url);

// Edge embeds a re-encoded PNG as a base64 launch-intent string. Android's
// shortcut persistence rejects strings >65535 bytes and loses the icon after
// reboot. Bound source complexity here; verify the actual re-encoded string on
// Android too, since resizing and encoder choices affect the final byte count.
test("PWA install icons stay compact and use new URLs when their pixels change", async () => {
  const manifest = JSON.parse(await readFile(new URL("manifest.webmanifest", publicDir)));
  const html = await readFile(new URL("../index.html", import.meta.url), "utf8");
  const manifestHref = html.match(/rel="manifest" href="([^"]+)"/)[1];
  const version = new URL(manifestHref, "https://example.test").searchParams.get("v");
  const primaryIcon = manifest.icons.find((icon) => icon.sizes === "512x512" && !icon.purpose);
  assert.ok(version && primaryIcon.src.includes(version), "version the manifest too: browser install metadata can remain cached for an hour");
  const worker = await readFile(new URL("sw.js", publicDir), "utf8");
  assert.ok(worker.includes(manifestHref), "precache the current versioned install metadata for offline use");
  const appleIcon = html.match(/rel="apple-touch-icon" href="([^"]+)"/)[1];
  const icons = [...manifest.icons.filter((icon) => Number(icon.sizes.split("x")[0]) >= 192), { src: appleIcon, sizes: "180x180" }];
  for (const icon of icons) {
    const data = await readFile(new URL(icon.src.slice(1), publicDir));
    assert.equal(data.subarray(0, 8).toString("hex"), "89504e470d0a1a0a");
    assert.equal(`${data.readUInt32BE(16)}x${data.readUInt32BE(20)}`, icon.sizes);
    assert.ok(data.length < 28 * 1024, `${icon.src}: reduce icon entropy before publishing`);
    assert.ok(icon.src.includes(createHash("sha256").update(data).digest("hex").slice(0, 10)), "new icon pixels must bypass stale browser/OS caches");
  }
});
