<script setup>
import { computed } from "vue";
import { Maximize2 } from "lucide-vue-next";
import { coverUrl } from "../../apis/music";
import { useMediaStore } from "../../stores/media";

const media = useMediaStore();

// Apple Music's LCD reads "artist — album" on its second line.
const byline = computed(() => {
  const track = media.track;
  if (!track) return "选择一首音乐开始";
  return [track.artist, track.album].filter(Boolean).join(" — ") || "未知艺人";
});

const cover = computed(() => {
  if (!media.track || media.coverBroken) return "";
  return media.track.cover || (media.track.cover_url ? coverUrl(media.track) : "");
});
</script>

<template>
  <button class="track-info" :disabled="!media.track" :title="media.track ? '展开播放器与歌词' : '选择一首音乐开始'" @click="media.expanded = Boolean(media.track)">
    <span class="track-info__cover">
      <img v-if="cover" :src="cover" alt="" @error="media.coverBroken = true" />
      <span v-if="media.track" class="track-info__expand" aria-hidden="true"><Maximize2 :size="14" :stroke-width="2.4" /></span>
    </span>
    <span class="track-info__text">
      <span class="track-info__title">{{ media.track?.title || "未播放" }}</span>
      <span class="track-info__artist">{{ byline }}</span>
    </span>
  </button>
</template>
