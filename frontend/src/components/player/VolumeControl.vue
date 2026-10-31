<script setup>
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { Volume, Volume1, Volume2, VolumeX } from "lucide-vue-next";
import * as player from "../../core/player";
import { useStatusStore } from "../../stores/status";

const status = useStatusStore();
// Volume works like Apple Music's: clicking the speaker opens the slider and
// it stays open while adjusting, until a click elsewhere or Escape.
const root = ref(null);
const open = ref(false);
const icon = computed(() => status.volume <= 0 ? VolumeX : status.volume < 0.34 ? Volume : status.volume < 0.67 ? Volume1 : Volume2);
function closeOutside(event) {
  if (!root.value?.contains(event.target)) open.value = false;
}
function closeOnEscape(event) {
  if (event.key === "Escape") open.value = false;
}
watch(open, (value) => {
  const method = value ? "addEventListener" : "removeEventListener";
  document[method]("pointerdown", closeOutside, true);
  document[method]("keydown", closeOnEscape);
});
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", closeOutside, true);
  document.removeEventListener("keydown", closeOnEscape);
});
</script>

<template>
  <div ref="root" class="volume-control" :class="{ 'is-open': open }">
    <button class="volume-control__button" type="button" title="音量" aria-label="音量" :aria-expanded="open" @click="open = !open">
      <component :is="icon" :size="20" />
    </button>
    <input
      type="range"
      min="0"
      max="1"
      step="0.01"
      aria-label="音量大小"
      :tabindex="open ? 0 : -1"
      :value="status.volume"
      :style="{ '--volume': `${status.volume * 100}%` }"
      @input="player.setVolume(Number($event.target.value))"
    />
  </div>
</template>
