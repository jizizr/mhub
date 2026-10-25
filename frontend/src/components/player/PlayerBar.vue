<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { Volume2 } from "lucide-vue-next";
import * as player from "../../core/player";
import { useMediaStore } from "../../stores/media";
import { useStatusStore } from "../../stores/status";
import { formatTime } from "../../utils/format";
import { attachLiquidGlass } from "../../utils/liquid-glass";
import PlayerControls from "./PlayerControls.vue";
import TrackInfo from "./TrackInfo.vue";
import ListeningSettings from "./ListeningSettings.vue";
import SeekSlider from "./SeekSlider.vue";

const status = useStatusStore();
const media = useMediaStore();
const draft = ref(null);
const root = ref(null);
let detachGlass = () => {};
onMounted(() => { detachGlass = attachLiquidGlass(root.value); });
onBeforeUnmount(() => detachGlass());
const sliderValue = computed(() => draft.value ?? status.position);
const duration = computed(() => Math.max(status.duration || media.track?.duration || 0, 0));
const progress = computed(() => duration.value > 0
  ? Math.min(1, Math.max(0, sliderValue.value / duration.value))
  : 0);
</script>

<template>
  <footer ref="root" class="player-bar" :class="{ 'player-bar--empty': !media.track }" aria-label="当前播放">
    <div class="player-bar__grid">
      <PlayerControls />
      <!-- Apple Music-style "now playing" well: cover and title with a thin
           progress line; hovering swaps the title for the elapsed and
           remaining time. -->
      <div class="player-bar__now">
        <TrackInfo />
        <div class="player-bar__times" aria-hidden="true">
          <span>{{ formatTime(sliderValue) }}</span>
          <span>-{{ formatTime(Math.max(0, duration - sliderValue)) }}</span>
        </div>
        <div class="progress-rail" :style="{ '--progress': `${progress * 100}%` }">
          <SeekSlider
            class="progress-input"
            :position="status.position"
            :duration="duration"
            :track-key="media.track?.id || ''"
            :disabled="!media.track"
            @preview="draft = $event"
            @seek="player.seek"
          />
        </div>
      </div>
      <div class="player-toolbar">
        <label class="volume-control" title="音量">
          <Volume2 :size="20" />
          <input
            type="range"
            min="0"
            max="1"
            step="0.01"
            aria-label="音量"
            :value="status.volume"
            @input="player.setVolume(Number($event.target.value))"
          />
        </label>
        <ListeningSettings compact />
      </div>
    </div>
    <!-- The compact dock shows progress without a native range thumb covering
         the feed. Opening the player retains the full accessible seek slider. -->
    <div class="player-bar__compact-progress" aria-hidden="true">
      <span :style="{ transform: `scaleX(${progress})` }" />
    </div>
  </footer>
</template>
