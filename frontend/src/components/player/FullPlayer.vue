<script setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { ChevronDown, Languages } from "lucide-vue-next";
import { coverUrl } from "../../apis/music";
import { loadLyricConverter } from "../../utils/opencc-loader.js";
import * as player from "../../core/player";
import { useMediaStore } from "../../stores/media";
import { useStatusStore } from "../../stores/status";
import PlayerControls from "./PlayerControls.vue";
import QueuePanel from "./QueuePanel.vue";
import SeekSlider from "./SeekSlider.vue";
import LyricsView from "./LyricsView.vue";

// 歌词渲染器依赖 Pixi 和 AMLL，体积较大。只有展开播放器且确实有歌词时才加载，
// 避免无歌词歌曲和普通播放场景下载这部分代码。

const media = useMediaStore();
const status = useStatusStore();
const draft = ref(null);
const isMobileLayout = ref(false);
const lyricScriptOverride = ref("");
const lyricScriptDifference = ref(false);
const lyricScriptCheckFailed = ref(false);
const lyricConversionState = ref({ script: "original", state: "original" });
const lyricConversionRetry = ref(0);
let mobileQuery = null;

const cover = computed(() => {
  if (!media.track || media.coverBroken) return "";
  return media.track.cover || (media.track.cover_url ? coverUrl(media.track) : "");
});

function onLineClick(event) {
  if (Number.isFinite(event)) player.seek(event);
}

const duration = computed(() => Math.max(status.duration || media.track?.duration || 0, 0));
const sliderValue = computed(() => draft.value ?? status.position);
const lyricAlignPosition = computed(() => (isMobileLayout.value ? 0.24 : 0.36));
const lyricScript = computed(() => lyricScriptOverride.value || status.lyricScript || "original");
const canToggleLyricScript = computed(
    () =>
      status.lyricScript !== "original" &&
      media.lyricLines.length > 0 &&
      lyricScriptDifference.value,
);
const lyricConversionFailed = computed(() => lyricScript.value !== "original" && (
  lyricScriptCheckFailed.value ||
  (lyricConversionState.value.script === lyricScript.value && lyricConversionState.value.state === "error")
));
const lyricConversionLoading = computed(() => lyricScript.value !== "original" &&
  lyricConversionState.value.script === lyricScript.value && lyricConversionState.value.state === "loading");

let lyricCheckToken = 0;
async function refreshLyricScriptDifference() {
  const lines = media.lyricLines;
  const script = status.lyricScript;
  const token = ++lyricCheckToken;
  lyricScriptDifference.value = false;
  lyricScriptCheckFailed.value = false;
  if (script === "original" || !lines.length) return;
  try {
    // Share the same statically discoverable, lazy modules as the renderer.
    const module = await loadLyricConverter(script);
    if (token !== lyricCheckToken) return;
    lyricScriptDifference.value = Boolean(module?.hasLyricScriptDifference(lines));
  } catch {
    if (token === lyricCheckToken) lyricScriptCheckFailed.value = true;
  }
}

function retryLyricConversion() {
  lyricScriptCheckFailed.value = false;
  lyricConversionState.value = { script: lyricScript.value, state: "loading" };
  lyricConversionRetry.value += 1;
  void refreshLyricScriptDifference();
}

function handleLyricConversionState(state) {
  if (state.script !== lyricScript.value) return;
  lyricConversionState.value = state;
  // The renderer may recover after an earlier toggle check failed. Refresh
  // that check from the shared, now-ready dictionary instead of a stale error.
  if (state.state === "ready" && lyricScriptCheckFailed.value) {
    void refreshLyricScriptDifference();
  }
}

function toggleLyricScript() {
  if (!canToggleLyricScript.value) return;
  lyricScriptOverride.value = lyricScript.value === "simplified" ? "traditional" : "simplified";
}

function syncMobileLayout(event) {
  isMobileLayout.value = Boolean(event.matches);
}

watch(
  () => [status.lyricScript, media.track?.id],
  () => {
    lyricScriptOverride.value = "";
    lyricConversionState.value = { script: lyricScript.value, state: "original" };
    void refreshLyricScriptDifference();
  },
  { immediate: true },
);

watch(
  () => media.lyricLines,
  () => void refreshLyricScriptDifference(),
);

onMounted(() => {
  mobileQuery = window.matchMedia("(max-width: 880px)");
  isMobileLayout.value = mobileQuery.matches;
  mobileQuery.addEventListener("change", syncMobileLayout);
});

onBeforeUnmount(() => {
  lyricCheckToken += 1;
  mobileQuery?.removeEventListener("change", syncMobileLayout);
  mobileQuery = null;
});
</script>

<template>
  <Transition name="full-player" appear>
  <section v-if="media.expanded && media.track" class="full-player">
    <div class="full-player__bg">
      <img v-if="cover" :src="cover" alt="" />
    </div>
    <button class="collapse-button" title="收起" @click="media.expanded = false">
      <ChevronDown :size="24" />
    </button>
    <div class="full-player__content">
      <div class="full-player__left">
        <div class="full-player__cover">
          <img v-if="cover" :src="cover" alt="" @error="media.coverBroken = true" />
        </div>
        <div class="full-player__meta">
          <h1>{{ media.track.title }}</h1>
          <p>{{ media.track.artist || "未知艺人" }}</p>
        </div>
      </div>
      <div class="full-player__right">
        <div class="full-player__lyrics">
          <button
            v-if="media.lyricLines.length && lyricConversionFailed"
            class="lyric-script-toggle"
            type="button"
            title="重新加载歌词转换，当前显示原文"
            @click="retryLyricConversion"
          >转换失败 · 重试</button>
          <span v-else-if="media.lyricLines.length && lyricConversionLoading" class="lyric-script-toggle" role="status">正在转换…</span>
          <button
            v-else-if="canToggleLyricScript"
            class="lyric-script-toggle"
            type="button"
            :title="lyricScript === 'simplified' ? '切换为繁体歌词' : '切换为简体歌词'"
            @click="toggleLyricScript"
          >
            <Languages :size="15" />
            {{ lyricScript === "simplified" ? "繁體" : "简体" }}
          </button>
          <LyricsView
            v-if="media.lyricLines.length"
            :lyric-lines="media.lyricLines"
            :script="lyricScript"
            :retry-token="lyricConversionRetry"
            :playing="status.isPlaying"
            :align-position="lyricAlignPosition"
            :word-fade-width="0.5"
            @seek="onLineClick"
            @conversion-state="handleLyricConversionState"
          />
          <div v-else class="lyric-empty">
            <p>{{ media.lyricLoading ? "正在加载歌词..." : "暂无歌词" }}</p>
          </div>
        </div>
      </div>
    </div>
    <div class="full-player__bottom">
      <div class="full-player__bottom-spacer"></div>
      <div class="full-player__transport">
        <PlayerControls />
        <div class="full-player__progress">
          <span>{{ Math.floor(sliderValue / 60000) }}:{{ String(Math.floor((sliderValue / 1000) % 60)).padStart(2, "0") }}</span>
          <SeekSlider
            :position="status.position"
            :duration="duration"
            :track-key="media.track?.id || ''"
            @preview="draft = $event"
            @seek="player.seek"
          />
          <span>{{ Math.floor(duration / 60000) }}:{{ String(Math.floor((duration / 1000) % 60)).padStart(2, "0") }}</span>
        </div>
      </div>
    </div>
    <Transition name="drawer">
      <QueuePanel v-if="status.queueOpen" class="queue-panel--full" />
    </Transition>
  </section>
  </Transition>
</template>
