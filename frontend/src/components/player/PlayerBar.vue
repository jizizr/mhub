<script setup>
import { computed, ref } from "vue";
import { Volume2 } from "lucide-vue-next";
import * as player from "../../core/player";
import { useMediaStore } from "../../stores/media";
import { useStatusStore } from "../../stores/status";
import { formatTime } from "../../utils/format";
import PlayerControls from "./PlayerControls.vue";
import TrackInfo from "./TrackInfo.vue";
import ListeningSettings from "./ListeningSettings.vue";
import SeekSlider from "./SeekSlider.vue";

const status = useStatusStore();
const media = useMediaStore();
const draft = ref(null);
const sliderValue = computed(() => draft.value ?? status.position);
const duration = computed(() => Math.max(status.duration || media.track?.duration || 0, 0));
</script>

<template>
  <footer class="player-bar">
    <div class="progress-rail">
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
    <div class="player-bar__grid">
      <TrackInfo />
      <PlayerControls />
      <div class="player-toolbar">
        <span class="time-readout">{{ formatTime(sliderValue) }} / {{ formatTime(duration) }}</span>
        <label class="volume-control" title="音量">
          <Volume2 :size="17" />
          <input
            type="range"
            min="0"
            max="1"
            step="0.01"
            :value="status.volume"
            @input="player.setVolume(Number($event.target.value))"
          />
        </label>
        <ListeningSettings compact />
      </div>
    </div>
  </footer>
</template>

<style scoped>
.progress-rail { top: -12px; height: 24px; z-index: 1; }
.player-toolbar { gap: 12px; }
@media (min-width: 881px) and (max-width: 1180px) {
  .player-toolbar .time-readout { display: none; }
  .volume-control input { width: 70px; }
}
</style>
