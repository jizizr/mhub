import { defineStore } from "pinia";
import { trackIdentity } from "./queue";

const STORAGE_KEY = "music-web-splayer-playlists-v1";

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
    cover: item.cover || item.tracks?.[0]?.cover || "",
    tracks: uniqueTracks(item.tracks),
    created_at: item.created_at || Date.now(),
    updated_at: item.updated_at || Date.now(),
  };
}

export const usePlaylistStore = defineStore("playlists", {
  state: () => ({
    items: restore().map(normalizePlaylist),
  }),
  getters: {
    count: (state) => state.items.length,
    getById: (state) => (id) => state.items.find((item) => item.id === id) || null,
  },
  actions: {
    save() {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(this.items));
    },
    create({ name, tracks, platform, sourceId, sourceUrl }) {
      const now = Date.now();
      const playlist = normalizePlaylist({
        id: makeId(),
        name,
        tracks,
        platform,
        source_id: sourceId,
        source_url: sourceUrl,
        created_at: now,
        updated_at: now,
      });
      this.items.unshift(playlist);
      this.save();
      return playlist;
    },
    merge(id, tracks) {
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
      playlist.name = nextName;
      playlist.updated_at = Date.now();
      this.save();
      return playlist;
    },
    remove(id) {
      this.items = this.items.filter((item) => item.id !== id);
      this.save();
    },
  },
});
