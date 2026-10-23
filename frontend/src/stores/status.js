import { defineStore } from "pinia";
import { useQueueStore } from "./queue";

const STORAGE_KEY = "music-web-splayer-status-v3";
// Player actions persist several times per gesture; write once per task.
let persistScheduled = false;
let toastTimer = 0;

function restore() {
  try {
    const data = JSON.parse(localStorage.getItem(STORAGE_KEY) || "{}");
    delete data.position;
    // Listening preferences survive reloads; analysis and transition progress
    // always belong to the current audio session.
    delete data.soundCheckState;
    delete data.transitionState;
    data.crossfadeEnabled = data.crossfadeEnabled === true;
    if (!["system", "light", "dark"].includes(data.theme)) data.theme = "system";
    data.soundCheckEnabled = data.soundCheckEnabled === true;
    const crossfadeSeconds = Number(data.crossfadeSeconds ?? 4);
    data.crossfadeSeconds = Number.isFinite(crossfadeSeconds)
      ? Math.min(12, Math.max(1, Math.round(crossfadeSeconds)))
      : 4;
    return data;
  } catch {
    return {};
  }
}

export const useStatusStore = defineStore("status", {
  state: () => ({
    state: "idle",
    position: 0,
    duration: 0,
    volume: 0.86,
    currentSource: null,
    currentSourceTrackId: null,
    trackLoading: false,
    playIndex: -1,
    repeatMode: "list",
    shuffleMode: "off",
    autoplayEnabled: false,
    autoplayState: "idle",
    autoplayError: "",
    crossfadeEnabled: false,
    crossfadeSeconds: 4,
    soundCheckEnabled: false,
    soundCheckState: "idle",
    transitionState: "idle",
    speed: 1.0,
    searchPlatform: "applemusic",
    quality: "high",
    lyricScript: "original",
    theme: "system",
    activeView: "discover",
    activePlaylistId: null,
    searchOpen: false,
    queueOpen: false,
    notice: "搜索或选择一首音乐",
    // Transient feedback shown on every screen size (the sidebar notice is
    // hidden on phones). `toastId` restarts the animation for repeats.
    toastMessage: "",
    toastId: 0,
    ...restore(),
  }),
  getters: {
    // Keep the transport controls in the pause state while an uncached track
    // is waiting for its first playable bytes.  Treating that phase as
    // "playing" in the UI used to make the progress interpolator advance
    // before the media element had actually started, which looked like a
    // silent playback or a frozen progress bar depending on the browser.
    isPlaying: (state) => state.state === "playing" || state.state === "buffering",
    isBuffering: (state) => state.state === "buffering",
    isPaused: (state) => state.state === "paused",
    progress: (state) => (state.duration > 0 ? state.position / state.duration : 0),
    currentTrack: (state) => useQueueStore().getTrack(state.playIndex),
  },
  actions: {
    persist() {
      if (persistScheduled) return;
      persistScheduled = true;
      queueMicrotask(() => {
        persistScheduled = false;
        this.writePersisted();
      });
    },
    writePersisted() {
      localStorage.setItem(
        STORAGE_KEY,
        JSON.stringify({
          volume: this.volume,
          playIndex: this.playIndex,
          repeatMode: this.repeatMode,
          shuffleMode: this.shuffleMode,
          autoplayEnabled: this.autoplayEnabled,
          crossfadeEnabled: this.crossfadeEnabled,
          crossfadeSeconds: this.crossfadeSeconds,
          soundCheckEnabled: this.soundCheckEnabled,
          speed: this.speed,
          searchPlatform: this.searchPlatform,
          quality: this.quality,
          lyricScript: this.lyricScript,
          theme: this.theme,
          activeView: this.activeView,
          activePlaylistId: this.activePlaylistId,
        }),
      );
    },
    setNotice(notice) {
      this.notice = notice;
    },
    toast(message) {
      this.notice = message;
      this.toastMessage = message;
      const id = ++this.toastId;
      globalThis.clearTimeout?.(toastTimer);
      toastTimer = globalThis.setTimeout?.(() => {
        if (this.toastId === id) this.toastMessage = "";
      }, 2600);
    },
    dismissToast() {
      globalThis.clearTimeout?.(toastTimer);
      this.toastMessage = "";
    },
  },
});
