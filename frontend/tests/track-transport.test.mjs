import test from "node:test";
import assert from "node:assert/strict";
import { trackFromPlayback } from "../src/utils/track.js";
import { cacheKey, cacheEntryMatches } from "../src/services/localCache.js";

const requested = {
  platform: "applemusic", source_id: "song-123", quality: "lossless",
  title: "Test track", streaming_mode: "wasm_alac", streaming_url: "/old/packets",
  streaming_codec: "alac", streaming_protocol: "splayer-alac-v2",
};

for (const mode of [null, "", undefined]) {
  test(`native response (${String(mode)}) clears a previously requested WASM transport`, () => {
    const task = { id: "playback-123", platform: "applemusic", track_id: "song-123", quality: "lossless", streaming_mode: mode, audio_url: "/audio", status: "completed" };
    const track = trackFromPlayback(task, requested);
    assert.equal(track.streaming_mode, "");
    assert.equal(track.streaming_url, "");
    assert.equal(track.streaming_codec, "");
    assert.equal(track.streaming_protocol, "");
    assert.equal(track.audio_url, "/audio");
    assert.equal(track.title, requested.title);
    assert.equal(cacheKey(track), "applemusic:song-123:lossless");
    assert.equal(cacheEntryMatches({ task, track }, task), true);
    assert.equal(cacheEntryMatches({ task, track }, requested), false);
  });
}

test("confirmed WASM response keeps its separate packet cache identity", () => {
  const task = { ...requested, streaming_url: "/current/packets", id: "playback-123", track_id: "song-123" };
  const track = trackFromPlayback(task, { ...requested, streaming_mode: "", quality: "high" });
  assert.equal(track.streaming_mode, "wasm_alac");
  assert.equal(track.streaming_url, "/current/packets");
  assert.equal(track.quality, "lossless");
  assert.equal(cacheKey(track), "applemusic:song-123:lossless:wasm_alac_v2");
});

test("metadata-only fallback without a server response preserves its existing transport", () => {
  const track = trackFromPlayback(null, requested);
  assert.equal(track.streaming_mode, "wasm_alac");
  assert.equal(track.streaming_url, "/old/packets");
});
