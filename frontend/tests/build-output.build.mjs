import assert from "node:assert/strict";
import { access, readFile, readdir } from "node:fs/promises";
import test from "node:test";

const output = new URL("../../web/", import.meta.url);

test("frontend build copies the service worker without changing its cache policy", async () => {
  assert.equal(
    await readFile(new URL("sw.js", output), "utf8"),
    await readFile(new URL("../public/sw.js", import.meta.url), "utf8"),
  );
});

test("public build excludes documentation and runtime configuration", async () => {
  await access(new URL("index.html", output));
  for (const name of ["docs", "config.toml", "conf.toml", ".env", "data", ".git"]) {
    await assert.rejects(access(new URL(name, output)), { code: "ENOENT" });
  }
});

test("generated decoder validates and contains no developer machine paths", async () => {
  const assets = new URL("assets/", output);
  const files = (await readdir(assets)).filter(name => name.endsWith(".wasm"));
  assert.ok(files.length > 0, "the build must include its decoder");
  for (const file of files) {
    const bytes = await readFile(new URL(file, assets));
    assert.ok(WebAssembly.validate(bytes), file);
    assert.doesNotMatch(bytes.toString("latin1"), /\/(?:Users|home)\/[^/]+\//);
  }
});

test("every compressible file has fresh .br and .gz siblings with identical content", async () => {
  const { brotliDecompressSync, gunzipSync } = await import("node:zlib");
  const files = [];
  const walk = async (directory) => {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const url = new URL(entry.name + (entry.isDirectory() ? "/" : ""), directory);
      if (entry.isDirectory()) await walk(url);
      else if (/\.(?:js|mjs|css|html|wasm|svg|json|txt|webmanifest)$/.test(entry.name)) files.push(url);
    }
  };
  await walk(output);
  assert.ok(files.some((url) => url.pathname.endsWith("/manifest.webmanifest")), "small files such as manifests are covered");
  for (const url of files) {
    const source = await readFile(url);
    // A missing or stale sibling would be served instead of this file.
    assert.deepEqual(brotliDecompressSync(await readFile(new URL(`${url.href}.br`))), source, `${url.pathname}.br`);
    assert.deepEqual(gunzipSync(await readFile(new URL(`${url.href}.gz`))), source, `${url.pathname}.gz`);
  }
});
