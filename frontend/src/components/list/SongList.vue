<script setup>
import { ListPlus, Play } from "lucide-vue-next";
import { computed } from "vue";
import { useMediaStore } from "../../stores/media";
import { trackIdentity } from "../../stores/queue";
import { useStatusStore } from "../../stores/status";
import { formatSize, formatTime } from "../../utils/format";

const props = defineProps({
  items: { type: Array, default: () => [] },
  loading: { type: Boolean, default: false },
  emptyText: { type: String, default: "没有结果" },
});

const emit = defineEmits(["play", "queue", "open"]);
const media = useMediaStore();
const status = useStatusStore();
const rows = computed(() => props.items);

function isActiveTrack(track) {
  const key = trackIdentity(track);
  return key && [media.track, status.currentTrack].some((item) => trackIdentity(item) === key);
}

// Playlist entries keep the quality that was selected when they were
// imported.  That value is only a historical hint; playback always resolves
// the source with the current player preference.  Keep the list metadata in
// sync with that same preference so a playlist does not misleadingly show an
// old quality after the user switches to Hi-Res or lossless.
function trackInfo(track) {
  // A playlist may have been imported with a different quality. The row is a
  // playback surface, so always show the quality that will be used now.
  const parts = [status.quality || track.quality || "high"];
  if (track.duration) parts.push(formatTime(track.duration));
  if (track.byte_size) parts.push(formatSize(track.byte_size));
  return parts.join(" · ");
}
</script>

<template>
  <div class="song-list">
    <div class="song-list__head">
      <span>#</span>
      <span>标题</span>
      <span class="hide-mobile">平台</span>
      <span class="hide-mobile">信息</span>
      <span></span>
    </div>

    <Transition name="list-state" mode="out-in">
      <div v-if="loading" key="loading" class="list-state">正在搜索...</div>
      <div v-else-if="!rows.length" key="empty" class="list-state">{{ emptyText }}</div>

      <TransitionGroup v-else name="song" tag="div" class="song-list__rows">
        <div
          v-for="(track, index) in rows"
          :key="`${trackIdentity(track)}:${index}`"
          class="song-row"
          :class="{ 'is-playing': isActiveTrack(track) }"
          @dblclick="emit('play', track)"
        >
          <span class="song-row__index">{{ index + 1 }}</span>
          <button class="song-row__main song-row__open" title="播放并打开全屏" @click.stop="emit('open', track)">
            <span class="cover-thumb">
              <img v-if="track.cover" :src="track.cover" alt="" loading="lazy" />
            </span>
            <span class="song-row__text">
              <span class="song-row__title">{{ track.title }}</span>
              <span class="song-row__artist">{{ track.artist || "未知艺人" }}</span>
            </span>
          </button>
          <span class="song-row__meta hide-mobile">{{ track.platform }}</span>
          <span class="song-row__meta hide-mobile">
            {{ trackInfo(track) }}
          </span>
          <span class="song-row__actions">
            <button class="mini-button" title="播放" @click.stop="emit('play', track)">
              <Play :size="16" />
            </button>
            <button class="mini-button" title="加入播放队列" @click.stop="emit('queue', track)">
              <ListPlus :size="16" />
            </button>
          </span>
        </div>
      </TransitionGroup>
    </Transition>
  </div>
</template>
