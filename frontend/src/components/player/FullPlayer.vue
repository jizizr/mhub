<script setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { ChevronDown, Download, Languages, LoaderCircle, Share } from "lucide-vue-next";
import { coverUrl } from "../../apis/music";
import * as player from "../../core/player";
import { useMediaStore } from "../../stores/media";
import { useStatusStore } from "../../stores/status";
import { COVER_TONE_FALLBACK, coverTone } from "../../utils/cover-tone";
import { canShareTrack } from "../../utils/share";
import { downloadKey, downloadTrack, downloads } from "../../services/track-download";
import { lyricToggleTarget, lyricsAreChinese, lyricsLookTraditional } from "../../utils/lyric-script";
import PlayerControls from "./PlayerControls.vue";
import QueuePanel from "./QueuePanel.vue";
import SeekSlider from "./SeekSlider.vue";
import LyricsView from "./LyricsView.vue";
import LyricsBackground from "./LyricsBackground.vue";
import ListeningSettings from "./ListeningSettings.vue";
import VolumeControl from "./VolumeControl.vue";

// 歌词渲染器依赖 Pixi 和 AMLL，体积较大。只有展开播放器且确实有歌词时才加载，
// 避免无歌词歌曲和普通播放场景下载这部分代码。

const emit = defineEmits(["chrome-color"]);
const media = useMediaStore();
const status = useStatusStore();
const draft = ref(null);
const isMobileLayout = ref(false);
const lyricScriptOverride = ref("");
const lyricScriptCheckFailed = ref(false);
const lyricConversionState = ref({ script: "original", state: "original" });
const lyricConversionRetry = ref(0);
let mobileQuery = null;

const cover = computed(() => {
  if (!media.track || media.coverBroken) return "";
  return media.track.cover || (media.track.cover_url ? coverUrl(media.track) : "");
});

// While open, the status and gesture bars take the backdrop's tone instead of
// the page theme, so the view reads as edge to edge.
const open = computed(() => Boolean(media.expanded && media.track));
let toneToken = 0;
watch([open, cover], async ([isOpen, url]) => {
  const token = ++toneToken;
  if (!isOpen) {
    emit("chrome-color", "");
    return;
  }
  emit("chrome-color", COVER_TONE_FALLBACK);
  const tone = await coverTone(url);
  if (token === toneToken) emit("chrome-color", tone);
}, { immediate: true });

// Saves the decrypted file; a song already cached whole needs no request.
const download = computed(() => (media.track ? downloads[downloadKey(media.track)] : null));
const canDownload = computed(() => Boolean(media.track && (media.track.playback_id || media.track.id) && media.track.platform));
const downloadTitle = computed(() => {
  const state = download.value;
  if (!state) return "下载音频文件";
  if (state.stage === "preparing") return "正在准备音频文件…";
  if (state.stage === "downloading" && state.progress) return `正在下载 ${Math.round(state.progress * 100)}%`;
  return "正在下载…";
});
function startDownload() {
  if (!canDownload.value || download.value) return;
  void downloadTrack(media.track, { notify: (message) => status.toast(message) });
}

function onLineClick(event) {
  if (Number.isFinite(event)) player.seek(event);
}

const duration = computed(() => Math.max(status.duration || media.track?.duration || 0, 0));
const sliderValue = computed(() => draft.value ?? status.position);
const lyricAlignPosition = computed(() => (isMobileLayout.value ? 0.24 : 0.36));
const lyricScript = computed(() => lyricScriptOverride.value || status.lyricScript || "original");
// The toggle is always offered for Chinese lyrics, as Apple Music offers its
// lyric-script switch. Under the "original" setting it targets the other
// script; a dictionary loads only once a conversion is actually chosen.
const lyricsHaveHan = computed(() => lyricsAreChinese(media.lyricLines));
const lyricsAreTraditional = ref(false);
const toggleTarget = computed(() => lyricToggleTarget(lyricScript.value, lyricsAreTraditional.value));
const canToggleLyricScript = computed(() => media.lyricLines.length > 0 && lyricsHaveHan.value);
const lyricConversionFailed = computed(() => lyricScript.value !== "original" && (
  lyricScriptCheckFailed.value ||
  (lyricConversionState.value.script === lyricScript.value && lyricConversionState.value.state === "error")
));
const lyricConversionLoading = computed(() => lyricScript.value !== "original" &&
  lyricConversionState.value.script === lyricScript.value && lyricConversionState.value.state === "loading");

function refreshLyricScriptDifference() {
  lyricScriptCheckFailed.value = false;
  lyricsAreTraditional.value = lyricsLookTraditional(media.lyricLines);
}

function retryLyricConversion() {
  lyricScriptCheckFailed.value = false;
  lyricConversionState.value = { script: lyricScript.value, state: "loading" };
  lyricConversionRetry.value += 1;
  refreshLyricScriptDifference();
}

function handleLyricConversionState(state) {
  if (state.script !== lyricScript.value) return;
  lyricConversionState.value = state;
  // The renderer may recover after an earlier toggle check failed. Refresh
  // that check from the shared, now-ready dictionary instead of a stale error.
  if (state.state === "ready" && lyricScriptCheckFailed.value) {
    refreshLyricScriptDifference();
  }
}

function toggleLyricScript() {
  if (!canToggleLyricScript.value) return;
  lyricScriptOverride.value = toggleTarget.value;
}

function syncMobileLayout(event) {
  isMobileLayout.value = Boolean(event.matches);
}

watch(
  () => [status.lyricScript, media.track?.id],
  () => {
    lyricScriptOverride.value = "";
    lyricConversionState.value = { script: lyricScript.value, state: "original" };
    refreshLyricScriptDifference();
  },
  { immediate: true },
);

watch(
  () => media.lyricLines,
  () => refreshLyricScriptDifference(),
);

onMounted(() => {
  mobileQuery = window.matchMedia("(max-width: 880px)");
  isMobileLayout.value = mobileQuery.matches;
  mobileQuery.addEventListener("change", syncMobileLayout);
});

onBeforeUnmount(() => {
  toneToken += 1;
  emit("chrome-color", "");
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
    <LyricsBackground :cover="cover" :playing="status.isPlaying" />
    <button class="collapse-button" title="收起" @click="media.expanded = false">
      <ChevronDown :size="24" />
    </button>
    <div class="full-player__content">
      <div class="full-player__left">
        <div class="full-player__cover">
          <img v-if="cover" :src="cover" alt="" @error="media.coverBroken = true" />
        </div>
        <div class="full-player__meta">
          <div class="full-player__meta-text">
            <h1>{{ media.track.title }}</h1>
            <p>{{ media.track.artist || "未知艺人" }}</p>
          </div>
          <button
            v-if="canDownload"
            class="full-player__share full-player__download"
            :class="{ 'is-busy': download }"
            type="button"
            :title="downloadTitle"
            :aria-label="downloadTitle"
            :aria-busy="Boolean(download)"
            :style="download?.progress ? { '--download-progress': `${download.progress * 360}deg` } : undefined"
            @click="startDownload"
          >
            <LoaderCircle v-if="download" class="full-player__download-spin" :size="19" />
            <Download v-else :size="19" />
          </button>
          <button
            v-if="canShareTrack(media.track)"
            class="full-player__share"
            type="button"
            title="分享"
            aria-label="分享这首歌"
            @click="status.shareLyricScript = lyricScript; status.shareTrack = media.track"
          ><Share :size="19" /></button>
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
            :title="toggleTarget === 'traditional' ? '切换为繁体歌词' : '切换为简体歌词'"
            @click="toggleLyricScript"
          >
            <Languages :size="15" />
            {{ toggleTarget === "traditional" ? "繁體" : "简体" }}
          </button>
          <LyricsView
            v-if="media.lyricLines.length"
            :lyric-lines="media.lyricLines"
            :script="lyricScript"
            :retry-token="lyricConversionRetry"
            :playing="status.isPlaying"
            :align-position="lyricAlignPosition"
            :word-fade-width="0.5"
            :word-mode="status.lyricWordMode"
            :show-line-romanization="status.lyricRoman"
            :show-word-romanization="status.lyricRoman"
            @seek="onLineClick"
            @conversion-state="handleLyricConversionState"
          >
            <template v-if="media.lyricCredits.length" #bottom>
              <ul class="lyric-credits" aria-label="歌曲署名">
                <li v-for="credit in media.lyricCredits" :key="credit">{{ credit }}</li>
              </ul>
            </template>
          </LyricsView>
          <div v-else class="lyric-empty">
            <p>{{ media.lyricLoading ? "正在加载歌词..." : "暂无歌词" }}</p>
          </div>
        </div>
      </div>
    </div>
    <div class="full-player__bottom">
      <div class="full-player__bottom-spacer"></div>
      <div class="full-player__transport">
        <div class="full-player__capsule">
          <PlayerControls />
          <!-- Volume and settings stay reachable while the lyrics cover the
               page bar, as in Apple Music's full-screen player. -->
          <span class="full-player__capsule-divider" aria-hidden="true"></span>
          <VolumeControl />
          <ListeningSettings compact />
        </div>
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
