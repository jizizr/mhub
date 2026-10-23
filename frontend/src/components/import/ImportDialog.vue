<script setup>
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { Check, ChevronDown, CircleAlert, CircleHelp, Search, X } from "lucide-vue-next";
import { matchCollection, resolveCollection } from "../../apis/music";
import { usePlaylistStore } from "../../stores/playlists";
import { useStatusStore } from "../../stores/status";
import { trackFromSearch } from "../../utils/track";

const props = defineProps({
  platforms: { type: Array, default: () => [] },
});
const emit = defineEmits(["close", "imported"]);

const status = useStatusStore();
const playlists = usePlaylistStore();
const input = ref(null);
const url = ref("");
const loading = ref(false);
const error = ref("");
const collection = ref(null);
const matches = ref(null);
// Imported songs are played from the platform the user is browsing.
const target = ref(status.searchPlatform);
// Index -> candidate id ("" = skip). Unset entries use the automatic choice.
const selections = ref({});
const expanded = ref({});
const filter = ref("all");
const name = ref("");
const mode = ref("create");
const mergeId = ref(playlists.items[0]?.id || "");
let requestVersion = 0;

const platformName = (id) => props.platforms.find((item) => item.id === id)?.label || id;
const sourcePlatform = (value) => /music\.163\.com\//i.test(value) || /163cn\.tv\//i.test(value)
  ? "netease"
  : /music\.apple\.com\//i.test(value)
    ? "applemusic"
    : /y\.qq\.com\//i.test(value)
      ? "tencent"
      : status.searchPlatform;

function chosen(item, index) {
  const selected = selections.value[index];
  if (selected !== undefined) return item.candidates?.find((candidate) => candidate.id === selected) || null;
  return item.status === "matched" ? item.candidates?.[0] || null : null;
}

const rows = computed(() => (matches.value?.items || []).map((item, index) => ({
  item,
  index,
  choice: chosen(item, index),
  decided: selections.value[index] !== undefined,
})));
// Songs that need a decision come first; the import keeps playlist order.
const ORDER = { review: 0, missing: 1, matched: 2 };
const visibleRows = computed(() => rows.value
  .filter((row) => filter.value === "all" || row.item.status === filter.value)
  .sort((left, right) => ORDER[left.item.status] - ORDER[right.item.status] || left.index - right.index));
const counts = computed(() => ({
  all: rows.value.length,
  review: rows.value.filter((row) => row.item.status === "review").length,
  missing: rows.value.filter((row) => row.item.status === "missing").length,
  matched: rows.value.filter((row) => row.item.status === "matched").length,
  pending: rows.value.filter((row) => row.item.status === "review" && !row.decided).length,
}));
const selectedTracks = computed(() => rows.value
  .filter((row) => row.choice)
  .map((row) => trackFromSearch(row.choice, status.quality)));
const canImport = computed(() => !loading.value && !error.value && selectedTracks.value.length > 0 &&
  (mode.value === "create" ? Boolean(name.value.trim()) : Boolean(mergeId.value)));

async function resolve() {
  const value = url.value.trim();
  if (!value || loading.value) return;
  const version = ++requestVersion;
  loading.value = true;
  error.value = "";
  collection.value = null;
  matches.value = null;
  try {
    const source = sourcePlatform(value);
    const resolved = await resolveCollection({ platform: source, url: value });
    if (version !== requestVersion) return;
    if (!resolved.items?.length) throw new Error("歌单没有可导入的歌曲");
    collection.value = resolved;
    name.value = resolved.title || "未命名歌单";
    await match(version);
  } catch (reason) {
    if (version === requestVersion) error.value = reason?.message || "歌单解析失败";
  } finally {
    if (version === requestVersion) loading.value = false;
  }
}

async function match(version = ++requestVersion) {
  if (!collection.value) return;
  loading.value = true;
  error.value = "";
  matches.value = null;
  selections.value = {};
  expanded.value = {};
  try {
    const result = await matchCollection({
      sourcePlatform: collection.value.platform,
      targetPlatform: target.value,
      items: collection.value.items,
    });
    if (version !== requestVersion) return;
    matches.value = result;
    filter.value = result.review_count ? "review" : "all";
  } catch (reason) {
    if (version === requestVersion) error.value = reason?.message || "跨平台匹配失败";
  } finally {
    if (version === requestVersion) loading.value = false;
  }
}

function choose(index, candidateId) {
  selections.value = { ...selections.value, [index]: candidateId };
}

function chooseAllReview(useBest) {
  const next = { ...selections.value };
  for (const row of rows.value) {
    if (row.item.status === "review") next[row.index] = useBest ? row.item.candidates?.[0]?.id || "" : "";
  }
  selections.value = next;
}

function toggle(index) {
  expanded.value = { ...expanded.value, [index]: !expanded.value[index] };
}

function submit() {
  if (!canImport.value) return;
  const tracks = selectedTracks.value;
  const playlist = mode.value === "create"
    ? playlists.create({
      name: name.value,
      tracks,
      platform: matches.value?.target_platform || target.value,
      sourceId: collection.value.id,
      sourceUrl: collection.value.source_url,
    })
    : playlists.merge(mergeId.value, tracks);
  if (!playlist) return;
  status.toast(mode.value === "create" ? `已导入 ${tracks.length} 首到“${playlist.name}”` : `已合并到“${playlist.name}”`);
  emit("imported", playlist.id);
}

function close() {
  if (loading.value && !matches.value) requestVersion += 1;
  emit("close");
}

function keydown(event) {
  if (event.key === "Escape") close();
}

watch(target, (value, previous) => {
  if (value !== previous && collection.value) void match();
});
onMounted(() => {
  document.addEventListener("keydown", keydown);
  void nextTick(() => input.value?.focus());
});
onBeforeUnmount(() => {
  requestVersion += 1;
  document.removeEventListener("keydown", keydown);
});
</script>

<template>
  <div class="dialog-backdrop" @click.self="close">
    <section class="dialog import" :class="{ 'has-result': matches }" role="dialog" aria-modal="true" aria-labelledby="import-title">
      <header class="import__head">
        <div>
          <h2 id="import-title">导入歌单</h2>
          <p>粘贴网易云、Apple Music 或 QQ 音乐歌单链接，匹配到你正在使用的平台。</p>
        </div>
        <button class="mini-button" type="button" title="关闭" aria-label="关闭" @click="close"><X :size="18" /></button>
      </header>

      <form class="search-box import__url" @submit.prevent="resolve">
        <Search :size="18" />
        <input ref="input" v-model="url" placeholder="粘贴歌单链接" aria-label="歌单链接" :disabled="loading" />
        <button type="submit" :disabled="loading || !url.trim()">{{ loading ? "处理中" : "解析" }}</button>
      </form>

      <div v-if="error" class="error-banner import__error">{{ error }}</div>

      <template v-if="collection">
        <div class="import__route">
          <span class="import__source"><strong>{{ collection.title || "未命名歌单" }}</strong><small>{{ platformName(collection.platform) }} · {{ collection.items.length }} 首</small></span>
          <label class="import__target">
            <span>导入到</span>
            <select v-model="target" :disabled="loading" aria-label="目标平台">
              <option v-for="item in platforms" :key="item.id" :value="item.id">{{ item.label }}</option>
            </select>
          </label>
        </div>

        <div v-if="loading && !matches" class="import__loading" role="status">正在 {{ platformName(target) }} 中逐首匹配，歌曲较多时需要一点时间…</div>

        <template v-if="matches">
          <nav class="import__tabs" aria-label="筛选匹配结果">
            <button v-for="tab in [['review', '待确认'], ['missing', '未找到'], ['matched', '已匹配'], ['all', '全部']]"
              :key="tab[0]" type="button" :class="{ 'is-active': filter === tab[0], [`is-${tab[0]}`]: true }"
              :disabled="!counts[tab[0]]" @click="filter = tab[0]">
              {{ tab[1] }} <b>{{ counts[tab[0]] }}</b>
            </button>
          </nav>
          <div v-if="counts.review && filter !== 'matched' && filter !== 'missing'" class="import__bulk">
            <span>{{ counts.pending ? `还有 ${counts.pending} 首待确认，未选择的不会导入` : "待确认歌曲已全部处理" }}</span>
            <button type="button" class="ghost-action" @click="chooseAllReview(true)">全部选用最佳</button>
            <button type="button" class="ghost-action" @click="chooseAllReview(false)">全部跳过</button>
          </div>

          <ol class="import__list">
            <li v-for="row in visibleRows" :key="row.index" class="import-row" :class="`is-${row.item.status}`">
              <div class="import-row__head">
                <span class="import-row__badge" :title="row.item.status === 'matched' ? '自动匹配' : row.item.status === 'review' ? '需要确认' : '目标平台没有可信结果'">
                  <Check v-if="row.item.status === 'matched'" :size="14" />
                  <CircleHelp v-else-if="row.item.status === 'review'" :size="14" />
                  <CircleAlert v-else :size="14" />
                </span>
                <div class="import-row__source">
                  <strong>{{ row.item.source.song }}</strong>
                  <small>{{ row.item.source.singer || "未知艺人" }}</small>
                </div>
                <div v-if="row.choice && !(row.item.status === 'review' || expanded[row.index])" class="import-row__choice">
                  <span><strong>{{ row.choice.song }}</strong><small>{{ row.choice.singer }}</small></span>
                </div>
                <span v-else-if="!row.item.candidates?.length" class="import-row__none">目标平台未找到</span>
                <span v-else-if="row.item.status === 'missing' && !expanded[row.index]" class="import-row__none">没有可信结果</span>
                <button v-if="row.item.candidates?.length && row.item.status !== 'review'" type="button" class="import-row__toggle"
                  :aria-expanded="Boolean(expanded[row.index])" @click="toggle(row.index)">
                  {{ expanded[row.index] ? "收起" : row.item.status === "matched" ? "更换" : "查看结果" }}
                  <ChevronDown :size="14" />
                </button>
              </div>
              <div v-if="row.item.status === 'review' || expanded[row.index]" class="import-row__candidates" role="radiogroup" :aria-label="`${row.item.source.song} 的候选`">
                <label v-for="candidate in row.item.candidates" :key="candidate.id" class="import-candidate" :class="{ 'is-selected': row.choice?.id === candidate.id }">
                  <input type="radio" :name="`import-${row.index}`" :checked="row.choice?.id === candidate.id" @change="choose(row.index, candidate.id)" />
                  <span class="cover-thumb is-small"><img v-if="candidate.cover" :src="candidate.cover" alt="" loading="lazy" /></span>
                  <span class="import-candidate__text"><strong>{{ candidate.song }}</strong><small>{{ candidate.singer || "未知艺人" }}</small></span>
                  <span v-if="candidate.score != null" class="import-candidate__score" :class="{ 'is-high': candidate.score >= 88, 'is-low': candidate.score < 50 }">{{ candidate.score }}%</span>
                </label>
                <label class="import-candidate is-skip" :class="{ 'is-selected': !row.choice }">
                  <input type="radio" :name="`import-${row.index}`" :checked="!row.choice" @change="choose(row.index, '')" />
                  <span class="import-candidate__text"><strong>不导入这首</strong></span>
                </label>
              </div>
            </li>
          </ol>

          <footer class="import__footer">
            <div class="import__mode" role="radiogroup" aria-label="导入方式">
              <label><input v-model="mode" type="radio" value="create" />新建歌单</label>
              <label><input v-model="mode" type="radio" value="merge" :disabled="!playlists.items.length" />合并到已有</label>
            </div>
            <input v-if="mode === 'create'" v-model="name" class="import__field" aria-label="新歌单名称" placeholder="歌单名称" />
            <select v-else v-model="mergeId" class="import__field" aria-label="选择歌单">
              <option v-for="playlist in playlists.items" :key="playlist.id" :value="playlist.id">{{ playlist.name }}</option>
            </select>
            <button type="button" class="primary-action" :disabled="!canImport" @click="submit">导入 {{ selectedTracks.length }} 首</button>
          </footer>
        </template>
      </template>
    </section>
  </div>
</template>

<style scoped>
.import { display: flex; flex-direction: column; width: min(760px, 100%); max-height: min(86vh, 860px); padding: 0; overflow: hidden; }
/* Only the song list scrolls; every other section keeps its height. */
.import > :not(.import__list) { flex-shrink: 0; }
.import__head { display: flex; justify-content: space-between; gap: 16px; padding: 20px 22px 0; }
.import__head h2 { margin: 0; font-size: 20px; }
.import__head p { margin: 6px 0 0; color: var(--text-muted); font-size: 13px; line-height: 1.5; }
.import__url { flex: 0 0 auto; max-width: none; margin: 16px 22px 0; box-shadow: none; }
.import__error { margin: 12px 22px 0; }
.import__route { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin: 16px 22px 0; padding: 12px 14px; border: 1px solid var(--line); border-radius: 14px; background: var(--surface); }
.import__source { display: grid; gap: 3px; min-width: 0; }
.import__source strong { overflow-wrap: anywhere; }
.import__source small { color: var(--text-muted); font-size: 12px; }
.import__target { display: flex; flex: 0 0 auto; align-items: center; gap: 8px; font-size: 13px; font-weight: 650; }
.import__target select, .import__field { height: 38px; padding: 0 10px; border: 1px solid var(--line); border-radius: 10px; color: var(--text); background: var(--input-bg); font: inherit; }
.import__loading { margin: 18px 22px; color: var(--text-muted); font-size: 13px; text-align: center; }
.import__tabs { display: flex; gap: 6px; margin: 14px 22px 0; overflow-x: auto; scrollbar-width: none; }
.import__tabs button { flex: 0 0 auto; display: inline-flex; align-items: center; gap: 6px; height: 32px; padding: 0 12px; border: 1px solid var(--line); border-radius: 999px; background: transparent; color: var(--text-muted); font-size: 13px; cursor: pointer; }
.import__tabs button b { font-weight: 700; color: var(--text); }
.import__tabs button.is-active { border-color: var(--accent-line); background: var(--accent-soft); color: var(--accent); }
.import__tabs button.is-review b { color: var(--accent); }
.import__bulk { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin: 10px 22px 0; color: var(--text-muted); font-size: 12px; }
.import__bulk span { flex: 1 1 200px; }
.import__bulk .ghost-action { min-height: 30px; padding: 0 11px; font-size: 12px; }
.import__list { flex: 1 1 auto; min-height: 120px; margin: 10px 0 0; padding: 0 22px 12px; overflow-y: auto; overscroll-behavior: contain; list-style: none; }
.import-row { padding: 12px 0; border-bottom: 1px solid var(--line); }
.import-row__head { display: grid; grid-template-columns: auto minmax(0, 1fr) minmax(0, 1fr) auto; align-items: center; gap: 10px; }
.import-row__badge { display: grid; place-items: center; width: 24px; height: 24px; border-radius: 50%; color: var(--success); background: color-mix(in srgb, var(--success) 14%, transparent); }
.is-review .import-row__badge { color: var(--accent); background: var(--accent-soft); }
.is-missing .import-row__badge { color: var(--text-muted); background: var(--surface-muted); }
.import-row__source, .import-row__choice span, .import-candidate__text { display: grid; gap: 2px; min-width: 0; }
.import-row__source strong, .import-row__choice strong, .import-candidate__text strong { font-size: 14px; line-height: 1.35; overflow-wrap: anywhere; }
.import-row__source small, .import-row__choice small, .import-candidate__text small { color: var(--text-muted); font-size: 12px; line-height: 1.35; overflow-wrap: anywhere; }
.is-review .import-row__source { grid-column: 2 / -1; }
.import-row__choice { min-width: 0; padding-left: 12px; border-left: 2px solid color-mix(in srgb, var(--success) 50%, transparent); }
.import-row__none { color: var(--text-muted); font-size: 12px; }
.import-row__toggle { display: inline-flex; align-items: center; gap: 3px; padding: 4px 6px; border: 0; border-radius: 8px; background: transparent; color: var(--text-muted); font-size: 12px; cursor: pointer; }
.import-row__toggle:hover { color: var(--accent); background: var(--surface-muted); }
.import-row__toggle[aria-expanded="true"] svg { transform: rotate(180deg); }
.import-row__candidates { display: grid; gap: 6px; margin: 10px 0 0 34px; }
.import-candidate { display: grid; grid-template-columns: auto auto minmax(0, 1fr) auto; align-items: center; gap: 10px; min-height: 52px; padding: 7px 10px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); cursor: pointer; }
.import-candidate.is-skip { grid-template-columns: auto minmax(0, 1fr); min-height: 40px; }
.import-candidate.is-selected { border-color: var(--accent-line); background: var(--accent-soft); }
.import-candidate input { margin: 0; accent-color: var(--accent); }
.import-candidate__score { padding: 2px 7px; border-radius: 999px; background: var(--surface-muted); color: var(--text-muted); font-size: 11px; font-weight: 700; font-variant-numeric: tabular-nums; }
.import-candidate__score.is-high { color: var(--success); background: color-mix(in srgb, var(--success) 14%, transparent); }
.import-candidate__score.is-low { color: var(--danger); }
.import__footer { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; padding: 14px 22px 18px; border-top: 1px solid var(--line); background: var(--surface-raised); }
.import__mode { display: flex; gap: 12px; font-size: 13px; }
.import__mode label { display: inline-flex; align-items: center; gap: 5px; cursor: pointer; }
.import__mode input { accent-color: var(--accent); }
.import__field { flex: 1 1 180px; min-width: 0; }
@media (max-width: 640px) {
  .import { width: 100%; max-height: calc(100dvh - 24px); border-radius: 16px; }
  .import__head, .import__footer { padding-left: 16px; padding-right: 16px; }
  .import__url, .import__route, .import__tabs, .import__bulk, .import__error { margin-left: 16px; margin-right: 16px; }
  .import__list { padding: 0 16px 12px; }
  /* Give the song list the screen once results are in. */
  .has-result .import__head p { display: none; }
  .import__head { padding-top: 16px; }
  .import__url { height: 44px; margin-top: 12px; }
  .import__route { flex-wrap: wrap; gap: 8px; margin-top: 10px; padding: 10px 12px; }
  .import__source { flex: 1 1 100%; }
  .import__source strong { font-size: 14px; }
  .import__target { flex: 1 1 100%; }
  .import__target select { flex: 1; height: 34px; }
  .import__tabs { margin-top: 10px; }
  .import__bulk { margin-top: 8px; }
  /* Source over choice: each name gets the full width to read and compare. */
  .import-row__head { grid-template-columns: auto minmax(0, 1fr) auto; }
  .import-row__choice, .import-row__none { grid-column: 2 / -1; padding-left: 10px; }
  .import-row__toggle { grid-column: 3; grid-row: 1; }
  .import-row__candidates { margin-left: 0; }
  .import__footer .primary-action { flex: 1 1 100%; justify-content: center; }
}
</style>
