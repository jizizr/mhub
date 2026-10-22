import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { cacheKey, cacheEntryMatches, hasCachedAudio, reusableNativeFlacCache } from "../src/services/localCache.js";
import { trackFromPlayback } from "../src/utils/track.js";
import { audioUrl } from "../src/apis/music.js";

const source = await readFile(new URL("../src/App.vue", import.meta.url), "utf8");
const resolver = source.slice(source.indexOf("async function resolveTrackSource("), source.indexOf("\n// Cold playback"));
const requested = { platform: "applemusic", source_id: "fixture", selected_id: "fixture", quality: "hires", streaming_mode: "wasm_alac" };
const native = { ...requested, id: "fixture-playback", streaming_mode: null, format: "m4a", content_type: "audio/mp4", status: "completed" };
const flacBytes = new Uint8Array(60);
flacBytes.set([0x66, 0x4c, 0x61, 0x43, 0x80, 0, 0, 34]);
flacBytes[8] = flacBytes[10] = 0x10; // 4096-frame blocks.
flacBytes.set([0x17, 0x70, 0x03, 0x70], 18); // 96 kHz, stereo, 24-bit STREAMINFO.
flacBytes.set([0xff, 0xf8, 0x10, 0x18], 42); // First frame header.
function entry(overrides = {}) {
  return { key: cacheKey(native), task: { ...native }, track: { ...native }, blob: new Blob([flacBytes], { type: "audio/mp4" }), ...overrides };
}
const playable = () => "probably";

test("legacy FLAC is identified by bytes despite stale M4A metadata and MIME", async () => {
  const original = entry();
  const result = await reusableNativeFlacCache(original, requested, playable);
  assert.ok(result);
  assert.equal(result.key, cacheKey(native));
  assert.equal(result.track.streaming_mode, "");
  assert.equal(result.task.streaming_mode, "");
  assert.equal(result.track.format, "flac");
  assert.equal(result.blob.type, "audio/flac");
  assert.deepEqual(new Uint8Array(await result.blob.arrayBuffer()), flacBytes);
  assert.equal(original.track.format, "m4a", "stored input is not mutated");
  assert.equal(original.blob.type, "audio/mp4");
});

for (const [label, value] of [
  ["metadata only", entry({ blob: null })],
  ["empty Blob", entry({ blob: new Blob([]) })],
  ["truncated FLAC", entry({ blob: new Blob(["fLaC"]) })],
  ["metadata without audio frames", entry({ blob: new Blob([flacBytes.slice(0, 42)]) })],
  ["invalid zero-filled STREAMINFO", entry({ blob: new Blob([flacBytes.slice(0, 8), new Uint8Array(34), flacBytes.slice(42)]) })],
  ["missing first frame sync", entry({ blob: new Blob([flacBytes.slice(0, 42), new Uint8Array(18)]) })],
  ["M4A even with a FLAC MIME", entry({ blob: new Blob([new Uint8Array(60)], { type: "audio/flac" }) })],
  ["packet bytes", entry({ blob: new Blob(["SPLALAC1", new Uint8Array(60)]) })],
  ["different quality under the same key", entry({ task: { ...native, quality: "lossless" } })],
  ["different song under the same key", entry({ track: { ...native, selected_id: "other-song" } })],
  ["conflicting transport metadata", entry({ track: requested })],
  ["wrong representation key", entry({ key: cacheKey(requested) })],
]) {
  test(`native cache reuse rejects ${label}`, async () => {
    assert.equal(await reusableNativeFlacCache(value, requested, playable), null);
  });
}

test("FLAC reuse skips bounded comments/artwork metadata before validating the first frame", async () => {
  const first = flacBytes.slice(0, 42); first[4] = 0;
  const withComment = entry({ blob: new Blob([first, new Uint8Array([0x84, 0, 0, 4, 0, 0, 0, 0]), flacBytes.slice(42)]) });
  assert.ok(await reusableNativeFlacCache(withComment, requested, playable));
  const truncated = entry({ blob: new Blob([first, new Uint8Array([0x84, 0xff, 0xff, 0xff]), flacBytes.slice(42)]) });
  assert.equal(await reusableNativeFlacCache(truncated, requested, playable), null);
});

test("a browser without native FLAC support leaves the old Blob untouched", async () => {
  const original = entry();
  assert.equal(await reusableNativeFlacCache(original, requested, () => ""), null);
  assert.equal(original.blob.size, flacBytes.length);
});

function harness(items, { quality = "hires", support = playable } = {}) {
  const reads = [], writes = [], deleted = [];
  let posts = 0;
  const deps = {
    requestedStreamingMode: () => "wasm_alac", status: { quality, setNotice() {} },
    inflightTrackResolutions: new Map(), cacheKey, cacheEntryMatches, hasCachedAudio, audioUrl, trackFromPlayback,
    reusableNativeFlacCache: (item, track) => reusableNativeFlacCache(item, track, support),
    getCachedTrack: async (key) => { reads.push(key); return items.get(key); },
    putCachedTrack: async (key, value) => { writes.push(key); return value; },
    deleteCachedTrack: async (key) => { deleted.push(key); }, refreshLocalCache() {},
    waitForReady: async (value) => value,
    createPlayback: async () => { posts++; throw new Error("offline"); },
    cachedAudioUrl: (item) => item.track.streaming_mode === "wasm_alac" ? "blob:packet" : "blob:flac",
  };
  const resolve = new Function("deps", `const { ${Object.keys(deps).join(", ")} } = deps; return (${resolver.trim()});`)(deps);
  return { resolve, reads, writes, deleted, posts: () => posts };
}

test("WASM request reuses a complete old FLAC offline before any POST", async () => {
  const old = entry();
  const h = harness(new Map([[old.key, old]]));
  const resolved = await h.resolve(requested);
  assert.equal(resolved.source, "blob:flac");
  assert.equal(resolved.track.quality, "hires");
  assert.equal(resolved.track.streaming_mode, "");
  assert.equal(h.posts(), 0);
  assert.deepEqual(h.reads, [cacheKey(requested), cacheKey(native)]);
  assert.deepEqual(h.deleted, []);
  assert.deepEqual(h.writes, [cacheKey(native)]);
});

test("a complete requested packet representation takes priority over legacy FLAC", async () => {
  const old = entry();
  const packet = { key: cacheKey(requested), task: requested, track: requested, blob: new Blob(["packet fixture"]) };
  const h = harness(new Map([[old.key, old], [packet.key, packet]]));
  assert.equal((await h.resolve(requested)).source, "blob:packet");
  assert.deepEqual(h.reads, [packet.key]);
  assert.equal(h.posts(), 0);
  assert.deepEqual(h.deleted, []);
});

test("current playback quality does not reuse a lower-quality legacy Blob", async () => {
  const low = { ...native, quality: "lossless" };
  const old = entry({ key: cacheKey(low), task: low, track: low });
  const h = harness(new Map([[old.key, old]]));
  await assert.rejects(h.resolve(low), /offline/);
  assert.equal(h.posts(), 1);
  assert.ok(h.reads.every((key) => key.includes(":hires")));
  assert.deepEqual(h.deleted, []);
});

test("metadata-only old native cache still negotiates with the server", async () => {
  const old = entry({ blob: null });
  const h = harness(new Map([[old.key, old]]));
  await assert.rejects(h.resolve(requested), /offline/);
  assert.equal(h.posts(), 1);
  assert.deepEqual(h.deleted, []);
});
