<script setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { Volume, Volume1, Volume2, VolumeX } from "lucide-vue-next";
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
// Volume works like Apple Music's: clicking the speaker opens the slider and
// it stays open while adjusting, until a click elsewhere or Escape.
const volumeRoot = ref(null);
const volumeOpen = ref(false);
const volumeIcon = computed(() => status.volume <= 0 ? VolumeX : status.volume < 0.34 ? Volume : status.volume < 0.67 ? Volume1 : Volume2);
function closeVolumeOutside(event) {
  if (!volumeRoot.value?.contains(event.target)) volumeOpen.value = false;
}
function closeVolumeOnEscape(event) {
  if (event.key === "Escape") volumeOpen.value = false;
}
watch(volumeOpen, (open) => {
  const method = open ? "addEventListener" : "removeEventListener";
  document[method]("pointerdown", closeVolumeOutside, true);
  document[method]("keydown", closeVolumeOnEscape);
});
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", closeVolumeOutside, true);
  document.removeEventListener("keydown", closeVolumeOnEscape);
});
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
        <div ref="volumeRoot" class="volume-control" :class="{ 'is-open': volumeOpen }">
          <button class="volume-control__button" type="button" title="音量" aria-label="音量" :aria-expanded="volumeOpen" @click="volumeOpen = !volumeOpen">
            <component :is="volumeIcon" :size="20" />
          </button>
          <input
            type="range"
            min="0"
            max="1"
            step="0.01"
            aria-label="音量大小"
            :tabindex="volumeOpen ? 0 : -1"
            :value="status.volume"
            :style="{ '--volume': `${status.volume * 100}%` }"
            @input="player.setVolume(Number($event.target.value))"
          />
        </div>
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
