// The account library as a Yjs document:
//
//   playlists: Y.Map<id, Y.Map{ name, platform, source_id, source_url, cover,
//                               created_at, updated_at, order: Y.Array<key> }>
//   tracks:    Y.Map<key, { platform, source_id, title, artist, cover }>
//   taste:     Y.Map{ liked: Y.Array<key>, less: Y.Array<key> }
//
// Keys are trackIdentity() values. Quality is never stored: playback always
// uses the quality selected at play time.
import * as Y from "yjs";
import { trackIdentity } from "../../stores/queue.js";

export const LOCAL = Symbol("local-edit");
const TASTE_LIMIT = 80;

const roots = (doc) => ({
  playlists: doc.getMap("playlists"),
  tracks: doc.getMap("tracks"),
  taste: doc.getMap("taste"),
});

function trackMeta(track) {
  const key = trackIdentity(track);
  if (!key) return null;
  const source = track.source_id || track.selected_id || track.track_id
    || (typeof track.id === "string" && track.id.includes("--") ? track.id.split("--").slice(1, -1).join("--") : track.id);
  return {
    key,
    meta: {
      platform: track.platform || "",
      source_id: String(source || ""),
      title: track.title || track.song || "",
      artist: track.artist || track.singer || "",
      cover: track.cover || track.cover_url || "",
    },
  };
}

function rememberTracks(tracksMap, tracks) {
  const keys = [];
  for (const track of tracks || []) {
    const entry = track && trackMeta(track);
    if (!entry) continue;
    const existing = tracksMap.get(entry.key);
    if (!existing || existing.title !== entry.meta.title || existing.cover !== entry.meta.cover) tracksMap.set(entry.key, entry.meta);
    keys.push(entry.key);
  }
  return keys;
}

function tasteList(taste, kind) {
  let list = taste.get(kind);
  if (!(list instanceof Y.Array)) {
    list = new Y.Array();
    taste.set(kind, list);
  }
  return list;
}

/** Plain snapshot for the UI; newest playlists first, like local storage. */
export function readLibrary(doc) {
  const { playlists, tracks, taste } = roots(doc);
  const toTrack = (key) => {
    const meta = tracks.get(key);
    return meta ? { ...meta, key } : null;
  };
  const items = [];
  playlists.forEach((entry, id) => {
    if (!(entry instanceof Y.Map)) return;
    const order = entry.get("order");
    const seen = new Set();
    const list = [];
    for (const key of order instanceof Y.Array ? order.toArray() : []) {
      if (seen.has(key)) continue;
      seen.add(key);
      const track = toTrack(key);
      if (track) list.push(track);
    }
    items.push({
      id,
      name: entry.get("name") || "未命名歌单",
      platform: entry.get("platform") || "",
      source_id: entry.get("source_id") || "",
      source_url: entry.get("source_url") || "",
      cover: entry.get("cover") || list[0]?.cover || "",
      created_at: entry.get("created_at") || 0,
      updated_at: entry.get("updated_at") || 0,
      tracks: list,
    });
  });
  items.sort((left, right) => right.created_at - left.created_at || (left.id < right.id ? -1 : 1));
  const keysOf = (kind) => {
    const list = taste.get(kind);
    return list instanceof Y.Array ? [...new Set(list.toArray())] : [];
  };
  return { playlists: items, liked: keysOf("liked"), less: keysOf("less") };
}

export function createPlaylist(doc, { id, name, platform = "", sourceId = "", sourceUrl = "", cover = "", tracks = [], now = Date.now() }) {
  doc.transact(() => {
    const { playlists, tracks: tracksMap } = roots(doc);
    const keys = [...new Set(rememberTracks(tracksMap, tracks))];
    const entry = new Y.Map();
    entry.set("name", String(name || "未命名歌单").trim() || "未命名歌单");
    entry.set("platform", platform);
    entry.set("source_id", sourceId);
    entry.set("source_url", sourceUrl);
    entry.set("cover", cover || tracks[0]?.cover || "");
    entry.set("created_at", now);
    entry.set("updated_at", now);
    const order = new Y.Array();
    order.push(keys);
    entry.set("order", order);
    playlists.set(id, entry);
  }, LOCAL);
}

function playlistEntry(doc, id) {
  const entry = roots(doc).playlists.get(id);
  return entry instanceof Y.Map ? entry : null;
}

/** Append tracks that are not in the playlist yet; returns how many. */
export function appendTracks(doc, id, tracks, now = Date.now()) {
  let added = 0;
  doc.transact(() => {
    const entry = playlistEntry(doc, id);
    const order = entry?.get("order");
    if (!(order instanceof Y.Array)) return;
    const present = new Set(order.toArray());
    const keys = rememberTracks(roots(doc).tracks, tracks).filter((key) => !present.has(key) && present.add(key));
    if (!keys.length) return;
    order.push(keys);
    if (!entry.get("cover")) entry.set("cover", tracks[0]?.cover || "");
    entry.set("updated_at", now);
    added = keys.length;
  }, LOCAL);
  return added;
}

export function renamePlaylist(doc, id, name, now = Date.now()) {
  doc.transact(() => {
    const entry = playlistEntry(doc, id);
    if (!entry) return;
    entry.set("name", name);
    entry.set("updated_at", now);
  }, LOCAL);
}

export function deletePlaylist(doc, id) {
  doc.transact(() => roots(doc).playlists.delete(id), LOCAL);
}

export function removeTrack(doc, id, key, now = Date.now()) {
  doc.transact(() => {
    const entry = playlistEntry(doc, id);
    const order = entry?.get("order");
    if (!(order instanceof Y.Array)) return;
    const values = order.toArray();
    for (let index = values.length - 1; index >= 0; index -= 1) {
      if (values[index] === key) order.delete(index, 1);
    }
    entry.set("updated_at", now);
  }, LOCAL);
}

/** Most recent first, unique, bounded. */
export function addTaste(doc, kind, key) {
  doc.transact(() => {
    const list = tasteList(roots(doc).taste, kind);
    const values = list.toArray();
    for (let index = values.length - 1; index >= 0; index -= 1) {
      if (values[index] === key) list.delete(index, 1);
    }
    list.insert(0, [key]);
    if (list.length > TASTE_LIMIT) list.delete(TASTE_LIMIT, list.length - TASTE_LIMIT);
  }, LOCAL);
}

/**
 * Two devices adding the same song concurrently leaves it twice in an order
 * array. Every replica removes later duplicates the same way, so concurrent
 * clean-ups converge (deleting an already deleted item is a no-op).
 */
export function removeDuplicates(doc) {
  let removed = 0;
  doc.transact(() => {
    const { playlists, taste } = roots(doc);
    const dedupe = (list) => {
      if (!(list instanceof Y.Array)) return;
      const values = list.toArray();
      for (let index = values.length - 1; index >= 0; index -= 1) {
        if (values.indexOf(values[index]) !== index) { list.delete(index, 1); removed += 1; }
      }
    };
    playlists.forEach((entry) => entry instanceof Y.Map && dedupe(entry.get("order")));
    dedupe(taste.get("liked"));
    dedupe(taste.get("less"));
  }, LOCAL);
  return removed;
}

/** Apply a guest merge plan (see merge.js) in one transaction. */
export function applyMergePlan(doc, plan, newId, now = Date.now()) {
  doc.transact(() => {
    for (const playlist of plan.create) {
      createPlaylist(doc, { ...playlist, id: newId(), now });
    }
    for (const { cloudId, tracks } of plan.merge) appendTracks(doc, cloudId, tracks, now);
    for (const key of [...plan.liked].reverse()) addTaste(doc, "liked", key);
    for (const key of [...plan.less].reverse()) addTaste(doc, "less", key);
  }, LOCAL);
}
