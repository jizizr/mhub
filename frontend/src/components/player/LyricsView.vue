<script setup>
import { defineAsyncComponent, onErrorCaptured, ref } from "vue";
import LyricsFallback from "./LyricsFallback.vue";

defineOptions({ inheritAttrs: false });
const props = defineProps({
  lyricLines: { type: Array, default: () => [] },
  script: { type: String, default: "original" },
  retryToken: { type: Number, default: 0 },
});
const emit = defineEmits(["seek", "conversion-state"]);
const ready = ref(false);
const failed = ref(false);
const AnimatedLyrics = defineAsyncComponent({
  loader: () => import("./SPlayerLyrics.vue"),
  timeout: 15000,
});
// A failed optional renderer must not remove the already available lyric text.
onErrorCaptured((error) => {
  failed.value = true;
  ready.value = false;
  console.warn("[lyrics] animated view unavailable; retaining readable lyrics", error);
  return false;
});
</script>

<template>
  <div class="lyrics-view">
    <LyricsFallback v-if="!ready" v-bind="props" @seek="emit('seek', $event)"
      @conversion-state="emit('conversion-state', $event)" />
    <div v-if="!failed" class="lyrics-view__animated" :class="{ 'is-ready': ready }" :aria-hidden="!ready" :inert="!ready">
      <AnimatedLyrics v-bind="{ ...$attrs, ...props }"
        @render-ready="ready = true" @seek="emit('seek', $event)"
        @conversion-state="emit('conversion-state', $event)" />
    </div>
  </div>
</template>

<style scoped>
.lyrics-view { position: relative; width: 100%; height: 100%; }
.lyrics-view__animated { position: absolute; inset: 0; opacity: 0; pointer-events: none; }
.lyrics-view__animated.is-ready { opacity: 1; pointer-events: auto; }
</style>
