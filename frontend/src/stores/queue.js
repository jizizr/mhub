import { defineStore } from "pinia";

const STORAGE_KEY = "music-web-splayer-queue-v3";
let saveScheduled = false;

function restore() {
  try {
    return JSON.parse(localStorage.getItem(STORAGE_KEY) || "{}");
  } catch {
    return {};
  }
}

function shuffleArray(items) {
  const next = [...items];
  for (let i = next.length - 1; i > 0; i -= 1) {
    const j = Math.floor(Math.random() * (i + 1));
    [next[i], next[j]] = [next[j], next[i]];
  }
  return next;
}

export function trackIdentity(track) {
  if (!track) return "";
  if (typeof track === "string") {
    const parts = track.split("--");
    return parts.length > 2 ? parts.slice(0, -1).join("--") : track;
  }
  const platform = String(track.platform || "").trim().toLowerCase();
  const source =
    track.source_id ||
    track.selected_id ||
    track.track_id ||
    (typeof track.id === "string" && track.id.includes("--")
      ? track.id.split("--").slice(0, -1).join("--")
      : track.id) ||
    track.query ||
    track.title;
  return [platform, source].filter(Boolean).join(":");
}

function uniqueTracks(items) {
  const seen = new Set();
  const tracks = [];
  for (const item of items || []) {
    const key = trackIdentity(item);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    tracks.push(item);
  }
  return tracks;
}

export const useQueueStore = defineStore("queue", {
  state: () => {
    const data = restore();
    return {
      queue: Array.isArray(data.queue) ? uniqueTracks(data.queue) : [],
      originalQueue: Array.isArray(data.originalQueue) ? uniqueTracks(data.originalQueue) : null,
      sourcePlaylist: data.sourcePlaylist || null,
    };
  },
  getters: {
    queueLength: (state) => state.queue.length,
  },
  actions: {
    // Bulk edits call save() once per item. Serialize the whole queue once
    // per task instead; the microtask still runs before any unload handler.
    save() {
      if (saveScheduled) return;
      saveScheduled = true;
      queueMicrotask(() => {
        saveScheduled = false;
        localStorage.setItem(
          STORAGE_KEY,
          JSON.stringify({ queue: this.queue, originalQueue: this.originalQueue, sourcePlaylist: this.sourcePlaylist }),
        );
      });
    },
    setQueue(items, sourcePlaylist = null) {
      this.queue = uniqueTracks(items);
      this.originalQueue = null;
      this.sourcePlaylist = sourcePlaylist
        ? { id: sourcePlaylist.id, name: sourcePlaylist.name, type: sourcePlaylist.type || "playlist",
            ...(sourcePlaylist.resource_type ? { resource_type: sourcePlaylist.resource_type } : {}) }
        : null;
      this.save();
    },
    replaceTrack(index, item) {
      if (index < 0 || index >= this.queue.length) return;
      const oldKey = trackIdentity(this.queue[index]);
      this.queue[index] = item;
      if (this.originalQueue) {
        const originalIndex = this.originalQueue.findIndex((track) => trackIdentity(track) === oldKey);
        if (originalIndex !== -1) this.originalQueue[originalIndex] = item;
      }
      this.save();
    },
    insertToQueue(item, index) {
      const safeIndex = Math.max(0, Math.min(index, this.queue.length));
      this.queue.splice(safeIndex, 0, item);
      if (this.originalQueue) this.originalQueue.push(item);
      this.save();
    },
    insertManyToQueue(items, index) {
      const nextItems = uniqueTracks(items);
      if (!nextItems.length) return;
      const safeIndex = Math.max(0, Math.min(index, this.queue.length));
      this.queue.splice(safeIndex, 0, ...nextItems);
      this.queue = uniqueTracks(this.queue);
      if (this.originalQueue) this.originalQueue = uniqueTracks([...this.originalQueue, ...nextItems]);
      this.save();
    },
    queueNextMany(items, currentTrack) {
      const currentKey = trackIdentity(currentTrack);
      const nextItems = uniqueTracks(items).filter((item) => trackIdentity(item) !== currentKey);
      if (!nextItems.length) return { count: 0, currentIndex: this.findTrackIndex(currentTrack) };

      const nextKeys = new Set(nextItems.map(trackIdentity));
      const arrange = (tracks) => {
        // Move overlaps into the requested segment rather than letting an
        // earlier queue occurrence win deduplication and silently disappear
        // from "play next". The active item itself is never replaced.
        const remaining = tracks.filter((item) => !nextKeys.has(trackIdentity(item)));
        const currentIndex = remaining.findIndex((item) => trackIdentity(item) === currentKey);
        remaining.splice(currentIndex + 1, 0, ...nextItems);
        return remaining;
      };
      this.queue = arrange(this.queue);
      // An explicit next-up segment survives turning shuffle off. The rest
      // of the original order is unchanged, including the active item.
      if (this.originalQueue) this.originalQueue = arrange(this.originalQueue);
      this.save();
      return { count: nextItems.length, currentIndex: this.findTrackIndex(currentTrack) };
    },
    removeFromQueue(index) {
      if (index < 0 || index >= this.queue.length) return null;
      const [removed] = this.queue.splice(index, 1);
      if (this.originalQueue) {
        const removedKey = trackIdentity(removed);
        const originalIndex = this.originalQueue.findIndex((track) => trackIdentity(track) === removedKey);
        if (originalIndex !== -1) this.originalQueue.splice(originalIndex, 1);
      }
      this.save();
      return removed;
    },
    moveInQueue(fromIndex, toIndex) {
      if (fromIndex === toIndex) return;
      if (fromIndex < 0 || fromIndex >= this.queue.length) return;
      if (toIndex < 0 || toIndex >= this.queue.length) return;
      const [item] = this.queue.splice(fromIndex, 1);
      this.queue.splice(toIndex, 0, item);
      this.save();
    },
    clearQueue() {
      this.queue = [];
      this.originalQueue = null;
      this.sourcePlaylist = null;
      this.save();
    },
    removeUpcomingRecommendations(currentIndex) {
      const removeKeys = new Set(this.queue
        .filter((item, index) => index > currentIndex && item.autoplayRecommended)
        .map(trackIdentity));
      if (!removeKeys.size) return;
      this.queue = this.queue.filter((item) => !removeKeys.has(trackIdentity(item)));
      if (this.originalQueue) this.originalQueue = this.originalQueue.filter((item) => !removeKeys.has(trackIdentity(item)));
      this.save();
    },
    shuffleQueue(keepIndex, { recommendationsLast = false } = {}) {
      if (this.queue.length <= 1) return;
      const safeIndex = keepIndex >= 0 && keepIndex < this.queue.length ? keepIndex : 0;
      if (!this.originalQueue) this.originalQueue = [...this.queue];
      const keepTrack = this.queue[safeIndex];
      const rest = this.queue.filter((_, index) => index !== safeIndex);
      this.queue = [keepTrack, ...(recommendationsLast
        ? [...shuffleArray(rest.filter((item) => !item.autoplayRecommended)), ...shuffleArray(rest.filter((item) => item.autoplayRecommended))]
        : shuffleArray(rest))];
      this.save();
    },
    unshuffleQueue(currentTrack, { recommendationsLast = false } = {}) {
      if (!this.originalQueue) return 0;
      this.queue = [...this.originalQueue];
      this.originalQueue = null;
      const currentKey = trackIdentity(currentTrack);
      const index = this.queue.findIndex((track) => trackIdentity(track) === currentKey);
      if (recommendationsLast && index >= 0) {
        // Additions made during shuffle are also recorded in originalQueue.
        // Restoring that order must not put newly chosen songs after the
        // generated tail. Keep history/current intact and stably partition
        // only the upcoming segment.
        const upcoming = this.queue.splice(index + 1);
        this.queue.push(...upcoming.filter((track) => !track.autoplayRecommended),
          ...upcoming.filter((track) => track.autoplayRecommended));
      }
      this.save();
      return index !== -1 ? index : 0;
    },
    getTrack(index) {
      return index >= 0 && index < this.queue.length ? this.queue[index] : null;
    },
    findTrackIndex(track) {
      const key = trackIdentity(track);
      return this.queue.findIndex((item) => trackIdentity(item) === key);
    },
  },
});
