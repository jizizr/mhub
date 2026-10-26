import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { createThemeController } from "../src/utils/theme.js";

const html = await readFile(new URL("../index.html", import.meta.url), "utf8");
const bootstrap = html.match(/<script>([\s\S]*?)<\/script>/)[1];
function harness(theme, dark = false, blocked = false) {
  const win = new EventTarget();
  const media = new EventTarget();
  media.matches = dark;
  win.matchMedia = () => media;
  const doc = new EventTarget();
  doc.visibilityState = "visible";
  doc.documentElement = { dataset: {}, style: {} };
  const metas = { 'meta[name="theme-color"]': {}, 'meta[name="color-scheme"]': {} };
  let href = "/manifest.webmanifest?v=29e8713a10";
  const manifest = { getAttribute: () => href, setAttribute: (_, value) => { href = value; } };
  doc.querySelector = (selector) => metas[selector] || manifest;
  vm.runInNewContext(bootstrap, {
    document: doc, matchMedia: win.matchMedia,
    localStorage: { getItem() { if (blocked) throw new Error("blocked"); return JSON.stringify({ theme }); } },
  });
  const controller = createThemeController(() => theme, win, doc);
  return {
    win, doc, media, controller,
    setTheme: (value) => { theme = value; controller.sync(); },
    actual: () => [doc.documentElement.style.colorScheme, metas['meta[name="theme-color"]'].content,
      doc.documentElement.style.backgroundColor, metas['meta[name="color-scheme"]'].content, href],
  };
}
const expected = (dark) => [dark ? "dark" : "light", dark ? "#1f1f1f" : "#ffffff",
  dark ? "#1f1f1f" : "#ffffff", dark ? "dark" : "light",
  dark ? "/manifest-dark.webmanifest?v=29e8713a10" : "/manifest.webmanifest?v=29e8713a10"];

test("first paint and runtime agree for explicit overrides and either system theme", () => {
  assert.equal((html.match(/<meta name="theme-color"/g) || []).length, 1);
  for (const theme of ["light", "dark", "system", "invalid", undefined]) {
    for (const osDark of [false, true]) {
      const h = harness(theme, osDark);
      const dark = theme === "dark" || (theme !== "light" && osDark);
      assert.deepEqual(h.actual(), expected(dark), "before Vue loads");
      h.controller.sync();
      assert.deepEqual(h.actual(), expected(dark), "after Vue mounts");
      h.controller.dispose();
    }
  }
});

test("blocked storage still renders system dark before first paint", () => {
  const h = harness("system", true, true);
  assert.deepEqual(h.actual(), expected(true));
  h.controller.dispose();
});

test("OS changes, explicit overrides, resume and disposal update browser chrome", () => {
  const h = harness("system");
  h.media.matches = true;
  h.media.dispatchEvent(new Event("change"));
  assert.deepEqual(h.actual(), expected(true));
  h.setTheme("light");
  h.media.dispatchEvent(new Event("change"));
  assert.deepEqual(h.actual(), expected(false));
  h.setTheme("system");
  assert.equal(h.doc.documentElement.dataset.theme, undefined);
  h.media.matches = false;
  h.win.dispatchEvent(new Event("pageshow"));
  assert.deepEqual(h.actual(), expected(false));
  h.media.matches = true;
  h.doc.dispatchEvent(new Event("visibilitychange"));
  assert.deepEqual(h.actual(), expected(true));
  h.controller.dispose();
  h.media.matches = false;
  h.media.dispatchEvent(new Event("change"));
  h.win.dispatchEvent(new Event("pageshow"));
  h.doc.dispatchEvent(new Event("visibilitychange"));
  assert.deepEqual(h.actual(), expected(true));
});

test("light and dark install manifests share identity, launch URL, icons and features", async () => {
  const light = JSON.parse(await readFile(new URL("../public/manifest.webmanifest?v=29e8713a10", import.meta.url)));
  const dark = JSON.parse(await readFile(new URL("../public/manifest-dark.webmanifest?v=29e8713a10", import.meta.url)));
  assert.equal(light.id, "/");
  assert.equal(light.theme_color, "#f6f5f3");
  assert.equal(dark.theme_color, "#141416");
  assert.equal(dark.background_color, dark.theme_color);
  assert.deepEqual({ ...dark, theme_color: light.theme_color, background_color: light.background_color }, light);
});
