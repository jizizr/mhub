<script setup>
import { LyricPlayer as CoreLyricPlayer } from "@applemusic-like-lyrics/core";
import "@applemusic-like-lyrics/core/style.css";
import { computed, onBeforeUnmount, onMounted, ref, shallowRef, toRaw, watch, watchEffect } from "vue";
import * as playback from "../../services/playback";
import { loadLyricConverter } from "../../utils/opencc-loader.js";

const props = defineProps({
  lyricLines: { type: Array, default: () => [] },
  script: { type: String, default: "original" },
  retryToken: { type: Number, default: 0 },
  playing: { type: Boolean, default: false },
  alignPosition: { type: Number, default: 0.36 },
  wordFadeWidth: { type: Number, default: 0.5 },
  hidePassedLines: { type: Boolean, default: false },
  enableBlur: { type: Boolean, default: false },
  showTranslation: { type: Boolean, default: true },
  showLineRomanization: { type: Boolean, default: true },
  showWordRomanization: { type: Boolean, default: true },
});

const emit = defineEmits(["seek", "conversion-state", "render-ready"]);
const wrapperRef = ref(null);
const bottomLineEl = ref(null);
const isFrozen = ref(false);
let player = null;
let rafId = 0;
let lastFrameTime = 0;
let pendingLyrics = null;
let scrollHoldUntil = 0;
let lastScrollToIndex = -1;
let userScrolledSinceAutoAlign = false;
let renderReady = false;
const openccModule = shallowRef(null);

const SCROLL_HOLD_MS = 240;
const PAUSED_SCROLL_HOLD_MS = 60 * 60 * 1000;

function cloneLines(lines) {
  return JSON.parse(JSON.stringify(toRaw(lines || [])));
}

const processedLyrics = computed(() => {
  const source = cloneLines(props.lyricLines);
  const converted =
    props.script === "original" || openccModule.value?.script !== props.script
      ? source
      : openccModule.value.module.convertLyricLines(source);
  return converted.map((line) => {
    const nextLine = {
      ...line,
      translatedLyric: props.showTranslation ? line.translatedLyric : "",
      romanLyric: props.showLineRomanization ? line.romanLyric : "",
    };
    if (Array.isArray(line.words)) {
      nextLine.words = line.words.map((word) => {
        const nextWord = { ...word };
        if (!props.showWordRomanization) delete nextWord.romanWord;
        return nextWord;
      });
    }
    return nextLine;
  });
});

watch(
  [() => props.script, () => props.lyricLines, () => props.retryToken],
  async ([script, lines, retryToken], _previous, onCleanup) => {
    let active = true;
    // Vue calls cleanup on the next request and when the component unmounts.
    onCleanup(() => { active = false; });
    const isCurrent = () => active && props.script === script &&
      props.lyricLines === lines && props.retryToken === retryToken;
    openccModule.value = null;
    if (script !== "simplified" && script !== "traditional") {
      emit("conversion-state", { script, state: "original" });
      return;
    }
    emit("conversion-state", { script, state: "loading" });
    try {
      const module = await loadLyricConverter(script);
      if (isCurrent() && module) {
        openccModule.value = { script, module };
        emit("conversion-state", { script, state: "ready" });
      }
    } catch (error) {
      // Keep the original lyrics usable and offer retry without changing songs.
      if (isCurrent()) {
        emit("conversion-state", { script, state: "error" });
        console.warn("歌词字形转换暂不可用", error);
      }
    }
  },
  { immediate: true },
);

function updateFrame(time) {
  if (!player) return;
  if (!lastFrameTime) lastFrameTime = time;
  if (props.playing && !isUserScrollActive(time)) {
    const beforeIndex = player.timelineState?.scrollToIndex ?? -1;
    player.setCurrentTime(Math.round(playback.getCurrentTime()));
    const afterIndex = player.timelineState?.scrollToIndex ?? -1;
    if (afterIndex !== beforeIndex || afterIndex !== lastScrollToIndex) {
      maybeAutoAlignCurrentLine();
      lastScrollToIndex = afterIndex;
    }
  }
  player.update(time - lastFrameTime);
  if (!renderReady) {
    const viewport = wrapperRef.value?.getBoundingClientRect();
    const line = [...(player.getElement().querySelectorAll("[class*='lyricMainLine']"))].find((element) => {
      const rect = element.getBoundingClientRect();
      return viewport && element.textContent?.trim() && rect.width > 0 && rect.height > 0 && rect.bottom > viewport.top && rect.top < viewport.bottom &&
        getComputedStyle(element).visibility === "visible";
    });
    if (line) { renderReady = true; emit("render-ready"); }
  }
  lastFrameTime = time;
  rafId = window.requestAnimationFrame(updateFrame);
}

function startRaf() {
  if (!rafId) rafId = window.requestAnimationFrame(updateFrame);
}

function stopRaf() {
  if (rafId) window.cancelAnimationFrame(rafId);
  rafId = 0;
  lastFrameTime = 0;
}

function applyOptions() {
  if (!player) return;
  player.setAlignPosition(props.alignPosition);
  player.setAlignAnchor(props.alignPosition > 0.4 ? "center" : "top");
  player.setWordFadeWidth(props.wordFadeWidth);
  player.setHidePassedLines(props.hidePassedLines);
  player.setEnableBlur(props.enableBlur);
  player.setEnableSpring(true);
  player.setEnableScale(true);
  player.setOverscanPx(Math.max(900, Math.round((wrapperRef.value?.clientHeight || 0) * 1.6)));
}

function handleLineClick(event) {
  const line = event?.line?.getLine?.();
  const start = line?.startTime ?? line?.words?.[0]?.startTime;
  if (Number.isFinite(start)) {
    emit("seek", start);
    player?.setCurrentTime(start, true);
  }
}

function handleVisibility() {
  if (document.hidden) {
    stopRaf();
    player?.pause();
  } else if (!isFrozen.value) {
    if (props.playing) player?.resume();
    else player?.pause();
    startRaf();
  }
}

function isUserScrollActive(time = performance.now()) {
  const scrollState = player?.scrollState;
  return Boolean(
    scrollState?.isUserScrolling ||
      time < scrollHoldUntil ||
      (!props.playing && Math.abs(scrollState?.scrollOffset || 0) > 0),
  );
}

function holdAutoTimeForScroll() {
  const holdMs = props.playing ? SCROLL_HOLD_MS : PAUSED_SCROLL_HOLD_MS;
  scrollHoldUntil = performance.now() + holdMs;
  userScrolledSinceAutoAlign = true;
  cancelCoreScrollSnap();
  window.requestAnimationFrame(cancelCoreScrollSnap);
}

function cancelCoreScrollSnap() {
  if (!player) return;
  if (player.scrolledHandler) {
    window.clearTimeout(player.scrolledHandler);
    player.scrolledHandler = 0;
  }
  if (player.scrollState) {
    player.scrollState.isScrolled = true;
  }
}

function currentLineIsVisible() {
  if (!player?.getElement) return false;
  const index = player.timelineState?.scrollToIndex;
  const group = Number.isFinite(index) ? player.currentLyricGroups?.[index] : null;
  const element = group?.element || group?.getElement?.();
  if (!element) return false;
  const viewport = player.getElement().getBoundingClientRect();
  const rect = element.getBoundingClientRect();
  const margin = viewport.height * 0.12;
  return rect.bottom > viewport.top + margin && rect.top < viewport.bottom - margin;
}

function maybeAutoAlignCurrentLine() {
  if (!userScrolledSinceAutoAlign) return;
  if (isUserScrollActive()) return;
  if (!currentLineIsVisible()) return;
  player?.resetScroll?.();
  player?.calcLayout?.(false, false);
  userScrolledSinceAutoAlign = false;
}

onMounted(() => {
  if (!wrapperRef.value) return;
  player = new CoreLyricPlayer();
  const element = player.getElement();
  element.style.width = "100%";
  element.style.height = "100%";
  element.addEventListener("wheel", holdAutoTimeForScroll, { passive: true });
  element.addEventListener("touchstart", holdAutoTimeForScroll, { passive: true });
  element.addEventListener("touchmove", holdAutoTimeForScroll, { passive: true });
  element.addEventListener("touchend", holdAutoTimeForScroll, { passive: true });
  element.addEventListener("touchcancel", holdAutoTimeForScroll, { passive: true });
  element.addEventListener("pointerdown", holdAutoTimeForScroll, { passive: true });
  element.addEventListener("pointerup", holdAutoTimeForScroll, { passive: true });
  wrapperRef.value.appendChild(element);
  const bottomEl = player.getBottomLineElement();
  if (bottomEl) {
    bottomEl.classList.add("lp-line", "lp-credit");
    bottomLineEl.value = bottomEl;
  }
  player.addEventListener("line-click", handleLineClick);
  applyOptions();
  player.setLyricLines(processedLyrics.value, Math.round(playback.getCurrentTime()));
  player.setCurrentTime(Math.round(playback.getCurrentTime()), true);
  lastScrollToIndex = player.timelineState?.scrollToIndex ?? -1;
  if (props.playing) player.resume();
  else player.pause();
  startRaf();
  document.addEventListener("visibilitychange", handleVisibility);
});

onBeforeUnmount(() => {
  document.removeEventListener("visibilitychange", handleVisibility);
  stopRaf();
  if (player) {
    const element = player.getElement();
    element.removeEventListener("wheel", holdAutoTimeForScroll);
    element.removeEventListener("touchstart", holdAutoTimeForScroll);
    element.removeEventListener("touchmove", holdAutoTimeForScroll);
    element.removeEventListener("touchend", holdAutoTimeForScroll);
    element.removeEventListener("touchcancel", holdAutoTimeForScroll);
    element.removeEventListener("pointerdown", holdAutoTimeForScroll);
    element.removeEventListener("pointerup", holdAutoTimeForScroll);
    player.removeEventListener("line-click", handleLineClick);
    player.dispose();
    player = null;
    bottomLineEl.value = null;
  }
});

watch(
  processedLyrics,
  (lines) => {
    if (!player) return;
    if (isFrozen.value) {
      pendingLyrics = lines;
    } else {
      player.setLyricLines(lines);
      player.setCurrentTime(Math.round(playback.getCurrentTime()), true);
    }
  },
);

watchEffect(() => {
  if (!player) return;
  if (!isFrozen.value && !document.hidden) {
    if (props.playing) player.resume();
    else player.pause();
    startRaf();
  } else {
    stopRaf();
    player.pause();
  }
});

function freeze() {
  isFrozen.value = true;
}

function resume() {
  if (pendingLyrics && player) {
    player.setLyricLines(pendingLyrics);
    pendingLyrics = null;
  }
  isFrozen.value = false;
}

watch(() => [props.alignPosition, props.wordFadeWidth], applyOptions);

watch(
  () => props.playing,
  (playing) => {
    if (!player) return;
    if (playing) {
      scrollHoldUntil = 0;
      player.resetScroll?.();
      player.setCurrentTime(Math.round(playback.getCurrentTime()), true);
      lastScrollToIndex = player.timelineState?.scrollToIndex ?? -1;
      userScrolledSinceAutoAlign = false;
      player.resume();
    }
  },
);

defineExpose({ freeze, resume, lyricPlayer: () => player });
</script>

<template>
  <div ref="wrapperRef" class="splayer-lyrics" />
  <Teleport v-if="bottomLineEl" :to="bottomLineEl">
    <slot name="bottom" />
  </Teleport>
</template>

<style scoped>
.splayer-lyrics {
  width: 100%;
  height: 100%;
  position: relative;
  overflow: hidden;
  user-select: none;
  contain: layout paint style;
  transform: translateZ(0);
  backface-visibility: hidden;
}

:deep(.amll-lyric-player) {
  --amll-lp-font-size: 1em;
  --amll-lp-color: var(--lp-color, #fff);
  width: 100%;
  height: 100%;
  transform: translateZ(0);
  backface-visibility: hidden;
}
</style>
