<script setup>
import { computed, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import { ChevronLeft, Copy, Download, Image as ImageIcon, Languages, Link2, Quote, Share2, X } from "lucide-vue-next";
import { coverUrl } from "../../apis/music";
import { lyricIdentity, loadTrackLyrics } from "../../services/lyrics";
import { useMediaStore } from "../../stores/media";
import { useStatusStore } from "../../stores/status";
import { buildShareUrl, lyricLineText, shareText } from "../../utils/share";
import { loadLyricConverter } from "../../utils/opencc-loader.js";
import { lyricToggleTarget, lyricsAreChinese, lyricsLookTraditional } from "../../utils/lyric-script";

const props = defineProps({ track: { type: Object, required: true } });
const emit = defineEmits(["close"]);
const media = useMediaStore();
const status = useStatusStore();

const MAX_LYRIC_LINES = 8;

// "" → choose; "link"; "image" (song card); "lyrics" (pick lines); "lyric-image".
const mode = ref("");
const linkInput = ref(null);
const image = ref({ kind: "", url: "", blob: null, loading: false, error: "" });
let renderToken = 0;

// Lyrics as loaded, and as shown in the chosen script.
const sourceLines = shallowRef([]);
const convertedLines = shallowRef(null);
const lyricState = ref("idle");
const isPlayingTrack = computed(() => Boolean(media.track) && lyricIdentity(media.track) === lyricIdentity(props.track));
// Start in the script the full player shows for this song.
const lyricScript = ref((isPlayingTrack.value && status.shareLyricScript) || status.lyricScript || "original");
const conversion = ref("original");
const lyricsChinese = computed(() => lyricsAreChinese(sourceLines.value));
const toggleTarget = computed(() => lyricToggleTarget(lyricScript.value, lyricsLookTraditional(sourceLines.value)));
const lyricLines = computed(() => toShareLines(convertedLines.value || sourceLines.value));
const selection = ref(null);
const withTranslation = ref(true);

const link = computed(() => buildShareUrl(props.track, window.location.href));
const thumb = computed(() => props.track.cover || (props.track.cover_url ? coverUrl(props.track) : ""));
const covers = computed(() => [props.track.cover_url ? coverUrl(props.track) : "", props.track.cover]);
const baseName = computed(() => String(props.track.title || "song").replace(/[\\/:*?"<>|]+/g, " ").trim().slice(0, 60) || "song");
const fileName = computed(() => `${baseName.value}${image.value.kind === "lyrics" ? "-歌词" : ""}.png`);

const canSystemShare = typeof navigator !== "undefined" && typeof navigator.share === "function";
const canCopyImage = typeof navigator !== "undefined" && Boolean(navigator.clipboard?.write) && typeof ClipboardItem !== "undefined";
const canShareImage = computed(() => {
  if (!canSystemShare || !image.value.blob || typeof navigator.canShare !== "function") return false;
  try {
    return navigator.canShare({ files: [new File([image.value.blob], fileName.value, { type: "image/png" })] });
  } catch {
    return false;
  }
});

const title = computed(() => ({
  link: "链接分享",
  image: "图片分享",
  lyrics: "歌词分享",
  "lyric-image": "歌词图片",
}[mode.value] || "分享歌曲"));

const selectedLines = computed(() => {
  const range = selection.value;
  if (!range) return [];
  return lyricLines.value.slice(range.start, range.end + 1);
});
const hasTranslation = computed(() => selectedLines.value.some((line) => line.translation));
const excerpt = computed(() => selectedLines.value.map((line) => ({
  text: line.text,
  translation: withTranslation.value ? line.translation : "",
})));

function close() {
  emit("close");
}

function back() {
  mode.value = mode.value === "lyric-image" ? "lyrics" : "";
}

async function copyText(text, input) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Older WebViews and non-secure origins: copy from a selected field.
    input?.select();
    return Boolean(document.execCommand?.("copy"));
  }
}

async function copyLink() {
  status.toast(await copyText(link.value, linkInput.value) ? "链接已复制" : "无法自动复制，请长按链接手动复制");
}

async function systemShare(data, fallback) {
  try {
    await navigator.share(data);
  } catch (error) {
    if (error?.name !== "AbortError") await fallback();
  }
}

function systemShareLink() {
  return systemShare({ title: props.track.title || "分享歌曲", text: shareText(props.track), url: link.value }, copyLink);
}

// --- Lyrics ---------------------------------------------------------------

function toShareLines(lines) {
  return (lines || [])
    .filter((line) => !line.isBG)
    .map((line) => ({ text: lyricLineText(line), translation: String(line.translatedLyric || "").trim() }))
    .filter((line) => line.text);
}

async function chooseLyrics() {
  mode.value = "lyrics";
  if (lyricState.value === "ready" || lyricState.value === "loading") return;
  // The playing song's lyrics are already in memory.
  if (isPlayingTrack.value && media.lyricLines.length) {
    sourceLines.value = media.lyricLines;
  } else {
    lyricState.value = "loading";
    try {
      sourceLines.value = await loadTrackLyrics(props.track) || [];
    } catch {
      lyricState.value = "error";
      return;
    }
  }
  lyricState.value = toShareLines(sourceLines.value).length ? "ready" : "empty";
}

// Converting keeps every line, so a selection stays on the same lines.
watch([lyricScript, sourceLines], async ([script, lines], _previous, onCleanup) => {
  let active = true;
  onCleanup(() => { active = false; });
  convertedLines.value = null;
  if (script !== "simplified" && script !== "traditional" || !lines.length || !lyricsAreChinese(lines)) {
    conversion.value = "original";
    return;
  }
  conversion.value = "loading";
  try {
    const converter = await loadLyricConverter(script);
    if (!active) return;
    convertedLines.value = converter ? converter.convertLyricLines(lines) : null;
    conversion.value = "ready";
  } catch {
    if (active) conversion.value = "error";
  }
});

function toggleLyricScript() {
  lyricScript.value = toggleTarget.value;
}

// A failed dictionary load is retried with a fresh module URL by the loader.
function retryConversion() {
  sourceLines.value = [...sourceLines.value];
}

// Tap one line to start, then another to extend: the excerpt is always one
// unbroken run of lines. A further tap starts over.
function pickLine(index) {
  const range = selection.value;
  if (!range || range.start !== range.end || range.start === index) {
    selection.value = range && range.start === index && range.end === index ? null : { start: index, end: index };
    return;
  }
  let start = Math.min(range.start, index);
  let end = Math.max(range.start, index);
  if (end - start + 1 > MAX_LYRIC_LINES) {
    if (index > range.start) end = start + MAX_LYRIC_LINES - 1;
    else start = end - MAX_LYRIC_LINES + 1;
    status.toast(`最多选择 ${MAX_LYRIC_LINES} 行歌词`);
  }
  selection.value = { start, end };
}

function lineSelected(index) {
  const range = selection.value;
  return Boolean(range && index >= range.start && index <= range.end);
}

function lyricText() {
  const quote = excerpt.value.map((line) => (line.translation ? `${line.text}\n${line.translation}` : line.text)).join("\n");
  return `${quote}\n\n——《${props.track.title || "未知歌曲"}》${props.track.artist ? ` ${props.track.artist}` : ""}\n${link.value}`;
}

async function copyLyrics() {
  status.toast(await copyText(lyricText()) ? "歌词已复制" : "无法自动复制");
}

// --- Images ----------------------------------------------------------------

async function renderImage(kind) {
  if (image.value.url) URL.revokeObjectURL(image.value.url);
  const token = ++renderToken;
  image.value = { kind, url: "", blob: null, loading: true, error: "" };
  try {
    const cards = await import("../../services/share-card.js");
    const common = { title: props.track.title, artist: props.track.artist, covers: covers.value, link: link.value, siteName: "MHub" };
    const blob = kind === "lyrics"
      ? await cards.renderLyricCard({ ...common, lines: excerpt.value })
      : await cards.renderShareCard({ ...common, album: props.track.album });
    if (token !== renderToken) return;
    image.value = { kind, url: URL.createObjectURL(blob), blob, loading: false, error: "" };
  } catch (error) {
    if (token === renderToken) image.value = { kind, url: "", blob: null, loading: false, error: error?.message || "图片生成失败" };
  }
}

function chooseImage() {
  mode.value = "image";
  if (image.value.kind !== "song" || (!image.value.url && !image.value.loading)) void renderImage("song");
}

function makeLyricImage() {
  if (!selectedLines.value.length) return;
  mode.value = "lyric-image";
  void renderImage("lyrics");
}

function saveImage() {
  if (!image.value.url) return;
  const anchor = document.createElement("a");
  anchor.href = image.value.url;
  anchor.download = fileName.value;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
}

async function copyImage() {
  try {
    await navigator.clipboard.write([new ClipboardItem({ "image/png": image.value.blob })]);
    status.toast("图片已复制");
  } catch {
    status.toast("无法复制图片，请保存后分享");
  }
}

function systemShareImage() {
  return systemShare({
    title: props.track.title || "分享歌曲",
    text: `${shareText(props.track)}\n${link.value}`,
    files: [new File([image.value.blob], fileName.value, { type: "image/png" })],
  }, () => status.toast("系统分享不可用，请保存图片后分享"));
}

function onKeydown(event) {
  if (event.key === "Escape") {
    event.stopPropagation();
    close();
  }
}

onMounted(() => window.addEventListener("keydown", onKeydown, true));
onBeforeUnmount(() => {
  renderToken += 1;
  window.removeEventListener("keydown", onKeydown, true);
  if (image.value.url) URL.revokeObjectURL(image.value.url);
});
</script>

<template>
  <div class="dialog-backdrop share-backdrop" @click.self="close">
    <section class="dialog share" role="dialog" aria-modal="true" aria-labelledby="share-title">
      <header class="share__head">
        <button v-if="mode" class="mini-button" type="button" title="返回" aria-label="返回" @click="back"><ChevronLeft :size="18" /></button>
        <span class="share__thumb"><img v-if="thumb" :src="thumb" alt="" /></span>
        <span class="share__song">
          <h2 id="share-title">{{ title }}</h2>
          <p>{{ shareText(track) }}</p>
        </span>
        <button class="mini-button" type="button" title="关闭" aria-label="关闭" @click="close"><X :size="18" /></button>
      </header>

      <div v-if="!mode" class="share__choices">
        <button type="button" class="share__choice" @click="mode = 'link'">
          <span class="share__choice-icon"><Link2 :size="22" /></span>
          <strong>链接</strong>
          <small>复制链接，打开即进入播放</small>
        </button>
        <button type="button" class="share__choice" @click="chooseImage">
          <span class="share__choice-icon"><ImageIcon :size="22" /></span>
          <strong>图片</strong>
          <small>带封面和二维码的分享图</small>
        </button>
        <button type="button" class="share__choice" @click="chooseLyrics">
          <span class="share__choice-icon"><Quote :size="22" /></span>
          <strong>歌词</strong>
          <small>摘几句歌词，配图分享</small>
        </button>
      </div>

      <div v-else-if="mode === 'link'" class="share__body">
        <input ref="linkInput" class="share__link" :value="link" readonly aria-label="分享链接" @focus="$event.target.select()" />
        <div class="share__actions">
          <button class="primary-action" type="button" @click="copyLink"><Copy :size="16" />复制链接</button>
          <button v-if="canSystemShare" class="ghost-action" type="button" @click="systemShareLink"><Share2 :size="16" />更多分享方式</button>
        </div>
      </div>

      <div v-else-if="mode === 'lyrics'" class="share__body">
        <div v-if="lyricState === 'ready'" class="share__lyric-bar">
          <p class="share__hint">
            {{ selectedLines.length ? `已选 ${selectedLines.length} 行` : "点一行作为开头，再点一行作为结尾" }}（最多 {{ MAX_LYRIC_LINES }} 行）
          </p>
          <span v-if="lyricsChinese && conversion === 'loading'" class="share__script" role="status">正在转换…</span>
          <button
            v-else-if="lyricsChinese"
            class="share__script"
            type="button"
            :title="conversion === 'error' ? '转换失败，点击重试' : toggleTarget === 'traditional' ? '切换为繁体歌词' : '切换为简体歌词'"
            @click="conversion === 'error' ? retryConversion() : toggleLyricScript()"
          >
            <Languages :size="14" />
            {{ conversion === "error" ? "转换失败 · 重试" : toggleTarget === "traditional" ? "繁體" : "简体" }}
          </button>
        </div>
        <div class="share__lyrics" role="listbox" aria-multiselectable="true" aria-label="选择要分享的歌词">
          <span v-if="lyricState === 'loading'" class="share__status" role="status">正在加载歌词…</span>
          <span v-else-if="lyricState === 'empty'" class="share__status">这首歌暂无歌词</span>
          <span v-else-if="lyricState === 'error'" class="share__status">歌词加载失败</span>
          <template v-else>
            <button
              v-for="(line, index) in lyricLines"
              :key="index"
              type="button"
              role="option"
              class="share__lyric"
              :class="{ 'is-selected': lineSelected(index), 'is-edge': selection && (index === selection.start || index === selection.end) }"
              :aria-selected="lineSelected(index)"
              @click="pickLine(index)"
            >
              <span>{{ line.text }}</span>
              <small v-if="line.translation">{{ line.translation }}</small>
            </button>
          </template>
        </div>
        <label v-if="hasTranslation" class="share__toggle">
          <input v-model="withTranslation" type="checkbox" />
          附带翻译
        </label>
        <div class="share__actions">
          <button class="primary-action" type="button" :disabled="!selectedLines.length" @click="makeLyricImage"><ImageIcon :size="16" />生成图片</button>
          <button class="ghost-action" type="button" :disabled="!selectedLines.length" @click="copyLyrics"><Copy :size="16" />复制歌词</button>
        </div>
      </div>

      <div v-else class="share__body">
        <div class="share__preview" :class="{ 'share__preview--lyrics': mode === 'lyric-image' }">
          <img v-if="image.url" :src="image.url" alt="分享图片预览" />
          <span v-else-if="image.error" class="share__status">{{ image.error }}</span>
          <span v-else class="share__status" role="status">正在生成图片…</span>
        </div>
        <div class="share__actions">
          <button class="primary-action" type="button" :disabled="!image.url" @click="saveImage"><Download :size="16" />保存图片</button>
          <button v-if="canCopyImage" class="ghost-action" type="button" :disabled="!image.blob" @click="copyImage"><Copy :size="16" />复制图片</button>
          <button v-if="canShareImage" class="ghost-action" type="button" @click="systemShareImage"><Share2 :size="16" />更多分享方式</button>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.share-backdrop { z-index: 130; }
/* minmax(0, …): a long title must truncate, not widen the dialog. */
.share { width: min(460px, 100%); display: grid; grid-template-columns: minmax(0, 1fr); gap: 18px; overflow-x: hidden; }
.share > * { min-width: 0; }
.share__head { display: flex; align-items: center; gap: 12px; }
.share__thumb { flex: none; width: 46px; height: 46px; overflow: hidden; border-radius: 8px; background: var(--surface-muted); }
.share__thumb img { width: 100%; height: 100%; object-fit: cover; display: block; }
.share__song { flex: 1; min-width: 0; }
.share__song h2 { margin: 0; font-size: 17px; }
.share__song p { margin: 3px 0 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-muted); font-size: 13px; }
.share__choices { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; }
.share__choice { display: grid; justify-items: center; align-content: start; gap: 6px; padding: 18px 10px 16px; border: 1px solid var(--line); border-radius: 14px; background: var(--surface-muted); color: var(--text); text-align: center; cursor: pointer; transition: background var(--motion-fast), border-color var(--motion-fast); }
.share__choice:hover, .share__choice:focus-visible { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 8%, var(--surface-muted)); }
.share__choice-icon { display: grid; place-items: center; width: 46px; height: 46px; margin-bottom: 4px; border-radius: 999px; background: var(--accent); color: var(--on-accent); }
.share__choice strong { font-size: 15px; }
.share__choice small { color: var(--text-muted); font-size: 12px; line-height: 1.4; }
.share__body { display: grid; gap: 14px; min-height: 0; }
.share__link { width: 100%; min-height: 40px; padding: 0 12px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface-muted); color: var(--text); font: inherit; font-size: 13px; text-overflow: ellipsis; }
.share__actions { display: flex; flex-wrap: wrap; gap: 10px; }
.share__actions > button { flex: 1 1 auto; justify-content: center; }
.share__preview { display: grid; place-items: center; justify-self: center; width: min(100%, 345px, 39vh); aspect-ratio: 3 / 4; overflow: hidden; border-radius: 14px; background: var(--surface-muted); }
.share__preview img { width: 100%; height: 100%; object-fit: contain; display: block; }
.share__preview--lyrics { width: min(100%, 360px); aspect-ratio: auto; min-height: 200px; background: transparent; }
.share__preview--lyrics img { width: auto; max-width: 100%; height: auto; max-height: 56vh; border-radius: 14px; }
.share__status { padding: 24px 18px; color: var(--text-muted); font-size: 13px; text-align: center; }
.share__hint { margin: 0; color: var(--text-muted); font-size: 12px; }
.share__lyric-bar { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
.share__script { flex: none; display: inline-flex; align-items: center; gap: 5px; min-height: 28px; padding: 0 11px; border: 1px solid var(--line); border-radius: 999px; background: var(--surface-muted); color: var(--text); font: inherit; font-size: 12px; font-weight: 600; cursor: pointer; }
span.share__script { cursor: default; color: var(--text-muted); }
.share__lyrics { display: grid; align-content: start; gap: 2px; max-height: min(46vh, 420px); overflow-y: auto; overscroll-behavior: contain; margin: 0 -6px; padding: 2px 6px; }
.share__lyric { display: grid; gap: 2px; padding: 8px 12px; border: 0; border-radius: 10px; background: transparent; color: var(--text); font: inherit; font-size: 15px; line-height: 1.4; text-align: left; cursor: pointer; transition: background var(--motion-fast); }
.share__lyric small { color: var(--text-muted); font-size: 12px; }
.share__lyric:hover { background: var(--surface-muted); }
.share__lyric.is-selected { background: color-mix(in srgb, var(--accent) 14%, transparent); }
.share__lyric.is-edge { box-shadow: inset 3px 0 0 var(--accent); }
.share__lyric.is-selected span { font-weight: 650; }
.share__toggle { display: inline-flex; align-items: center; gap: 8px; color: var(--text-secondary, var(--text)); font-size: 13px; cursor: pointer; }
@media (max-width: 520px) {
  .share__choices { grid-template-columns: 1fr; }
  .share__choice { grid-template-columns: auto 1fr; justify-items: start; text-align: left; column-gap: 14px; padding: 14px; }
  .share__choice-icon { grid-row: span 2; margin: 0; }
}
</style>
