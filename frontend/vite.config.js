import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import { readdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { brotliCompress, constants, gzip } from "node:zlib";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const brotli = promisify(brotliCompress);
const gzipAsync = promisify(gzip);
const backgroundStub = fileURLToPath(new URL("./src/vendor/amll-background-stub.js", import.meta.url));
const COMPRESSIBLE = /\.(?:js|mjs|css|html|wasm|svg|json|txt|webmanifest)$/;

// The Rust server serves `.br` / `.gz` siblings through ServeDir's
// precompressed support, so compression happens once per build instead of
// per request (and also works without a compressing reverse proxy).
// Every compressible file always gets both siblings, even when tiny: deploys
// keep files missing from a build, and the server prefers a sibling, so a
// skipped sibling would leave an older compressed copy shadowing the update.
function precompress() {
  let outDir = "";
  return {
    name: "mhub-precompress",
    apply: "build",
    configResolved(config) { outDir = config.build.outDir; },
    async closeBundle() {
      const files = [];
      const walk = async (dir) => {
        for (const entry of await readdir(dir, { withFileTypes: true })) {
          const path = join(dir, entry.name);
          if (entry.isDirectory()) await walk(path);
          else if (COMPRESSIBLE.test(entry.name)) files.push(path);
        }
      };
      await walk(outDir);
      await Promise.all(files.map(async (path) => {
        const source = await readFile(path);
        const [br, gz] = await Promise.all([
          brotli(source, { params: { [constants.BROTLI_PARAM_QUALITY]: 11, [constants.BROTLI_PARAM_SIZE_HINT]: source.length } }),
          gzipAsync(source, { level: 9 }),
        ]);
        await Promise.all([writeFile(`${path}.br`, br), writeFile(`${path}.gz`, gz)]);
      }));
    },
  };
}

const apiTarget = process.env.MHUB_API_PROXY || "http://127.0.0.1:8000";
const apiProxy = { target: apiTarget, changeOrigin: !apiTarget.includes("127.0.0.1") };

export default defineConfig({
  plugins: [vue(), precompress()],
  resolve: {
    alias: [
      { find: /^@pixi\/.+$/, replacement: backgroundStub },
      { find: /^gl-matrix$/, replacement: backgroundStub },
    ],
  },
  server: {
    host: "127.0.0.1",
    port: 5173,
    // MHUB_API_PROXY points the dev server at another backend, e.g. a
    // deployed instance, to try UI changes against real data.
    proxy: {
      "/api": apiProxy,
      "/health": apiProxy,
    },
  },
  build: {
    outDir: "../web",
    emptyOutDir: true,
  },
});
