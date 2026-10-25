import test from "node:test";
import assert from "node:assert/strict";
import * as Y from "yjs";
import {
  addTaste, appendTracks, applyMergePlan, createPlaylist, deletePlaylist, readLibrary, removeDuplicates, removeTrack, renamePlaylist,
} from "../src/services/sync/library.js";
import { planGuestMerge, planIsEmpty } from "../src/services/sync/merge.js";

const track = (id, title = `歌曲${id}`) => ({ platform: "applemusic", source_id: String(id), title, artist: "歌手", cover: "" });
const titles = (doc, id) => readLibrary(doc).playlists.find((item) => item.id === id).tracks.map((item) => item.title);
const exchange = (left, right) => {
  Y.applyUpdate(right, Y.encodeStateAsUpdate(left, Y.encodeStateVector(right)));
  Y.applyUpdate(left, Y.encodeStateAsUpdate(right, Y.encodeStateVector(left)));
};

test("a replica edited offline after its session expired converges with the cloud", () => {
  const cloud = new Y.Doc();
  createPlaylist(cloud, { id: "p1", name: "通勤", tracks: [track(1), track(2), track(3)], now: 1 });
  const phone = new Y.Doc();
  exchange(cloud, phone);

  // Offline on the phone: remove one, add one, rename.
  removeTrack(phone, "p1", "applemusic:2");
  appendTracks(phone, "p1", [track(4)]);
  renamePlaylist(phone, "p1", "通勤路上");
  // Meanwhile on the laptop (cloud): add a different song and a playlist.
  appendTracks(cloud, "p1", [track(5)]);
  createPlaylist(cloud, { id: "p2", name: "夜跑", tracks: [track(9)], now: 2 });

  exchange(phone, cloud);
  exchange(phone, cloud); // idempotent
  for (const doc of [cloud, phone]) {
    assert.deepEqual(titles(doc, "p1").sort(), ["歌曲1", "歌曲3", "歌曲4", "歌曲5"]);
    assert.equal(readLibrary(doc).playlists.find((item) => item.id === "p1").name, "通勤路上");
    assert.equal(readLibrary(doc).playlists.length, 2);
  }
  assert.deepEqual(titles(cloud, "p1"), titles(phone, "p1"), "same order everywhere");
});

test("the same song added on two devices at once is de-duplicated identically", () => {
  const left = new Y.Doc();
  createPlaylist(left, { id: "p", name: "x", tracks: [track(1)], now: 1 });
  const right = new Y.Doc();
  exchange(left, right);
  appendTracks(left, "p", [track(7)]);
  appendTracks(right, "p", [track(7)]);
  exchange(left, right);
  const raw = (doc) => doc.getMap("playlists").get("p").get("order").toArray();
  assert.equal(raw(left).filter((key) => key === "applemusic:7").length, 2, "CRDT keeps both inserts");
  assert.deepEqual(titles(left, "p"), ["歌曲1", "歌曲7"], "reads already hide the duplicate");
  removeDuplicates(left);
  removeDuplicates(right);
  exchange(left, right);
  assert.deepEqual(raw(left), ["applemusic:1", "applemusic:7"]);
  assert.deepEqual(raw(right), raw(left));
});

test("deleting a playlist on one device wins over stale edits elsewhere only for that playlist", () => {
  const a = new Y.Doc();
  createPlaylist(a, { id: "keep", name: "keep", tracks: [track(1)], now: 1 });
  createPlaylist(a, { id: "drop", name: "drop", tracks: [track(2)], now: 2 });
  const b = new Y.Doc();
  exchange(a, b);
  deletePlaylist(a, "drop");
  appendTracks(b, "keep", [track(3)]);
  exchange(a, b);
  assert.deepEqual(readLibrary(b).playlists.map((item) => item.id), ["keep"]);
  assert.deepEqual(titles(a, "keep"), ["歌曲1", "歌曲3"]);
});

test("taste lists keep the newest first, unique and bounded", () => {
  const doc = new Y.Doc();
  for (let index = 0; index < 90; index += 1) addTaste(doc, "liked", `k${index}`);
  addTaste(doc, "liked", "k5");
  const { liked } = readLibrary(doc);
  assert.equal(liked.length, 80);
  assert.equal(liked[0], "k5");
  assert.equal(new Set(liked).size, liked.length);
});

test("guest merge plan merges look-alike playlists, creates the rest and skips imported ones", () => {
  const cloud = new Y.Doc();
  createPlaylist(cloud, { id: "c1", name: "我的 最爱!", tracks: [track(1), track(2), track(3)], now: 1 });
  createPlaylist(cloud, { id: "c2", name: "工作", tracks: [track(50)], now: 2 });
  addTaste(cloud, "liked", "applemusic:1");
  const guest = {
    playlists: [
      { id: "g1", name: "我的最爱", tracks: [track(1), track(2), track(4)] },   // 2/3 overlap → merge
      { id: "g2", name: "工作", tracks: [track(60), track(61)] },               // same name, no overlap → new
      { id: "g3", name: "旧的", tracks: [track(70)], importedTo: 42 },          // already imported
    ],
    liked: ["applemusic:1", "applemusic:4"],
    less: ["applemusic:99"],
  };
  const plan = planGuestMerge(guest, readLibrary(cloud), 42);
  assert.deepEqual(plan.merge.map((item) => [item.cloudId, item.tracks.map((t) => t.source_id)]), [["c1", ["4"]]]);
  assert.deepEqual(plan.create.map((item) => item.name), ["工作"]);
  assert.deepEqual(plan.importedIds, ["g1", "g2"]);
  assert.deepEqual(plan.liked, ["applemusic:4"]);
  assert.deepEqual(plan.less, ["applemusic:99"]);
  assert.deepEqual(plan.counts, { created: 1, merged: 1, tracks: 3, taste: 2 });

  let next = 0;
  applyMergePlan(cloud, plan, () => `new-${++next}`, 3);
  const library = readLibrary(cloud);
  assert.deepEqual(titles(cloud, "c1"), ["歌曲1", "歌曲2", "歌曲3", "歌曲4"]);
  assert.equal(library.playlists.length, 3);
  assert.deepEqual(library.liked.slice(0, 2), ["applemusic:4", "applemusic:1"]);

  // Logging in again with the guest playlists marked as imported: nothing to do.
  const again = planGuestMerge({ ...guest, playlists: guest.playlists.map((p) => ({ ...p, importedTo: 42 })), liked: [], less: [] }, library, 42);
  assert.ok(planIsEmpty(again));
});
