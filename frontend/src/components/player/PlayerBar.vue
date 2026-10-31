<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import * as player from "../../core/player";
import { useMediaStore } from "../../stores/media";
import { useStatusStore } from "../../stores/status";
import { formatTime } from "../../utils/format";
import { attachLiquidGlass } from "../../utils/liquid-glass";
import PlayerControls from "./PlayerControls.vue";
import TrackInfo from "./TrackInfo.vue";
import ListeningSettings from "./ListeningSettings.vue";
import SeekSlider from "./SeekSlider.vue";
import VolumeControl from "./VolumeControl.vue";

const status = useStatusStore();
const media = useMediaStore();
const draft = ref(null);
const root = ref(null);
let detachGlass = () => {};
onMounted(() => { detachGlass = attachLiquidGlass(root.value); });
onBeforeUnmount(() => detachGlass());
// Like Apple's progress-container: expand once the pointer moves onto the
// strip (not merely over the LCD), collapse when it leaves.
const progressExpanded = ref(false);
function expandProgress(event) {
  if (!progressExpanded.value && media.track && event.offsetY >= -4) progressExpanded.value = true;
}
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
      <!-- Apple Music-style "now playing" well: cover and title over a thin
           progress line. -->
      <div class="player-bar__now" :class="{ 'is-scrubbing': progressExpanded }">
        <!-- Idle LCD: the brand mark alone, as Apple Music shows its logo. -->
        <svg v-if="!media.track" class="player-bar__idle" width="22" height="22" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <rect x="2" y="7.4" width="3" height="9.2" rx="1.5" />
          <rect x="6.25" y="4.8" width="3" height="14.4" rx="1.5" />
          <rect x="10.5" y="8.4" width="3" height="7.2" rx="1.5" />
          <rect x="14.75" y="4.8" width="3" height="14.4" rx="1.5" />
          <rect x="19" y="7.4" width="3" height="9.2" rx="1.5" />
        </svg>
        <TrackInfo />
        <!-- Apple Music's scrubber: a 2px line that, while the pointer is on
             it (or the slider has focus), grows to 7px with the times above
             and a 4px blur over the LCD. -->
        <div class="player-bar__progress" :class="{ 'is-expanded': progressExpanded }" @mousemove="expandProgress" @mouseleave="progressExpanded = false">
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
      </div>
      <div class="player-toolbar">
        <VolumeControl />
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
