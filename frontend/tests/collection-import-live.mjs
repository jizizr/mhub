// Run against an already running API; only in-memory browser storage is used.
// MUSIC_IMPORT_SOURCE_URL=https://music.163.com/playlist?id=... node tests/collection-import-live.mjs
import assert from "node:assert/strict";
import { createServer } from "vite";
import { createPinia, setActivePinia } from "pinia";

const apiBase = process.env.MUSIC_IMPORT_API_BASE || "http://127.0.0.1:8000";
const sourceUrl = process.env.MUSIC_IMPORT_SOURCE_URL;
assert.ok(sourceUrl, "Set MUSIC_IMPORT_SOURCE_URL to the NetEase playlist being verified");
const sourceId = new URL(sourceUrl.replace("/#/", "/")).searchParams.get("id");
assert.ok(sourceId && /^\d+$/.test(sourceId), "Source URL must contain a numeric playlist ID");
const originalFetch = globalThis.fetch;
const sourceResponse = await originalFetch(`https://music.163.com/api/v6/playlist/detail?id=${sourceId}`, {
  headers: { Referer: "https://music.163.com/", "User-Agent": "Mozilla/5.0" },
});
assert.equal(sourceResponse.status, 200);
const source = (await sourceResponse.json()).playlist;
const expectedIds = source.trackIds.map((item) => String(item.id));
assert.equal(expectedIds.length, source.trackCount);
globalThis.fetch = (url, options) => originalFetch(new URL(url, apiBase), options);
const storage = new Map();
globalThis.localStorage = {
  getItem: (key) => storage.get(key) ?? null,
  setItem: (key, value) => storage.set(key, value),
  removeItem: (key) => storage.delete(key),
};
const server = await createServer({ server: { middlewareMode: true }, appType: "custom" });
try {
  const { resolveCollection, matchCollection } = await server.ssrLoadModule("/src/apis/music.js");
  const { trackFromSearch } = await server.ssrLoadModule("/src/utils/track.js");
  const { usePlaylistStore } = await server.ssrLoadModule("/src/stores/playlists.js");
  const collection = await resolveCollection({ platform: "netease", url: sourceUrl });
  assert.deepEqual(collection.items.map((item) => item.id), expectedIds);
  const matched = await matchCollection({ sourcePlatform: collection.platform, targetPlatform: "netease", items: collection.items });
  assert.equal(matched.matched_count, expectedIds.length);
  assert.equal(matched.review_count + matched.missing_count, 0);
  const tracks = matched.items.map((item) => trackFromSearch(item.candidates[0], "lossless"));
  setActivePinia(createPinia());
  const playlist = usePlaylistStore().create({ name: "Import verification fixture", tracks, platform: "netease" });
  assert.deepEqual(playlist.tracks.map((track) => track.source_id), expectedIds);
  setActivePinia(createPinia());
  const restored = usePlaylistStore().items[0];
  assert.deepEqual(restored.tracks.map((track) => track.source_id), expectedIds);
  console.log(JSON.stringify({
    source_track_count: source.trackCount,
    source_embedded_tracks: source.tracks.length,
    source_full_ids: expectedIds.length,
    resolved: collection.items.length,
    matched: matched.matched_count,
    review: matched.review_count,
    missing: matched.missing_count,
    persisted: playlist.tracks.length,
    restored: restored.tracks.length,
    source_order_preserved: true,
  }, null, 2));
} finally {
  await server.close();
  globalThis.fetch = originalFetch;
}
