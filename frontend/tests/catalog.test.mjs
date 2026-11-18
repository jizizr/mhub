import test from "node:test";
import assert from "node:assert/strict";
import {
  albumSummary,
  cardSubtitle,
  discGroups,
  formatReleaseDate,
  loadCatalogPage,
  pageForItem,
  pageForTrack,
  pageKey,
  platformHas,
  visibleSections,
} from "../src/services/catalog.js";
import { trackFromCatalog } from "../src/utils/track.js";

const song = (id, extra = {}) => ({ type: "song", platform: "applemusic", id, title: `t${id}`, subtitle: "周杰伦", cover: "c", album: "范特西", ...extra });

test("tabs follow the platform's sections that found something", () => {
  assert.deepEqual(visibleSections({ sections: ["songs"], songs: [song("1")] }), []);
  assert.deepEqual(
    visibleSections({ sections: ["top", "artists", "albums", "songs", "playlists"], top: [song("1")], artists: [], albums: [{}], songs: [song("1")], playlists: [] }),
    ["top", "albums", "songs"],
  );
  assert.deepEqual(visibleSections(null), []);
});

test("only platforms listing a section have its pages", () => {
  const platforms = [{ id: "applemusic", catalog: ["top", "artists", "albums", "songs", "playlists"] }, { id: "tencent", catalog: ["top", "albums", "songs", "playlists"] }, { id: "bilibili" }];
  assert.equal(platformHas(platforms, "applemusic", "artists"), true);
  assert.equal(platformHas(platforms, "tencent", "albums"), true);
  assert.equal(platformHas(platforms, "tencent", "artists"), false);
  assert.equal(platformHas(platforms, "bilibili", "albums"), false);
  assert.equal(platformHas(platforms, "youtubemusic", "albums"), false);
});

test("catalog songs keep album, duration and track number", () => {
  const track = trackFromCatalog(song("536115195", { duration_ms: 297200, track_number: 2, disc_number: 1 }), "lossless");
  assert.equal(track.id, "applemusic--536115195--lossless");
  assert.equal(track.title, "t536115195");
  assert.equal(track.artist, "周杰伦");
  assert.equal(track.album, "范特西");
  assert.equal(track.duration, 297200);
  assert.equal(track.track_number, 2);
});

test("card subtitles name the kind in best results and the year for albums", () => {
  assert.equal(cardSubtitle({ type: "album", subtitle: "周杰伦", release_date: "2001-09-14" }), "2001 · 周杰伦");
  assert.equal(cardSubtitle({ type: "album", subtitle: "周杰伦", release_date: "2001-09-14" }, { withKind: true }), "专辑 · 周杰伦");
  assert.equal(cardSubtitle({ type: "artist" }, { withKind: true }), "艺人");
});

test("a song's album page is found through its track info", async () => {
  const calls = [];
  const api = {
    getTrackInfo: async (args) => { calls.push(["info", args.id]); return { album_id: "536114662", artist_id: "300117743" }; },
    getAlbum: async (args) => { calls.push(["album", args.id]); return { id: args.id, title: "七里香", tracks: [song("1", { track_number: 1 })] }; },
  };
  const page = pageForTrack("album", { platform: "applemusic", source_id: "536115195", album: "七里香", artist: "周杰伦", cover: "c" });
  assert.equal(page.title, "七里香");
  const data = await loadCatalogPage(page, { api, quality: "high" });
  assert.deepEqual(calls, [["info", "536115195"], ["album", "536114662"]]);
  assert.equal(data.tracks[0].track_number, 1);
  assert.equal(pageKey(page), "album:applemusic:track:536115195");
});

test("a song without an album says so", async () => {
  const api = { getTrackInfo: async () => ({ album_id: "", artist_id: "" }) };
  await assert.rejects(loadCatalogPage(pageForTrack("artist", { platform: "tencent", source_id: "x" }), { api }), /艺人/);
});

test("playlist cards open through their link", async () => {
  let resolved = "";
  const api = { resolveCollection: async ({ url }) => { resolved = url; return { id: "7", title: "歌单", source_url: url, items: [{ platform: "tencent", id: "a", song: "s", singer: "x", cover: "" }] }; } };
  const page = pageForItem({ type: "playlist", platform: "tencent", id: "7", title: "歌单", url: "https://y.qq.com/n/ryqq/playlist/7" });
  const data = await loadCatalogPage(page, { api });
  assert.equal(resolved, "https://y.qq.com/n/ryqq/playlist/7");
  assert.equal(data.tracks[0].title, "s");
});

test("album footer text", () => {
  assert.equal(formatReleaseDate("2001-09-14"), "2001年9月14日");
  assert.equal(formatReleaseDate("2004"), "2004年");
  assert.equal(formatReleaseDate(""), "");
  assert.equal(albumSummary([{ duration: 234253 }, { duration: 235000 }]), "2 首歌曲，8 分钟");
  assert.equal(albumSummary(Array.from({ length: 30 }, () => ({ duration: 180000 }))), "30 首歌曲，1 小时 30 分钟");
  assert.equal(albumSummary([]), "");
});

test("discs split only when an album has several", () => {
  assert.deepEqual(discGroups([{ disc_number: 1 }, { disc_number: 1 }]).map((group) => [group.disc, group.tracks.length]), [[0, 2]]);
  assert.deepEqual(discGroups([{ disc_number: 1 }, { disc_number: 2 }, { disc_number: 2 }]).map((group) => [group.disc, group.tracks.length]), [[1, 1], [2, 2]]);
});
