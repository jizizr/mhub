<script setup>
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { Check, Infinity as InfinityIcon, ListMusic, ListOrdered, LoaderCircle, Mic2, Pause, Play, Repeat, Repeat1, Shuffle, SkipBack, SkipForward } from "lucide-vue-next";
import * as player from "../../core/player";
import { useMediaStore } from "../../stores/media";
import { useStatusStore } from "../../stores/status";
import IconButton from "../ui/IconButton.vue";

const status = useStatusStore();
const media = useMediaStore();
const modeOpen = ref(false);
const modeControl = ref(null);

const modes = [
  { key: "sequence", label: "顺序播放", icon: ListOrdered },
  { key: "list", label: "列表循环", icon: Repeat },
  { key: "one", label: "单曲循环", icon: Repeat1 },
  { key: "shuffle", label: "随机播放", icon: Shuffle },
];

const currentMode = computed(() => modes.find((mode) => mode.key === player.playbackMode()) || modes[0]);

function selectMode(mode) {
  player.setPlaybackMode(mode.key);
  modeOpen.value = false;
}

// Behave like every other popover: an outside press or Escape dismisses it.
function closeOnOutside(event) {
  if (!modeControl.value?.contains(event.target)) modeOpen.value = false;
}
function closeOnEscape(event) {
  if (event.key === "Escape") modeOpen.value = false;
}
watch(modeOpen, (open) => {
  const method = open ? "addEventListener" : "removeEventListener";
  document[method]("pointerdown", closeOnOutside, true);
  document[method]("keydown", closeOnEscape);
});
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", closeOnOutside, true);
  document.removeEventListener("keydown", closeOnEscape);
});
</script>

<template>
  <div class="player-controls">
    <div ref="modeControl" class="play-mode-control player-control--secondary">
      <IconButton
        :title="`播放顺序：${currentMode.label}`"
        :active="currentMode.key !== 'sequence'"
        @click="modeOpen = !modeOpen"
      >
        <component :is="currentMode.icon" :size="18" />
      </IconButton>
      <div v-if="modeOpen" class="play-mode-menu" role="menu">
        <button
          v-for="mode in modes"
          :key="mode.key"
          class="play-mode-option"
          :class="{ 'is-active': mode.key === currentMode.key }"
          @click="selectMode(mode)"
        >
          <component :is="mode.icon" :size="16" />
          <span>{{ mode.label }}</span>
          <Check v-if="mode.key === currentMode.key" :size="15" />
        </button>
      </div>
    </div>
    <IconButton
      class="player-control--secondary"
      :title="status.autoplayEnabled ? '关闭自动续播' : '开启自动续播：队列结束后推荐新歌曲'"
      :active="status.autoplayEnabled"
      :aria-pressed="status.autoplayEnabled"
      @click="player.toggleAutoplay()"
    >
      <InfinityIcon :size="20" />
    </IconButton>
    <IconButton class="player-control--secondary" title="上一首" @click="player.prevTrack()">
      <SkipBack :size="20" />
    </IconButton>
    <IconButton size="lg" :title="status.isBuffering ? '正在缓冲，点击暂停' : status.isPlaying ? '暂停' : '播放'" :active="status.isBuffering" @click="player.togglePlay()">
      <LoaderCircle v-if="status.isBuffering" class="is-spinning" :size="24" />
      <Pause v-else-if="status.isPlaying" :size="24" />
      <Play v-else :size="24" fill="currentColor" />
    </IconButton>
    <IconButton title="下一首" @click="player.nextTrack(true)">
      <SkipForward :size="20" />
    </IconButton>
    <IconButton class="player-control--secondary" title="歌词" :active="media.expanded" :disabled="!media.track" @click="media.expanded = true">
      <Mic2 :size="18" />
    </IconButton>
    <IconButton class="player-control--queue" title="队列" :active="status.queueOpen" @click="status.queueOpen = !status.queueOpen">
      <ListMusic :size="18" />
    </IconButton>
  </div>
</template>
