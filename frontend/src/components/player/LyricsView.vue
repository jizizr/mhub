<script setup>
import { defineAsyncComponent, onBeforeUnmount, onErrorCaptured, ref } from "vue";
import LyricsFallback from "./LyricsFallback.vue";

defineOptions({ inheritAttrs: false });
const props = defineProps({
  lyricLines: { type: Array, default: () => [] },
  script: { type: String, default: "original" },
  retryToken: { type: Number, default: 0 },
  alignPosition: { type: Number, default: 0.36 },
});
const emit = defineEmits(["seek", "conversion-state"]);
const ready = ref(false);
const failed = ref(false);
// The readable fallback only appears when the animated renderer is slow (cold
// chunk). Showing it on every open and then swapping renderers read as a jump.
const FALLBACK_DELAY_MS = 300;
const FALLBACK_FADE_MS = 220;
const fallbackVisible = ref(false);
const fallbackRetired = ref(false);
let fallbackTimer = window.setTimeout(() => { fallbackVisible.value = true; }, FALLBACK_DELAY_MS);
const AnimatedLyrics = defineAsyncComponent({
  loader: () => import("./SPlayerLyrics.vue"),
  timeout: 15000,
});

function clearFallbackTimer() {
  window.clearTimeout(fallbackTimer);
  fallbackTimer = 0;
}

function handleRenderReady() {
  clearFallbackTimer();
  ready.value = true;
  if (!fallbackVisible.value) {
    fallbackRetired.value = true;
    return;
  }
  // Keep a shown fallback underneath until the animated layer has faded in.
  fallbackTimer = window.setTimeout(() => { if (ready.value) fallbackRetired.value = true; }, FALLBACK_FADE_MS);
}

// A failed optional renderer must not remove the already available lyric text.
onErrorCaptured((error) => {
  clearFallbackTimer();
  failed.value = true;
  ready.value = false;
  fallbackVisible.value = true;
  fallbackRetired.value = false;
  console.warn("[lyrics] animated view unavailable; retaining readable lyrics", error);
  return false;
});
onBeforeUnmount(clearFallbackTimer);
</script>

<template>
  <div class="lyrics-view">
    <LyricsFallback v-if="!fallbackRetired" v-bind="props" :class="{ 'is-pending': !fallbackVisible }"
      @seek="emit('seek', $event)" @conversion-state="emit('conversion-state', $event)" />
    <div v-if="!failed" class="lyrics-view__animated" :class="{ 'is-ready': ready }" :aria-hidden="!ready" :inert="!ready">
      <AnimatedLyrics v-bind="{ ...$attrs, ...props }"
        @render-ready="handleRenderReady" @seek="emit('seek', $event)"
        @conversion-state="emit('conversion-state', $event)" />
    </div>
  </div>
</template>

<style scoped>
.lyrics-view { position: relative; width: 100%; height: 100%; }
.lyrics-view > .is-pending { visibility: hidden; }
.lyrics-view__animated { position: absolute; inset: 0; opacity: 0; pointer-events: none; transition: opacity 180ms ease-out; }
.lyrics-view__animated.is-ready { opacity: 1; pointer-events: auto; }
@media (prefers-reduced-motion: reduce) { .lyrics-view__animated { transition: none; } }
</style>
