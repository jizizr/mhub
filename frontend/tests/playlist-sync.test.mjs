import test from "node:test";
import assert from "node:assert/strict";
import * as Y from "yjs";
import { canSyncPlaylist, isSyncDue, planPlaylistSync, platformFromUrl, syncedTracks } from "../src/services/playlist-sync.js";
import { createPlaylist, readLibrary, syncPlaylist } from "../src/services/sync/library.js";
import * as library from "../src/services/sync/library.js";

const track = (id) => ({ platform: "applemusic", source_id: String(id), id: `applemusic--${id}--high`, title: `歌曲${id}`, artist: "歌手", cover: "" });
const key = (id) => `applemusic:${id}`;
const keys = (...ids) => ids.map(key);

test("source additions land next to their source neighbours and removals follow the source", () => {
  const plan = planPlaylistSync({ local: keys(1, 2, 3), base: keys(1, 2, 3), upstream: keys(9, 1, 3, 4) });
  assert.deepEqual(plan.remove, [key(2)]);
  assert.deepEqual(plan.next, keys(9, 1, 3, 4));
  assert.deepEqual(plan.insert, [{ key: key(9), after: null }, { key: key(4), after: key(3) }]);
});

test("the user's own additions and removals survive a sync", () => {
  // The user removed 2 and added 7 locally; the source added 5.
  const plan = planPlaylistSync({ local: keys(1, 7, 3), base: keys(1, 2, 3), upstream: keys(1, 2, 3, 5) });
  assert.deepEqual(plan.remove, []);
  assert.deepEqual(plan.next, keys(1, 7, 3, 5), "2 stays removed, 7 stays");
});

test("syncing twice with no source change does nothing", () => {
  const first = planPlaylistSync({ local: keys(1, 2), base: keys(1, 2), upstream: keys(1, 2, 3) });
  const second = planPlaylistSync({ local: first.next, base: keys(1, 2, 3), upstream: keys(1, 2, 3) });
  assert.deepEqual([second.remove, second.insert], [[], []]);
});

test("a playlist imported before syncing existed never loses songs on its first sync", () => {
  const plan = planPlaylistSync({ local: keys(1, 2, 8), base: null, upstream: keys(1, 3) });
  assert.deepEqual(plan.remove, []);
  assert.deepEqual(plan.next, keys(1, 3, 2, 8));
});

test("only same-platform imports with a link sync, once a day", () => {
  const apple = { platform: "applemusic", source_url: "https://music.apple.com/cn/playlist/x/pl.1", created_at: 0 };
  assert.equal(platformFromUrl("https://y.qq.com/n/ryqq/playlist/1"), "tencent");
  assert.equal(canSyncPlaylist(apple), true);
  assert.equal(canSyncPlaylist({ ...apple, platform: "netease" }), false, "cross-platform import");
  assert.equal(canSyncPlaylist({ platform: "applemusic", source_url: "" }), false);
  assert.equal(isSyncDue({ ...apple, synced_at: 1000 }, 1000 + 60_000), false);
  assert.equal(isSyncDue({ ...apple, synced_at: 1000 }, 1000 + 25 * 3600_000), true);
});

test("account library: sync edits merge with a concurrent edit on another device", () => {
  const laptop = new Y.Doc();
  createPlaylist(laptop, { id: "p", name: "源歌单", platform: "applemusic", sourceUrl: "https://music.apple.com/x", sourcePlatform: "applemusic", sourceKeys: keys(1, 2), tracks: [track(1), track(2)], now: 1 });
  const phone = new Y.Doc();
  Y.applyUpdate(phone, Y.encodeStateAsUpdate(laptop));
  // The phone adds a song while the laptop syncs (source dropped 2, added 3).
  library.appendTracks(phone, "p", [track(7)]);
  const plan = planPlaylistSync({ local: keys(1, 2), base: keys(1, 2), upstream: keys(1, 3) });
  syncPlaylist(laptop, "p", { tracks: [track(1), track(3)], plan, sourceKeys: keys(1, 3), sourcePlatform: "applemusic", now: 5 });
  Y.applyUpdate(phone, Y.encodeStateAsUpdate(laptop));
  Y.applyUpdate(laptop, Y.encodeStateAsUpdate(phone));
  for (const doc of [laptop, phone]) {
    const playlist = readLibrary(doc).playlists[0];
    assert.deepEqual(playlist.tracks.map((item) => item.key).sort(), keys(1, 3, 7).sort());
    assert.deepEqual(playlist.source_keys, keys(1, 3));
    assert.equal(playlist.synced_at, 5);
  }
});

test("guest playlists keep their track objects and gain the source's new ones", () => {
  const local = [track(2), track(7)];
  const plan = planPlaylistSync({ local: keys(2, 7), base: keys(1, 2), upstream: keys(1, 2, 3) });
  const next = syncedTracks(local, [track(1), track(2), track(3)], plan, (item) => `${item.platform}:${item.source_id}`);
  assert.deepEqual(next.map((item) => item.source_id), ["2", "3", "7"], "1 stays removed; 3 lands after its source neighbour 2");
  assert.equal(next[0], local[0], "existing entries are kept as they are");
});
