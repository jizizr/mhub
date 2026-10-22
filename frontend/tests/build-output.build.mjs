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
