import { defineStore } from "pinia";
import { markRaw, toRaw } from "vue";

function plainLyricLines(lines) {
  if (!Array.isArray(lines)) return [];
  return lines.map((line, index) => {
    const rawLine = toRaw(line);
    const words = Array.isArray(rawLine.words) ? rawLine.words : [];
    const startTime = Number.isFinite(rawLine.startTime) ? rawLine.startTime : index * 4000;
    const endTime = Number.isFinite(rawLine.endTime) ? rawLine.endTime : startTime + 3800;
    return {
      words: words.map((word) => ({ ...toRaw(word) })),
      translatedLyric: rawLine.translatedLyric || "",
      romanLyric: rawLine.romanLyric || "",
      isBG: Boolean(rawLine.isBG),
      isDuet: Boolean(rawLine.isDuet),
      startTime,
      endTime,
    };
  });
}

export const useMediaStore = defineStore("media", {
  state: () => ({
    track: null,
    lyricLines: [],
    lyricLoading: false,
    coverBroken: false,
    expanded: false,
  }),
  actions: {
    setTrack(track) {
      this.track = track;
      this.coverBroken = false;
    },
    beginLyricLoad() {
      this.lyricLines = [];
      this.lyricLoading = true;
    },
    setLyric(lines) {
      this.lyricLines = markRaw(plainLyricLines(lines));
      this.lyricLoading = false;
    },
    clearLyric() {
      this.lyricLines = [];
      this.lyricLoading = false;
    },
  },
});
