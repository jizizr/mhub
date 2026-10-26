<script setup>
import { ListPlus, ListStart, MoreHorizontal, Play } from "lucide-vue-next";
import { computed } from "vue";
import { useMediaStore } from "../../stores/media";
import { trackIdentity } from "../../stores/queue";
import { useStatusStore } from "../../stores/status";
import { formatTime } from "../../utils/format";

const props = defineProps({
  items: { type: Array, default: () => [] },
  loading: { type: Boolean, default: false },
  emptyText: { type: String, default: "没有结果" },
});

const emit = defineEmits(["play", "queue", "queue-next", "menu"]);
const media = useMediaStore();
const status = useStatusStore();
const rows = computed(() => props.items);
// Apple Music only adds the album column when the list carries albums.
const hasAlbums = computed(() => rows.value.some((track) => track?.album));
const hasDurations = computed(() => rows.value.some((track) => track?.duration));

function isActiveTrack(track) {
  const key = trackIdentity(track);
  return key && [media.track, status.currentTrack].some((item) => trackIdentity(item) === key);
}

// As in Apple Music: a mouse click selects the row and a double click plays
// it; on touch screens a tap plays straight away. Neither opens the full player.
function handleRowClick(event, track) {
  const touch = event.pointerType === "touch" || event.pointerType === "pen"
    || (!event.pointerType && window.matchMedia?.("(hover: none)").matches);
  if (touch) emit("play", track);
}

function handleRowKey(event, track) {
  if (event.target !== event.currentTarget) return;
  if (event.key === "Enter") {
    event.preventDefault();
    emit("play", track);
  } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    const next = event.key === "ArrowDown" ? event.currentTarget.nextElementSibling : event.currentTarget.previousElementSibling;
    if (next?.classList.contains("song-row")) {
      event.preventDefault();
      next.focus();
    }
  }
}
</script>

<template>
  <div class="song-list" :class="{ 'song-list--albums': hasAlbums }">

    <Transition name="list-state" mode="out-in">
      <div v-if="loading" key="loading" class="list-state">正在搜索...</div>
      <div v-else-if="!rows.length" key="empty" class="list-state">{{ emptyText }}</div>

      <div v-else key="rows" class="song-list__table">
        <div class="song-list__head" aria-hidden="true">
          <span class="song-list__head-song">歌曲</span>
          <span class="song-list__head-artist">艺人</span>
          <span v-if="hasAlbums" class="song-list__head-album">专辑</span>
          <span class="song-list__head-time">{{ hasDurations ? "时长" : "" }}</span>
        </div>
        <TransitionGroup name="song" tag="div" class="song-list__rows">
          <div
            v-for="(track, index) in rows"
            :key="`${trackIdentity(track)}:${index}`"
            class="song-row"
            :class="{ 'is-playing': isActiveTrack(track) }"
            tabindex="0"
            :aria-label="`${track.title} - ${track.artist || '未知艺人'}`"
            :aria-current="isActiveTrack(track) ? 'true' : undefined"
            @click="handleRowClick($event, track)"
            @dblclick="emit('play', track)"
            @keydown="handleRowKey($event, track)"
            @contextmenu.prevent="emit('menu', { event: $event, track })"
          >
            <button class="song-row__art" type="button" title="播放" :aria-label="`播放 ${track.title}`" @click.stop="emit('play', track)" @dblclick.stop>
              <span class="cover-thumb">
                <img v-if="track.cover" :src="track.cover" alt="" loading="lazy" />
              </span>
              <span v-if="isActiveTrack(track)" class="now-playing-bars" :class="{ 'is-paused': !status.isPlaying }" aria-hidden="true"><i></i><i></i><i></i><i></i></span>
              <span class="song-row__art-play" aria-hidden="true"><Play :size="16" fill="currentColor" /></span>
            </button>
            <span class="song-row__text">
              <span class="song-row__title">{{ track.title }}</span>
              <span class="song-row__artist">{{ track.artist || "未知艺人" }}</span>
            </span>
            <span v-if="hasAlbums" class="song-row__album">{{ track.album }}</span>
            <span class="song-row__meta">{{ track.duration ? formatTime(track.duration) : "" }}</span>
            <span class="song-row__actions">
              <button class="mini-button hide-mobile song-row__hover" title="下一首播放" @click.stop="emit('queue-next', track)" @dblclick.stop>
                <ListStart :size="16" />
              </button>
              <button class="mini-button hide-mobile song-row__hover" title="加入播放队列" @click.stop="emit('queue', track)" @dblclick.stop>
                <ListPlus :size="16" />
              </button>
              <button class="mini-button" title="更多操作" aria-label="更多操作" aria-haspopup="menu" @click.stop="emit('menu', { event: $event, track })" @dblclick.stop>
                <MoreHorizontal :size="16" />
              </button>
            </span>
          </div>
        </TransitionGroup>
      </div>
    </Transition>
  </div>
</template>
