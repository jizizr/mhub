<script setup>
import { computed } from "vue";
import { coverUrl } from "../../apis/music";
import { useMediaStore } from "../../stores/media";

const media = useMediaStore();

const cover = computed(() => {
  if (!media.track || media.coverBroken) return "";
  return media.track.cover || (media.track.cover_url ? coverUrl(media.track) : "");
});
</script>

<template>
  <button class="track-info" :disabled="!media.track" @click="media.expanded = Boolean(media.track)">
    <span class="track-info__cover">
      <img v-if="cover" :src="cover" alt="" @error="media.coverBroken = true" />
    </span>
    <span class="track-info__text">
      <span class="track-info__title">{{ media.track?.title || "未播放" }}</span>
      <span class="track-info__artist">{{ media.track?.artist || "选择一首音乐开始" }}</span>
    </span>
  </button>
</template>
