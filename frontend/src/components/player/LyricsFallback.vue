<script setup>
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import { useStatusStore } from "../../stores/status";
import { loadLyricConverter } from "../../utils/opencc-loader.js";

const props = defineProps({
  lyricLines: { type: Array, default: () => [] },
  script: { type: String, default: "original" },
  retryToken: { type: Number, default: 0 },
});
const emit = defineEmits(["seek", "conversion-state"]);
const status = useStatusStore();
const viewport = ref(null);
const converted = ref(null);
let generation = 0;
let disposed = false;
const lines = computed(() => converted.value || props.lyricLines);
const currentIndex = computed(() => {
  let index = 0;
  for (let i = 0; i < lines.value.length; i += 1) {
    if (lines.value[i].startTime > status.position) break;
    index = i;
  }
  return index;
});
const textOf = (line) => line.words?.map((word) => word.word).join("") || "";

watch([() => props.script, () => props.lyricLines, () => props.retryToken], async () => {
  const token = ++generation;
  const script = props.script;
  converted.value = null;
  if (script !== "simplified" && script !== "traditional") return;
  emit("conversion-state", { script, state: "loading" });
  try {
    const converter = await loadLyricConverter(script);
    if (disposed || token !== generation) return;
    converted.value = converter.convertLyricLines(props.lyricLines);
    emit("conversion-state", { script, state: "ready" });
  } catch {
    if (!disposed && token === generation) emit("conversion-state", { script, state: "error" });
  }
}, { immediate: true });

watch([currentIndex, lines], async () => {
  await nextTick();
  if (disposed || !viewport.value) return;
  const line = viewport.value.children[currentIndex.value];
  if (line) viewport.value.scrollTop = Math.max(0, line.offsetTop - viewport.value.clientHeight * 0.3);
}, { immediate: true });
onBeforeUnmount(() => { disposed = true; generation += 1; });
</script>

<template>
  <div ref="viewport" class="lyrics-fallback" aria-label="歌词">
    <button v-for="(line, index) in lines" :key="index" type="button"
      class="lyrics-fallback__line" :class="{ 'is-current': index === currentIndex }"
      :aria-current="index === currentIndex ? 'true' : undefined" @click="emit('seek', line.startTime)">
      <span>{{ textOf(line) }}</span>
      <small v-if="line.translatedLyric">{{ line.translatedLyric }}</small>
    </button>
  </div>
</template>

<style scoped>
.lyrics-fallback { position: absolute; inset: 0; overflow: auto; overscroll-behavior: contain; padding: 22vh 20px 30vh; scrollbar-width: none; }
.lyrics-fallback::-webkit-scrollbar { display: none; }
.lyrics-fallback__line { display: block; width: 100%; border: 0; background: none; color: rgba(255,255,255,.48); padding: .5em 0; text-align: left; font: inherit; line-height: 1.3; cursor: pointer; overflow-wrap: anywhere; }
.lyrics-fallback__line.is-current { color: rgba(255,255,255,.92); }
.lyrics-fallback__line small { display: block; margin-top: .3em; font-size: .5em; color: inherit; }
.lyrics-fallback__line:focus-visible { outline: 2px solid currentColor; outline-offset: 4px; border-radius: 8px; }
</style>
