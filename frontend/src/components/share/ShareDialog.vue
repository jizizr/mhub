<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { ChevronLeft, Copy, Download, Image as ImageIcon, Link2, Share2, X } from "lucide-vue-next";
import { coverUrl } from "../../apis/music";
import { useStatusStore } from "../../stores/status";
import { buildShareUrl, shareText } from "../../utils/share";

const props = defineProps({ track: { type: Object, required: true } });
const emit = defineEmits(["close"]);
const status = useStatusStore();

const mode = ref("");
const linkInput = ref(null);
const imageUrl = ref("");
const imageBlob = ref(null);
const imageLoading = ref(false);
const imageError = ref("");
let renderToken = 0;

const link = computed(() => buildShareUrl(props.track, window.location.href));
// The QR code drops the cover address so it stays easy to scan.
const qrLink = computed(() => buildShareUrl({ ...props.track, cover: "" }, window.location.href));
const thumb = computed(() => props.track.cover || (props.track.cover_url ? coverUrl(props.track) : ""));
const fileName = computed(() => `${String(props.track.title || "song").replace(/[\\/:*?"<>|]+/g, " ").trim().slice(0, 60) || "song"}.png`);

const canSystemShare = typeof navigator !== "undefined" && typeof navigator.share === "function";
const canCopyImage = typeof navigator !== "undefined" && Boolean(navigator.clipboard?.write) && typeof ClipboardItem !== "undefined";
const canShareImage = computed(() => {
  if (!canSystemShare || !imageBlob.value || typeof navigator.canShare !== "function") return false;
  try {
    return navigator.canShare({ files: [new File([imageBlob.value], fileName.value, { type: "image/png" })] });
  } catch {
    return false;
  }
});

function close() {
  emit("close");
}

async function copyLink() {
  try {
    await navigator.clipboard.writeText(link.value);
  } catch {
    // Older WebViews and non-secure origins: copy from the selected field.
    linkInput.value?.select();
    if (!document.execCommand?.("copy")) {
      status.toast("无法自动复制，请长按链接手动复制");
      return;
    }
  }
  status.toast("链接已复制");
}

async function systemShareLink() {
  try {
    await navigator.share({ title: props.track.title || "分享歌曲", text: shareText(props.track), url: link.value });
  } catch (error) {
    if (error?.name !== "AbortError") status.toast("系统分享不可用，已改为复制链接");
    if (error?.name !== "AbortError") await copyLink();
  }
}

async function chooseImage() {
  mode.value = "image";
  if (imageBlob.value || imageLoading.value) return;
  const token = ++renderToken;
  imageLoading.value = true;
  imageError.value = "";
  try {
    const { renderShareCard } = await import("../../services/share-card.js");
    const blob = await renderShareCard({
      title: props.track.title,
      artist: props.track.artist,
      album: props.track.album,
      covers: [props.track.cover_url ? coverUrl(props.track) : "", props.track.cover],
      link: link.value,
      qrLink: qrLink.value,
      siteName: "MHub",
    });
    if (token !== renderToken) return;
    imageBlob.value = blob;
    imageUrl.value = URL.createObjectURL(blob);
  } catch (error) {
    if (token === renderToken) imageError.value = error?.message || "图片生成失败";
  } finally {
    if (token === renderToken) imageLoading.value = false;
  }
}

function saveImage() {
  if (!imageUrl.value) return;
  const anchor = document.createElement("a");
  anchor.href = imageUrl.value;
  anchor.download = fileName.value;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
}

async function copyImage() {
  try {
    await navigator.clipboard.write([new ClipboardItem({ "image/png": imageBlob.value })]);
    status.toast("图片已复制");
  } catch {
    status.toast("无法复制图片，请保存后分享");
  }
}

async function systemShareImage() {
  try {
    await navigator.share({
      title: props.track.title || "分享歌曲",
      text: `${shareText(props.track)}\n${link.value}`,
      files: [new File([imageBlob.value], fileName.value, { type: "image/png" })],
    });
  } catch (error) {
    if (error?.name !== "AbortError") status.toast("系统分享不可用，请保存图片后分享");
  }
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
  if (imageUrl.value) URL.revokeObjectURL(imageUrl.value);
});
</script>

<template>
  <div class="dialog-backdrop share-backdrop" @click.self="close">
    <section class="dialog share" role="dialog" aria-modal="true" aria-labelledby="share-title">
      <header class="share__head">
        <button v-if="mode" class="mini-button" type="button" title="返回" aria-label="返回" @click="mode = ''"><ChevronLeft :size="18" /></button>
        <span class="share__thumb"><img v-if="thumb" :src="thumb" alt="" /></span>
        <span class="share__song">
          <h2 id="share-title">{{ mode === "image" ? "图片分享" : mode === "link" ? "链接分享" : "分享歌曲" }}</h2>
          <p>{{ shareText(track) }}</p>
        </span>
        <button class="mini-button" type="button" title="关闭" aria-label="关闭" @click="close"><X :size="18" /></button>
      </header>

      <div v-if="!mode" class="share__choices">
        <button type="button" class="share__choice" @click="mode = 'link'">
          <span class="share__choice-icon"><Link2 :size="22" /></span>
          <strong>链接</strong>
          <small>复制链接发给朋友，打开即可收听</small>
        </button>
        <button type="button" class="share__choice" @click="chooseImage">
          <span class="share__choice-icon"><ImageIcon :size="22" /></span>
          <strong>图片</strong>
          <small>生成带封面和二维码的分享图</small>
        </button>
      </div>

      <div v-else-if="mode === 'link'" class="share__body">
        <input ref="linkInput" class="share__link" :value="link" readonly aria-label="分享链接" @focus="$event.target.select()" />
        <div class="share__actions">
          <button class="primary-action" type="button" @click="copyLink"><Copy :size="16" />复制链接</button>
          <button v-if="canSystemShare" class="ghost-action" type="button" @click="systemShareLink"><Share2 :size="16" />更多分享方式</button>
        </div>
      </div>

      <div v-else class="share__body">
        <div class="share__preview" :class="{ 'is-loading': imageLoading }">
          <img v-if="imageUrl" :src="imageUrl" alt="分享图片预览" />
          <span v-else-if="imageError" class="share__status">{{ imageError }}</span>
          <span v-else class="share__status" role="status">正在生成图片…</span>
        </div>
        <div class="share__actions">
          <button class="primary-action" type="button" :disabled="!imageUrl" @click="saveImage"><Download :size="16" />保存图片</button>
          <button v-if="canCopyImage" class="ghost-action" type="button" :disabled="!imageBlob" @click="copyImage"><Copy :size="16" />复制图片</button>
          <button v-if="canShareImage" class="ghost-action" type="button" @click="systemShareImage"><Share2 :size="16" />更多分享方式</button>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.share-backdrop { z-index: 130; }
.share { width: min(440px, 100%); display: grid; gap: 18px; }
.share__head { display: flex; align-items: center; gap: 12px; }
.share__thumb { flex: none; width: 46px; height: 46px; overflow: hidden; border-radius: 8px; background: var(--surface-muted); }
.share__thumb img { width: 100%; height: 100%; object-fit: cover; display: block; }
.share__song { flex: 1; min-width: 0; }
.share__song h2 { margin: 0; font-size: 17px; }
.share__song p { margin: 3px 0 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-muted); font-size: 13px; }
.share__choices { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
.share__choice { display: grid; justify-items: center; gap: 6px; padding: 20px 12px 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--surface-muted); color: var(--text); text-align: center; cursor: pointer; transition: background var(--motion-fast), border-color var(--motion-fast); }
.share__choice:hover, .share__choice:focus-visible { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 8%, var(--surface-muted)); }
.share__choice-icon { display: grid; place-items: center; width: 46px; height: 46px; margin-bottom: 4px; border-radius: 999px; background: var(--accent); color: var(--on-accent); }
.share__choice strong { font-size: 15px; }
.share__choice small { color: var(--text-muted); font-size: 12px; line-height: 1.4; }
.share__body { display: grid; gap: 14px; }
.share__link { width: 100%; min-height: 40px; padding: 0 12px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface-muted); color: var(--text); font: inherit; font-size: 13px; text-overflow: ellipsis; }
.share__actions { display: flex; flex-wrap: wrap; gap: 10px; }
.share__actions > button { flex: 1 1 auto; justify-content: center; }
.share__preview { display: grid; place-items: center; justify-self: center; width: min(100%, 345px, 39vh); aspect-ratio: 3 / 4; overflow: hidden; border-radius: 14px; background: var(--surface-muted); }
.share__preview img { width: 100%; height: 100%; object-fit: contain; display: block; }
.share__status { padding: 0 18px; color: var(--text-muted); font-size: 13px; text-align: center; }
@media (max-width: 520px) {
  .share__choices { grid-template-columns: 1fr; }
  .share__choice { grid-template-columns: auto 1fr; justify-items: start; text-align: left; column-gap: 14px; padding: 14px; }
  .share__choice-icon { grid-row: span 2; margin: 0; }
}
</style>
