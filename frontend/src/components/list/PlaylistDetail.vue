<script setup>
import { computed } from "vue";
import { ChevronLeft, ListPlus, ListStart, LoaderCircle, Music2, Play, RefreshCcw, RefreshCw, Shuffle, Trash2 } from "lucide-vue-next";
import { canSyncPlaylist } from "../../services/playlist-sync";
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
  syncing: { type: Boolean, default: false },
});

const emit = defineEmits(["back", "replace", "append", "next", "retry", "play", "queue", "queue-next", "menu", "delete", "sync"]);
const syncable = computed(() => props.local && canSyncPlaylist(props.playlist));
const syncedLabel = computed(() => {
  const at = props.playlist?.synced_at;
  if (!syncable.value || !at) return "";
  const minutes = Math.floor((Date.now() - at) / 60000);
  if (minutes < 1) return "刚刚同步";
  if (minutes < 60) return `${minutes} 分钟前同步`;
  if (minutes < 1440) return `${Math.floor(minutes / 60)} 小时前同步`;
  return `${Math.floor(minutes / 1440)} 天前同步`;
});
const tracks = computed(() => props.playlist?.tracks || []);
const title = computed(() => props.playlist?.name || (props.playlist?.resource_type === "albums" ? "推荐专辑" : "推荐歌单"));
const cover = computed(() => props.playlist?.cover || props.playlist?.cover_url || tracks.value.find((track) => track.cover)?.cover || "");
// A detail page never needs an account name or personal curator metadata.
// Use only the public platform label, including when the API includes more fields.
const source = computed(() => {
  if (!props.local) return "Apple Music";
  return ({ applemusic: "Apple Music", netease: "网易云音乐", tencent: "QQ 音乐", soda: "汽水音乐" })[props.playlist?.platform] || "本地歌单";
});
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
      <button type="button" class="page-back" @click="emit('back')">
        <ChevronLeft :size="20" aria-hidden="true" />{{ local ? "资料库" : "主页" }}
      </button>
      <div class="playlist-detail__tools">
        <button v-if="syncable" type="button" class="ghost-action playlist-detail__sync" :disabled="syncing" title="与原歌单同步：加入新增的歌，移除原歌单已删的歌" @click="emit('sync')">
          <RefreshCw :size="15" :class="{ 'is-spinning': syncing }" aria-hidden="true" />{{ syncing ? "同步中…" : "同步" }}
        </button>
        <button v-if="local" type="button" class="mini-button playlist-detail__delete" title="删除歌单" aria-label="删除歌单" @click="emit('delete')">
          <Trash2 :size="17" aria-hidden="true" />
        </button>
      </div>
    </nav>

    <header class="playlist-detail__header">
      <div class="playlist-detail__cover" :class="{ 'playlist-detail__cover--empty': !cover }">
        <img v-if="cover" :key="cover" :src="cover" alt="" decoding="async" />
        <Music2 v-else :size="46" :stroke-width="1.4" aria-hidden="true" />
      </div>
      <div class="playlist-detail__heading">
        <h1>{{ title }}</h1>
        <p class="playlist-detail__source">{{ source }}</p>
        <p class="playlist-detail__meta">
          {{ loading ? "正在读取" : error ? "未加载" : `${tracks.length} 首歌曲` }}<template v-if="syncedLabel"> · {{ syncedLabel }}</template>
        </p>
        <div class="playlist-detail__actions" role="group" aria-label="歌单播放方式">
          <button type="button" class="playlist-detail__action playlist-detail__action--primary" :disabled="unavailable" :title="replaceHint" @click="emit('replace')">
            <Shuffle v-if="shuffle" :size="17" aria-hidden="true" />
            <Play v-else :size="17" aria-hidden="true" />
            {{ shuffle ? "随机播放" : "播放" }}
          </button>
          <button type="button" class="playlist-detail__action" :disabled="unavailable" :title="nextHint" @click="emit('next')">
            <ListStart :size="17" aria-hidden="true" />下一首
          </button>
          <button type="button" class="playlist-detail__action" :disabled="unavailable" title="追加到队列末尾，保留当前播放" @click="emit('append')">
            <ListPlus :size="17" aria-hidden="true" />加入队列
          </button>
        </div>
      </div>
    </header>

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
                <button v-if="!local" type="button" class="ghost-action" @click="emit('retry')"><RefreshCcw :size="15" aria-hidden="true" />重新加载</button>
      </div>
      <SongList
        v-else
        key="tracks"
        :items="tracks"
        @play="emit('play', $event)"
        @queue="emit('queue', $event)"
        @queue-next="emit('queue-next', $event)"
        @menu="emit('menu', $event)"
      />
    </Transition>
  </div>
</template>

<style scoped>
.playlist-detail { min-width: 0; }
.playlist-detail__nav { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-bottom: 18px; }
.playlist-detail__tools { display: flex; align-items: center; gap: 6px; }
.playlist-detail__sync { min-height: 34px; padding: 0 12px; border-radius: 999px; font-size: 13px; }
.playlist-detail__delete { color: var(--text-muted); }
.playlist-detail__delete:hover { color: var(--danger); }
/* Header after Apple Music: large artwork, bold title, the source in the key
   colour, a small meta line, then pill buttons. */
.playlist-detail__header { display: grid; grid-template-columns: clamp(200px, 22vw, 270px) minmax(0, 1fr); align-items: end; gap: 34px; margin-bottom: 40px; }
.playlist-detail__cover { aspect-ratio: 1; overflow: hidden; border-radius: 8px; background: var(--placeholder); box-shadow: 0 0 0 .5px var(--am-divider), 0 4px 14px rgba(0, 0, 0, .1); }
.playlist-detail__cover img { display: block; width: 100%; height: 100%; object-fit: cover; }
.playlist-detail__cover--empty { display: grid; place-items: center; color: var(--text-muted); background: linear-gradient(135deg, var(--accent-soft), var(--surface)); }
.playlist-detail__heading { min-width: 0; }
/* Apple Music: 26px bold title over a 26px regular key-colour subtitle, an
   11px semibold meta line, then 36px key-colour pills. */
.playlist-detail__heading h1 { margin: 0; color: var(--am-primary); font-size: 26px; font-weight: 700; line-height: 30px; letter-spacing: 0; overflow-wrap: anywhere; }
.playlist-detail__source { margin: 0; color: var(--accent); font-size: 26px; line-height: 30px; }
.playlist-detail__meta { margin: 6px 0 0; color: var(--am-secondary); font-size: 11px; font-weight: 600; line-height: 14px; text-transform: uppercase; }
.playlist-detail__actions { display: flex; flex-wrap: wrap; gap: 12px; margin-top: 16px; }
.playlist-detail__action { display: inline-flex; align-items: center; justify-content: center; gap: 6px; min-width: 130px; min-height: 36px; padding: 0 12px; border: 0; border-radius: 24px; background: var(--accent); color: #fff; font-size: 15px; font-weight: 600; line-height: 20px; white-space: nowrap; cursor: pointer; }
.playlist-detail__action:hover:not(:disabled) { background: var(--accent-hover); }
.playlist-detail__action--primary { color: #fff; background: var(--accent); }
.playlist-detail__action--primary:hover:not(:disabled) { background: var(--accent-hover); }
.playlist-detail__feedback { display: flex; align-items: center; gap: 7px; min-height: 20px; margin: -8px 0 8px; color: var(--text-muted); font-size: 13px; line-height: 1.5; overflow-wrap: anywhere; }
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
/* Phones: centred artwork and title, then one row of actions, as in Apple Music. */
@media (max-width: 700px) {
  .playlist-detail__nav { margin-bottom: 8px; }
  .playlist-detail__header { grid-template-columns: 1fr; justify-items: center; gap: 16px; text-align: center; }
  .playlist-detail__cover { width: min(62vw, 260px); }
  .playlist-detail__heading { width: 100%; }
  .playlist-detail__heading h1 { font-size: 22px; }
  .playlist-detail__source { font-size: 20px; }
  .playlist-detail__meta { margin-top: 4px; }
  .playlist-detail__actions { display: grid; grid-template-columns: 1.2fr 1fr 1fr; gap: 8px; margin-top: 16px; }
  .playlist-detail__action { min-width: 0; min-height: 42px; padding: 0 10px; font-size: 14px; }
}
@media (prefers-reduced-motion: reduce) {
  .playlist-detail__action, .playlist-detail-state-enter-active, .playlist-detail-state-leave-active { transition: none; }
  .playlist-detail__skeleton-cover, .playlist-detail__skeleton-lines i, .playlist-detail__spinner { animation: none; }
}
</style>
