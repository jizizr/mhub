<script setup>
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { Check, Infinity as InfinityIcon, ListMusic, ListOrdered, LoaderCircle, Mic2, Pause, Play, Repeat, Repeat1, Shuffle } from "lucide-vue-next";
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

// The desktop capsule shows Apple Music's separate shuffle and repeat
// buttons; both map onto the same playback modes as the mode menu.
const shuffleOn = computed(() => currentMode.value.key === "shuffle");
const repeatMode = computed(() => ["list", "one"].includes(currentMode.value.key) ? currentMode.value.key : "off");
function toggleShuffle() {
  player.setPlaybackMode(shuffleOn.value ? "sequence" : "shuffle");
}
function cycleRepeat() {
  player.setPlaybackMode(repeatMode.value === "off" ? "list" : repeatMode.value === "list" ? "one" : "sequence");
}

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
      class="player-control--secondary player-control--autoplay"
      :title="status.autoplayEnabled ? '关闭自动续播' : '开启自动续播：队列结束后推荐新歌曲'"
      :active="status.autoplayEnabled"
      :aria-pressed="status.autoplayEnabled"
      @click="player.toggleAutoplay()"
    >
      <InfinityIcon :size="20" />
    </IconButton>
    <IconButton class="player-control--shuffle" :title="shuffleOn ? '随机播放：开' : '随机播放：关'" :active="shuffleOn" :aria-pressed="shuffleOn" @click="toggleShuffle">
      <Shuffle :size="16" />
    </IconButton>
    <IconButton class="player-control--secondary player-control--prev" title="上一首" @click="player.prevTrack()">
      <!-- Solid double triangles, like SF Symbols backward.fill. -->
      <svg class="transport-glyph" width="22" height="22" viewBox="0 0 24 24" aria-hidden="true"><path d="M11.5 5v14L1.8 12zM22.2 5v14L12.5 12z" fill="currentColor" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" /></svg>
    </IconButton>
    <IconButton class="player-control--play" size="lg" :title="status.isBuffering ? '正在缓冲，点击暂停' : status.isPlaying ? '暂停' : '播放'" :active="status.isBuffering" @click="player.togglePlay()">
      <LoaderCircle v-if="status.isBuffering" class="is-spinning" :size="24" />
      <Pause v-else-if="status.isPlaying" :size="24" fill="currentColor" />
      <Play v-else :size="24" fill="currentColor" />
    </IconButton>
    <IconButton class="player-control--next" title="下一首" @click="player.nextTrack(true)">
      <svg class="transport-glyph" width="22" height="22" viewBox="0 0 24 24" aria-hidden="true"><path d="M1.8 5v14l9.7-7zM12.5 5v14l9.7-7z" fill="currentColor" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" /></svg>
    </IconButton>
    <IconButton class="player-control--repeat" :title="repeatMode === 'one' ? '重复：单曲' : repeatMode === 'list' ? '重复：全部' : '重复：关'" :active="repeatMode !== 'off'" :aria-pressed="repeatMode !== 'off'" @click="cycleRepeat">
      <Repeat1 v-if="repeatMode === 'one'" :size="16" />
      <Repeat v-else :size="16" />
    </IconButton>
    <IconButton class="player-control--secondary player-control--lyrics" title="歌词" :active="media.expanded" :disabled="!media.track" @click="media.expanded = true">
      <Mic2 :size="18" />
    </IconButton>
    <IconButton class="player-control--queue" title="队列" :active="status.queueOpen" @click="status.queueOpen = !status.queueOpen">
      <ListMusic :size="18" />
    </IconButton>
  </div>
</template>
