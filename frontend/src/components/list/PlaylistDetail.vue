<script setup>
import { computed } from "vue";
import { ArrowLeft, ListMusic, ListPlus, ListStart, LoaderCircle, Music2, Play, RefreshCcw, Shuffle, Trash2 } from "lucide-vue-next";
import SongList from "./SongList.vue";

const props = defineProps({
  playlist: { type: Object, required: true },
  loading: { type: Boolean, default: false },
  error: { type: String, default: "" },
  quality: { type: String, default: "high" },
  shuffle: { type: Boolean, default: false },
  local: { type: Boolean, default: false },
  queueLength: { type: Number, default: 0 },
  busy: { type: Boolean, default: false },
  notice: { type: String, default: "" },
  currentTrack: { type: Object, default: null },
  showQueue: { type: Boolean, default: false },
});

const emit = defineEmits(["back", "replace", "append", "next", "retry", "play", "queue", "queue-next", "open", "menu", "delete", "show-queue"]);
const tracks = computed(() => props.playlist?.tracks || []);
const title = computed(() => props.playlist?.name || (props.playlist?.resource_type === "albums" ? "推荐专辑" : "推荐歌单"));
const cover = computed(() => props.playlist?.cover || props.playlist?.cover_url || tracks.value.find((track) => track.cover)?.cover || "");
// A detail page never needs an account name or personal curator metadata.
// Use only the public platform label, including when the API includes more fields.
const source = computed(() => {
  if (!props.local) return "Apple Music";
  return ({ applemusic: "Apple Music", netease: "网易云音乐", tencent: "QQ 音乐", soda: "汽水音乐" })[props.playlist?.platform] || "本地歌单";
});
const qualityLabel = computed(() => ({ standard: "标准", high: "高品质", lossless: "Lossless", hires: "Hi-Res" })[props.quality] || "高品质");
const unavailable = computed(() => props.loading || props.busy || !!props.error || !tracks.value.length);
const replaceHint = computed(() => {
  const start = props.shuffle ? "随机选一首开始" : "从第一首开始";
  return props.queueLength ? `替换现有 ${props.queueLength} 首，${start}` : `以此歌单建立队列，${start}`;
});
const nextHint = computed(() => props.currentTrack ? "接在当前歌曲之后，不打断正在播放" : "放到队列最前，原有歌曲顺延");
</script>

<template>
  <div class="playlist-detail" :aria-busy="loading || busy">
    <nav class="playlist-detail__nav" aria-label="歌单导航">
      <button type="button" class="ghost-action playlist-detail__back" @click="emit('back')">
        <ArrowLeft :size="16" aria-hidden="true" />{{ local ? "返回歌单" : "返回推荐" }}
      </button>
      <div class="playlist-detail__tools">
        <button v-if="showQueue || queueLength" type="button" class="ghost-action" @click="emit('show-queue')">
          <ListMusic :size="16" aria-hidden="true" />查看队列
        </button>
        <button v-if="local" type="button" class="ghost-action playlist-detail__delete" @click="emit('delete')">
          <Trash2 :size="15" aria-hidden="true" />删除歌单
        </button>
      </div>
    </nav>

    <header class="playlist-detail__header">
      <div class="playlist-detail__cover" :class="{ 'playlist-detail__cover--empty': !cover }">
        <img v-if="cover" :key="cover" :src="cover" alt="" decoding="async" />
        <Music2 v-else :size="46" :stroke-width="1.4" aria-hidden="true" />
      </div>
      <div class="playlist-detail__heading">
        <p class="playlist-detail__source">{{ source }} · {{ playlist.resource_type === "albums" ? "专辑" : "歌单" }}</p>
        <h1>{{ title }}</h1>
        <p class="playlist-detail__meta">
          <span>{{ loading ? "正在读取曲目" : error ? "曲目暂未加载" : `${tracks.length} 首音乐` }}</span>
          <span class="playlist-detail__quality">{{ qualityLabel }}</span>
          <span v-if="shuffle" class="playlist-detail__shuffle"><Shuffle :size="13" aria-hidden="true" />随机播放</span>
        </p>
      </div>
    </header>

    <div class="playlist-detail__actions" role="group" aria-label="歌单播放方式">
      <button
        type="button"
        class="playlist-detail__action playlist-detail__action--primary"
        :disabled="unavailable"
        :title="replaceHint"
        @click="emit('replace')"
      >
        <Shuffle v-if="shuffle" :size="20" aria-hidden="true" />
        <Play v-else :size="20" aria-hidden="true" />
        <span><strong><span class="playlist-detail__label-full">替换队列并播放</span><span class="playlist-detail__label-short">{{ shuffle ? "随机播放" : "播放" }}</span></strong><small>{{ replaceHint }}</small></span>
      </button>
      <button type="button" class="playlist-detail__action" :disabled="unavailable" title="追加到队列末尾，保留当前播放" @click="emit('append')">
        <ListPlus :size="20" aria-hidden="true" />
        <span><strong><span class="playlist-detail__label-full">加入播放队列</span><span class="playlist-detail__label-short">加入队列</span></strong><small>追加到队列末尾，保留当前播放</small></span>
      </button>
      <button type="button" class="playlist-detail__action" :disabled="unavailable" :title="nextHint" @click="emit('next')">
        <ListStart :size="20" aria-hidden="true" />
        <span><strong><span class="playlist-detail__label-full">下一首播放</span><span class="playlist-detail__label-short">下一首</span></strong><small>{{ nextHint }}</small></span>
      </button>
    </div>

    <p class="playlist-detail__feedback" role="status" aria-live="polite" aria-atomic="true">
      <LoaderCircle v-if="busy" class="playlist-detail__spinner" :size="14" aria-hidden="true" />
      <span>{{ busy ? "正在准备播放…" : notice }}</span>
    </p>

    <Transition name="playlist-detail-state" mode="out-in">
      <div v-if="loading" key="loading" class="playlist-detail__loading" role="status" aria-label="正在加载歌单曲目">
        <div v-for="row in 6" :key="row" class="playlist-detail__skeleton" aria-hidden="true">
          <span class="playlist-detail__skeleton-cover"></span>
          <span class="playlist-detail__skeleton-lines"><i></i><i></i></span>
        </div>
      </div>
      <div v-else-if="error" key="error" class="playlist-detail__state" role="alert">
        <Music2 :size="30" :stroke-width="1.5" aria-hidden="true" />
        <strong>暂时无法打开这份歌单</strong>
        <p>{{ error }}</p>
        <button type="button" class="ghost-action" @click="emit('retry')"><RefreshCcw :size="15" aria-hidden="true" />重新加载</button>
      </div>
      <div v-else-if="!tracks.length" key="empty" class="playlist-detail__state" role="status">
        <Music2 :size="30" :stroke-width="1.5" aria-hidden="true" />
        <strong>歌单里暂时没有歌曲</strong>
        <p>{{ local ? "添加歌曲后，就可以在这里浏览和播放。" : "可以返回推荐看看其他歌单，或重新加载。" }}</p>
        <button v-if="!local" type="button" class="ghost-action" @click="emit('retry')"><RefreshCcw :size="15" aria-hidden="true" />重新加载</button>
      </div>
      <SongList
        v-else
        key="tracks"
        :items="tracks"
        @play="emit('play', $event)"
        @queue="emit('queue', $event)"
        @queue-next="emit('queue-next', $event)"
        @open="emit('open', $event)"
        @menu="emit('menu', $event)"
      />
    </Transition>
  </div>
</template>

<style scoped>
.playlist-detail { min-width: 0; }
.playlist-detail__nav, .playlist-detail__tools { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; }
.playlist-detail__nav { justify-content: space-between; margin-bottom: 26px; }
.playlist-detail__back { padding-left: 11px; }
.playlist-detail__delete { color: var(--text-muted); }
.playlist-detail__delete:hover { color: var(--accent); }
.playlist-detail__header { display: grid; grid-template-columns: 152px minmax(0, 1fr); align-items: center; gap: 26px; }
.playlist-detail__cover { aspect-ratio: 1; overflow: hidden; border-radius: 20px; background: var(--placeholder); box-shadow: var(--shadow-md); }
.playlist-detail__cover img { display: block; width: 100%; height: 100%; object-fit: cover; }
.playlist-detail__cover--empty { display: grid; place-items: center; color: var(--text-muted); background: linear-gradient(135deg, var(--accent-soft), var(--surface)); }
.playlist-detail__heading { min-width: 0; }
.playlist-detail__source { margin: 0 0 10px; font-size: 13px; font-weight: 700; color: var(--text-muted); }
.playlist-detail__heading h1 { margin: 0; font-size: clamp(28px, 4vw, 44px); line-height: 1.15; letter-spacing: -.025em; overflow-wrap: anywhere; }
.playlist-detail__meta { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 12px; margin: 14px 0 0; color: var(--text-muted); font-size: 12px; }
.playlist-detail__quality { padding: 3px 7px; border: 1px solid var(--line); border-radius: 6px; font-size: 11px; font-weight: 720; }
.playlist-detail__shuffle { display: inline-flex; align-items: center; gap: 4px; }
.playlist-detail__actions { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; margin: 26px 0 24px; }
.playlist-detail__action { display: flex; align-items: center; gap: 12px; min-height: 80px; min-width: 0; padding: 16px; border: 1px solid var(--line); border-radius: 15px; background: var(--surface); text-align: left; cursor: pointer; }
.playlist-detail__action > svg { flex: 0 0 auto; }
.playlist-detail__action > span { min-width: 0; }
.playlist-detail__action strong, .playlist-detail__action small { display: block; overflow-wrap: anywhere; }
.playlist-detail__action strong { font-size: 14px; line-height: 1.4; }
.playlist-detail__action small { margin-top: 5px; color: var(--text-muted); font-size: 11px; line-height: 1.5; }
.playlist-detail__action:hover:not(:disabled) { background: var(--surface-strong); border-color: var(--accent-line); transform: translateY(-1px); }
.playlist-detail__action--primary { color: var(--on-accent); background: var(--accent); border-color: transparent; }
.playlist-detail__action--primary small { color: rgba(255, 255, 255, .88); }
.playlist-detail__action--primary:hover:not(:disabled) { background: var(--accent-hover); border-color: transparent; box-shadow: 0 5px 14px var(--accent-soft); }
.playlist-detail__label-short { display: none; }
.playlist-detail__feedback { display: flex; align-items: center; gap: 7px; min-height: 24px; margin: -14px 0 12px; color: var(--text-muted); font-size: 12px; line-height: 1.5; overflow-wrap: anywhere; }
.playlist-detail__spinner { flex: 0 0 auto; animation: playlist-detail-spin .9s linear infinite; }
.playlist-detail__loading { padding: 12px 0; }
.playlist-detail__skeleton { display: flex; align-items: center; gap: 14px; padding: 13px 12px; }
.playlist-detail__skeleton-cover { flex: 0 0 44px; height: 44px; border-radius: 10px; }
.playlist-detail__skeleton-lines { display: grid; gap: 8px; width: min(230px, 60%); }
.playlist-detail__skeleton-lines i { height: 12px; border-radius: 4px; }
.playlist-detail__skeleton-lines i:last-child { width: 60%; height: 9px; }
.playlist-detail__skeleton-cover, .playlist-detail__skeleton-lines i { background: var(--surface-muted); animation: playlist-detail-pulse 1.7s ease-in-out infinite alternate; }
.playlist-detail__skeleton:nth-child(2n) .playlist-detail__skeleton-cover, .playlist-detail__skeleton:nth-child(2n) .playlist-detail__skeleton-lines i { animation-delay: .12s; }
.playlist-detail__state { display: flex; flex-direction: column; align-items: center; gap: 12px; padding: 48px 20px; text-align: center; color: var(--text-muted); }
.playlist-detail__state strong { color: var(--text); }
.playlist-detail__state p { max-width: 480px; margin: 0 0 5px; font-size: 13px; line-height: 1.6; overflow-wrap: anywhere; }
.playlist-detail-state-enter-active, .playlist-detail-state-leave-active { transition: opacity 180ms var(--ease-standard), transform 220ms var(--ease-standard); }
.playlist-detail-state-enter-from { opacity: 0; transform: translateY(6px); }
.playlist-detail-state-leave-to { opacity: 0; transform: translateY(-3px); }
@keyframes playlist-detail-pulse { from { opacity: .45; } to { opacity: 1; } }
@keyframes playlist-detail-spin { to { transform: rotate(360deg); } }
@media (max-width: 900px) {
  .playlist-detail__actions { gap: 8px; }
  .playlist-detail__action { align-items: flex-start; gap: 9px; padding: 13px 11px; }
}
@media (max-width: 700px) {
  .playlist-detail__nav { margin-bottom: 22px; }
  .playlist-detail__header { grid-template-columns: 100px minmax(0, 1fr); gap: 17px; }
  .playlist-detail__cover { border-radius: 15px; }
  .playlist-detail__heading h1 { font-size: clamp(25px, 5.6vw, 34px); }
  .playlist-detail__source { margin-bottom: 7px; font-size: 11px; }
  .playlist-detail__meta { margin-top: 9px; gap: 6px 8px; }
  /* One compact row keeps the track list above the fold on phones. */
  .playlist-detail__actions { grid-template-columns: 1.3fr 1fr 1fr; gap: 8px; margin: 20px 0 14px; }
  .playlist-detail__action { justify-content: center; align-items: center; min-height: 46px; padding: 0 10px; gap: 6px; border-radius: 999px; }
  .playlist-detail__action > svg { width: 17px; height: 17px; }
  .playlist-detail__action small, .playlist-detail__label-full { display: none; }
  .playlist-detail__label-short { display: inline; white-space: nowrap; }
  .playlist-detail__action strong { font-size: 13px; }
  .playlist-detail__feedback { min-height: 18px; margin: -6px 0 6px; }
  .playlist-detail__tools .ghost-action, .playlist-detail__back { min-height: 40px; padding: 0 10px; font-size: 12px; }
}
@media (max-width: 360px) {
  .playlist-detail__header { grid-template-columns: 80px minmax(0, 1fr); gap: 12px; }
  .playlist-detail__tools { gap: 5px; }
}
@media (prefers-reduced-motion: reduce) {
  .playlist-detail__action, .playlist-detail-state-enter-active, .playlist-detail-state-leave-active { transition: none; }
  .playlist-detail__skeleton-cover, .playlist-detail__skeleton-lines i, .playlist-detail__spinner { animation: none; }
}
</style>
