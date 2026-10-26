import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { audioUrl } from "../src/apis/music.js";
import { trackFromPlayback } from "../src/utils/track.js";
import { cacheKey, cacheEntryMatches, reusableNativeFlacCache, hasCachedAudio, cachedAudioUrl } from "../src/services/localCache.js";

// Exercise the real exported capability decision and App source resolver. Only
// browser codec loading/network/IndexedDB are substituted; the transport policy,
// current-quality selection, cache key and readiness negotiation run unchanged.
const playerSource = (await readFile(new URL("../src/core/player/alac-worklet.js", import.meta.url), "utf8"))
  .replace('import processorUrl from "./alac-processor.js?url";', 'const processorUrl = "test:processor";')
  .replace('from "./audio-output.js";', `from ${JSON.stringify(new URL("../src/core/player/audio-output.js", import.meta.url).href)};`)
  .replace('from "./alac-support.js";', `from ${JSON.stringify(new URL("../src/core/player/alac-support.js", import.meta.url).href)};`)
  .replace('import initAlacWasm, { AlacDecoder } from "../../wasm/alac-decoder/alac_decoder.js";', "const initAlacWasm = async () => {}; class AlacDecoder {}");
const { canRequestAlacWorklet, canUseAlacWorklet } = await import(`data:text/javascript;base64,${Buffer.from(playerSource).toString("base64")}`);
const appSource = await readFile(new URL("../src/App.vue", import.meta.url), "utf8");
function extract(startMarker, endMarker) {
  const start = appSource.indexOf(startMarker);
  const end = appSource.indexOf(endMarker, start);
  assert.ok(start >= 0 && end > start, `App resolver boundary missing: ${startMarker}`);
  return appSource.slice(start, end).trim();
}
const modeSource = extract("function requestedStreamingMode(", "\nfunction loadRecentTracks(");
const readySource = extract("async function waitForReady(", "\n/**");
const resolverSource = extract("async function resolveTrackSource(", "\n// Cold playback");

function environment(t, { online = true, audioSession } = {}) {
  class Context { get audioWorklet() { return {}; } }
  const replacements = {
    navigator: {
      onLine: online,
      userAgentData: { platform: "Android" },
      userAgent: "Mozilla/5.0 (Linux; Android 15) Chrome/130.0 Mobile Safari/537.36",
      ...(audioSession ? { audioSession } : {}),
    },
    window: { AudioContext: Context, setTimeout: (resolve) => queueMicrotask(resolve) },
    AudioWorkletNode: class {},
    document: { createElement: () => ({ canPlayType: (type) => type === "audio/flac" ? "probably" : "" }) },
  };
  for (const [name, value] of Object.entries(replacements)) {
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, name);
    Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
    t.after(() => { if (descriptor) Object.defineProperty(globalThis, name, descriptor); else delete globalThis[name]; });
  }
}

function resolverFixture() {
  const status = { quality: "hires", setNotice() {} };
  const requestedStreamingMode = new Function("status", "canRequestAlacWorklet", `return (${modeSource});`)(status, canRequestAlacWorklet);
  const track = Object.freeze({
    id: "applemusic--synthetic--lossless", platform: "applemusic", source_id: "synthetic", selected_id: "synthetic",
    title: "Synthetic audio", artist: "Test signal", quality: "lossless", streaming_mode: "wasm_alac",
  });
  const packetTask = {
    ...track, id: "applemusic--synthetic--hires", track_id: "synthetic", quality: "hires", status: "completed",
    streaming_mode: "wasm_alac", streaming_url: "/api/v1/playbacks/synthetic/audio/stream",
    streaming_codec: "alac", streaming_protocol: "splayer-alac-v2", audio_url: "/api/v1/playbacks/synthetic/audio",
    format: "m4a", content_type: "application/x-splayer-alac",
  };
  const packetBytes = new Uint8Array(48);
  packetBytes.set(new TextEncoder().encode("SPLALAC1"));
  new DataView(packetBytes.buffer).setUint32(8, 2);
  const packetBlob = new Blob([packetBytes], { type: "application/x-splayer-alac" });
  const packetKey = cacheKey(packetTask);
  const packetEntry = { key: packetKey, task: packetTask, track: packetTask, blob: packetBlob };
  const entries = new Map([[packetKey, packetEntry]]);
  const creations = [];
  const reads = [];
  let polls = 0;
  const getPlayback = async () => {
    polls += 1;
    return { ...packetTask, status: "completed", format: "flac", content_type: "audio/flac" };
  };
  const waitForReady = new Function("getPlayback", "window", `return (${readySource});`)(getPlayback, window);
  const deps = {
    requestedStreamingMode, status, canRequestAlacWorklet,
    inflightTrackResolutions: new Map(), cacheKey, cacheEntryMatches, reusableNativeFlacCache,
    audioUrl, trackFromPlayback, hasCachedAudio, cachedAudioUrl, getPlayback, waitForReady,
    getCachedTrack: async (key) => { reads.push(key); return entries.get(key); },
    putCachedTrack: async (key, value) => { const stored = { ...entries.get(key), ...value, key }; entries.set(key, stored); return stored; },
    deleteCachedTrack: async (key) => { entries.delete(key); },
    refreshLocalCache() {},
    createPlayback: async (request) => {
      creations.push(request);
      // Another WASM client may own this shared task. Native readiness must
      // still wait for and select /audio, without inheriting /audio/stream.
      return { playback: { ...packetTask, status: "streaming" } };
    },
  };
  const resolve = new Function("deps", `const { ${Object.keys(deps).join(", ")} } = deps; return (${resolverSource});`)(deps);
  return { resolve, track, requestedStreamingMode, creations, reads, entries, packetEntry, packetKey, packetBlob, polls: () => polls };
}

test("online Android requests native Hi-Res using current quality and preserves existing WASM cache", async (t) => {
  environment(t);
  const h = resolverFixture();
  assert.equal(h.requestedStreamingMode(h.track), "");
  const resolved = await h.resolve(h.track);
  assert.equal(h.creations.length, 1);
  assert.equal(h.creations[0].quality, "hires", "the current playback setting wins over imported lossless metadata");
  assert.equal(h.creations[0].streamingMode, null);
  assert.equal(h.creations[0].selectedId, "synthetic");
  assert.equal(h.polls(), 1, "native playback waits for the completed derivative of a shared WASM task");
  assert.equal(resolved.track.quality, "hires");
  assert.equal(resolved.track.streaming_mode, "");
  assert.equal(resolved.track.format, "flac");
  assert.equal(resolved.source, "/api/v1/playbacks/synthetic/audio?v=applemusic--synthetic--hires");
  assert.equal(canUseAlacWorklet(resolved.track), false);
  assert.equal(h.track.quality, "lossless", "source metadata is never mutated by platform negotiation");
  assert.equal(h.entries.get(h.packetKey), h.packetEntry, "online native negotiation does not delete the offline packet cache");
  assert.equal(h.reads[0], "applemusic:synthetic:hires");
});

test("offline Android reuses its cached Hi-Res packet Blob without provider requests", async (t) => {
  environment(t, { online: false });
  const h = resolverFixture();
  assert.equal(h.requestedStreamingMode(h.track), "wasm_alac");
  const resolved = await h.resolve(h.track);
  t.after(() => URL.revokeObjectURL(resolved.source));
  assert.match(resolved.source, /^blob:/);
  assert.equal(h.creations.length, 0);
  assert.equal(h.polls(), 0);
  assert.equal(h.reads[0], h.packetKey);
  assert.equal(resolved.track.quality, "hires");
  assert.equal(resolved.track.streaming_mode, "wasm_alac");
  assert.equal(canUseAlacWorklet(resolved.track), true, "the cached packet representation remains playable by WASM");
  assert.equal(h.entries.get(h.packetKey).blob, h.packetBlob);
});

test("Android exposing Audio Session still negotiates native playback online and preserves offline WASM bytes", async (t) => {
  environment(t, { audioSession: { type: "playback" } });
  const h = resolverFixture();
  const resolved = await h.resolve(h.track);
  assert.match(resolved.source, /\/audio\?/);
  assert.equal(h.creations.length, 1);
  assert.equal(h.creations[0].streamingMode, null);
  assert.equal(h.polls(), 1);
  assert.equal(resolved.track.quality, "hires");
  assert.equal(canUseAlacWorklet(resolved.track), false);
  assert.equal(h.entries.get(h.packetKey).blob, h.packetBlob);
});
