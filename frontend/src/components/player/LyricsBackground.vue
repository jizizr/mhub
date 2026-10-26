<script setup>
import { onBeforeUnmount, onMounted, ref, watch } from "vue";

// Apple Music's now-playing backdrop: the artwork's colours flowing as a
// slowly moving mesh gradient. AMLL ships the same effect (WebGL), loaded
// lazily with the lyrics engine. The cover must be CORS-readable; if WebGL or
// the image fails, `ready` stays false and the CSS blurred cover remains.
const props = defineProps({
  cover: { type: String, default: "" },
  playing: { type: Boolean, default: false },
});

const host = ref(null);
const ready = ref(false);
let background = null;
let disposed = false;
let albumToken = 0;
const reducedMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)");

async function setAlbum(url) {
  const token = ++albumToken;
  if (!background || !url) {
    ready.value = false;
    return;
  }
  // The renderer only finishes its cross-fade while it is drawing, so it
  // runs during the change even when playback is paused.
  background.resume();
  try {
    await background.setAlbum(url);
    if (token === albumToken && !disposed) ready.value = true;
  } catch {
    if (token === albumToken) ready.value = false;
  } finally {
    if (token === albumToken) syncMotion();
  }
}

function syncMotion() {
  if (!background) return;
  background.setStaticMode(Boolean(reducedMotion?.matches));
  if (props.playing && !reducedMotion?.matches) background.resume();
  else background.pause();
}

onMounted(async () => {
  try {
    const { BackgroundRender, MeshGradientRenderer } = await import("@applemusic-like-lyrics/core");
    if (disposed) return;
    background = BackgroundRender.new(MeshGradientRenderer);
    const canvas = background.getElement();
    canvas.classList.add("lyrics-background__canvas");
    host.value?.appendChild(canvas);
    background.setRenderScale(0.5);
    background.setFPS(60);
    background.setFlowSpeed(1.5);
    background.setHasLyric(true);
    syncMotion();
    await setAlbum(props.cover);
  } catch {
    ready.value = false;
  }
  reducedMotion?.addEventListener?.("change", syncMotion);
});

watch(() => props.cover, (url) => void setAlbum(url));
watch(() => props.playing, syncMotion);

onBeforeUnmount(() => {
  disposed = true;
  albumToken += 1;
  reducedMotion?.removeEventListener?.("change", syncMotion);
  background?.dispose();
  background = null;
});
</script>

<template>
  <div ref="host" class="lyrics-background" :class="{ 'is-ready': ready }" aria-hidden="true"></div>
</template>
