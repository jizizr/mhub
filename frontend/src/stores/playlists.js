import { defineStore } from "pinia";
import { markRaw } from "vue";
import { trackIdentity } from "./queue";
import { trackFromSearch } from "../utils/track.js";
import { resolveCollection } from "../apis/music.js";
import { canSyncPlaylist, isSyncDue, planPlaylistSync, sourcePlatformOf, syncedTracks } from "../services/playlist-sync.js";

const STORAGE_KEY = "music-web-splayer-playlists-v1";
const TASTE_KEYS = { liked: "music-web-splayer-recommendation-liked-v1", less: "music-web-splayer-recommendation-less-v1" };
const TASTE_LIMIT = 80;

// Two storage back ends behind one API. Guests keep the original
// localStorage format; a device linked to an account reads and writes the
// account's Yjs replica (services/sync). Guest data stays untouched while
// linked, so signing out returns to it.
let cloud = null; // { doc, ops }

function restoreTaste(kind) {
  try {
    const value = JSON.parse(localStorage.getItem(TASTE_KEYS[kind]) || "[]");
    return Array.isArray(value) ? value : [];
  } catch {
    return [];
  }
}

function cloudTrack(track) {
  return trackFromSearch({ platform: track.platform, id: track.source_id, song: track.title, singer: track.artist, cover: track.cover }, "high");
}

function restore() {
  try {
    const value = JSON.parse(localStorage.getItem(STORAGE_KEY) || "[]");
    return Array.isArray(value) ? value : [];
  } catch {
    return [];
  }
}

function makeId() {
  if (globalThis.crypto?.randomUUID) return globalThis.crypto.randomUUID();
  return `playlist-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function uniqueTracks(items) {
  const seen = new Set();
  return (items || []).filter((track) => {
    const key = trackIdentity(track);
    if (!key || seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function normalizePlaylist(item) {
  return {
    id: item.id || makeId(),
    name: String(item.name || "未命名歌单").trim() || "未命名歌单",
    platform: item.platform || "",
    source_id: item.source_id || "",
    source_url: item.source_url || "",
    source_platform: item.source_platform || "",
    // Source keys at the last sync (null for imports made before syncing).
    source_keys: Array.isArray(item.source_keys) ? item.source_keys : null,
    synced_at: item.synced_at || 0,
    cover: item.cover || item.tracks?.[0]?.cover || "",
    tracks: uniqueTracks(item.tracks),
    created_at: item.created_at || Date.now(),
    updated_at: item.updated_at || Date.now(),
  };
}

export const usePlaylistStore = defineStore("playlists", {
  state: () => ({
    items: restore().map(normalizePlaylist),
    liked: restoreTaste("liked"),
    less: restoreTaste("less"),
    mode: "local",
    /** Ids of playlists syncing from their source right now. */
    syncing: [],
  }),
  getters: {
    count: (state) => state.items.length,
    getById: (state) => (id) => state.items.find((item) => item.id === id) || null,
  },
  actions: {
    save() {
      if (cloud) return;
      // Keep the import markers of guest playlists already merged into an account.
      const imported = new Map(restore().filter((item) => item.importedTo).map((item) => [item.id, item.importedTo]));
      localStorage.setItem(STORAGE_KEY, JSON.stringify(this.items.map((item) => (
        imported.has(item.id) ? { ...item, importedTo: imported.get(item.id) } : item))));
    },
    /** Switch to an account replica; `ops` is services/sync/library.js. */
    attachCloud(doc, ops) {
      cloud = { doc: markRaw(doc), ops };
      this.mode = "cloud";
      this.refreshCloud();
    },
    refreshCloud() {
      if (!cloud) return;
      const snapshot = cloud.ops.readLibrary(cloud.doc);
      this.items = snapshot.playlists.map((playlist) => ({ ...playlist, tracks: playlist.tracks.map(cloudTrack) }));
      this.liked = snapshot.liked;
      this.less = snapshot.less;
    },
    detachCloud() {
      cloud = null;
      this.mode = "local";
      this.items = restore().map(normalizePlaylist);
      this.liked = restoreTaste("liked");
      this.less = restoreTaste("less");
    },
    /** Guest data for merging into an account (raw, with import markers). */
    guestData() {
      return { playlists: restore().map((item) => ({ ...normalizePlaylist(item), importedTo: item.importedTo })), liked: restoreTaste("liked"), less: restoreTaste("less") };
    },
    markGuestImported(ids, accountId) {
      const marked = new Set(ids);
      localStorage.setItem(STORAGE_KEY, JSON.stringify(restore().map((item) => (
        marked.has(item.id) ? { ...item, importedTo: accountId } : item))));
    },
    addTaste(kind, key) {
      if (!key || !TASTE_KEYS[kind]) return;
      if (cloud) {
        cloud.ops.addTaste(cloud.doc, kind, key);
        this.refreshCloud();
        return;
      }
      this[kind] = [key, ...this[kind].filter((item) => item !== key)].slice(0, TASTE_LIMIT);
      localStorage.setItem(TASTE_KEYS[kind], JSON.stringify(this[kind]));
    },
    create({ name, tracks, platform, sourceId, sourceUrl, sourcePlatform, sourceKeys }) {
      if (cloud) {
        const id = makeId();
        cloud.ops.createPlaylist(cloud.doc, {
          id, name, tracks: uniqueTracks(tracks), platform: platform || "", sourceId: sourceId || "", sourceUrl: sourceUrl || "",
          sourcePlatform: sourcePlatform || "", sourceKeys: sourceKeys || null,
        });
        this.refreshCloud();
        return this.getById(id);
      }
      const now = Date.now();
      const playlist = normalizePlaylist({
        id: makeId(),
        name,
        tracks,
        platform,
        source_id: sourceId,
        source_url: sourceUrl,
        source_platform: sourcePlatform,
        source_keys: sourceKeys,
        synced_at: sourceKeys ? now : 0,
        created_at: now,
        updated_at: now,
      });
      this.items.unshift(playlist);
      this.save();
      return playlist;
    },
    merge(id, tracks) {
      if (cloud) {
        cloud.ops.appendTracks(cloud.doc, id, tracks || []);
        this.refreshCloud();
        return this.getById(id);
      }
      const playlist = this.getById(id);
      if (!playlist) return null;
      playlist.tracks = uniqueTracks([...playlist.tracks, ...(tracks || [])]);
      playlist.cover ||= playlist.tracks[0]?.cover || "";
      playlist.updated_at = Date.now();
      this.save();
      return playlist;
    },
    rename(id, name) {
      const playlist = this.getById(id);
      const nextName = String(name || "").trim();
      if (!playlist || !nextName) return null;
      if (cloud) {
        cloud.ops.renamePlaylist(cloud.doc, id, nextName);
        this.refreshCloud();
        return this.getById(id);
      }
      playlist.name = nextName;
      playlist.updated_at = Date.now();
      this.save();
      return playlist;
    },
    removeTrack(id, track) {
      const playlist = this.getById(id);
      const key = trackIdentity(track);
      if (!playlist || !key) return null;
      if (cloud) {
        if (!playlist.tracks.some((item) => trackIdentity(item) === key)) return null;
        cloud.ops.removeTrack(cloud.doc, id, key);
        this.refreshCloud();
        return this.getById(id);
      }
      const before = playlist.tracks.length;
      playlist.tracks = playlist.tracks.filter((item) => trackIdentity(item) !== key);
      if (playlist.tracks.length === before) return null;
      // Keep an imported collection cover; only a cover borrowed from the
      // removed song moves on to the new first track.
      if (playlist.cover && playlist.cover === track.cover) playlist.cover = playlist.tracks[0]?.cover || "";
      playlist.updated_at = Date.now();
      this.save();
      return playlist;
    },
    /**
     * Merge the source playlist's current songs into playlist `id`
     * (services/playlist-sync.js). Returns { added, removed } or null.
     */
    applySourceSync(id, upstreamTracks, now = Date.now()) {
      const playlist = this.getById(id);
      if (!playlist) return null;
      const tracks = uniqueTracks(upstreamTracks);
      const upstream = tracks.map(trackIdentity);
      const plan = planPlaylistSync({ local: playlist.tracks.map(trackIdentity), base: playlist.source_keys, upstream });
      const sourcePlatform = sourcePlatformOf(playlist);
      if (cloud) {
        cloud.ops.syncPlaylist(cloud.doc, id, { tracks, plan, sourceKeys: upstream, sourcePlatform, now });
        this.refreshCloud();
      } else {
        playlist.tracks = syncedTracks(playlist.tracks, tracks, plan, trackIdentity);
        playlist.source_keys = upstream;
        playlist.source_platform = sourcePlatform;
        playlist.synced_at = now;
        if (plan.remove.length || plan.insert.length) {
          playlist.cover ||= playlist.tracks[0]?.cover || "";
          playlist.updated_at = now;
        }
        this.save();
      }
      return { added: plan.insert.length, removed: plan.remove.length };
    },
    /** Fetch the source playlist and merge it in. Throws on network errors. */
    async syncFromSource(id) {
      const playlist = this.getById(id);
      if (!canSyncPlaylist(playlist) || this.syncing.includes(id)) return null;
      this.syncing = [...this.syncing, id];
      try {
        const collection = await resolveCollection({ platform: sourcePlatformOf(playlist), url: playlist.source_url });
        if (!collection.items?.length) throw new Error("源歌单为空或暂时无法读取");
        return this.applySourceSync(id, collection.items.map((item) => trackFromSearch(item, "high")));
      } finally {
        this.syncing = this.syncing.filter((item) => item !== id);
      }
    },
    /** Daily background sync of due playlists, one at a time. */
    async syncDue({ limit = 10, now = Date.now() } = {}) {
      const due = this.items.filter((playlist) => isSyncDue(playlist, now)).slice(0, limit);
      const summary = { playlists: 0, added: 0, removed: 0, failed: 0 };
      for (const playlist of due) {
        try {
          const result = await this.syncFromSource(playlist.id);
          if (result && (result.added || result.removed)) {
            summary.playlists += 1;
            summary.added += result.added;
            summary.removed += result.removed;
          }
        } catch {
          summary.failed += 1;
        }
      }
      return summary;
    },
    remove(id) {
      if (cloud) {
        cloud.ops.deletePlaylist(cloud.doc, id);
        this.refreshCloud();
        return;
      }
      this.items = this.items.filter((item) => item.id !== id);
      this.save();
    },
  },
});
