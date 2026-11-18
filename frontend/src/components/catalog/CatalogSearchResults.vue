<script setup>
import { computed } from "vue";
import { ChevronRight, Play } from "lucide-vue-next";
import SongList from "../list/SongList.vue";
import CatalogCard from "./CatalogCard.vue";
import { SECTION_LABELS, visibleSections } from "../../services/catalog";

// Search results in Apple Music's tabs: 最佳结果 mixes every kind and
// previews each section; the other tabs list one kind in full. A platform
// whose catalog only has songs keeps the plain song list.
const props = defineProps({
  result: { type: Object, default: null },
  // The result's songs as tracks, in result order.
  songs: { type: Array, default: () => [] },
  loading: { type: Boolean, default: false },
  section: { type: String, default: "top" },
  pending: { type: String, default: "" },
});
const emit = defineEmits(["update:section", "play", "queue", "queue-next", "menu", "open", "play-item", "play-all", "save"]);

const tabs = computed(() => visibleSections(props.result));
const active = computed(() => (tabs.value.includes(props.section) ? props.section : tabs.value[0] || "songs"));
const shelves = computed(() => ["albums", "artists", "playlists"].filter((key) => props.result?.[key]?.length));

function itemKey(item) {
  return `${item.type}:${item.id}`;
}

// A song card in 最佳结果 plays the same track object the song list would.
function trackFor(item) {
  return props.songs.find((track) => String(track.source_id) === String(item.id) && track.platform === item.platform) || null;
}

function playItem(item) {
  if (item.type === "song") {
    const track = trackFor(item);
    if (track) emit("play", track);
    else emit("play-item", item);
  } else {
    emit("play-item", item);
  }
}

function menuFor(item, event) {
  const track = trackFor(item);
  if (track) emit("menu", { event, track });
}

function select(section) {
  emit("update:section", section);
}
</script>

<template>
  <div class="catalog-results">
    <div v-if="tabs.length" class="catalog-tabs" role="tablist" aria-label="搜索结果分类">
      <button
        v-for="tab in tabs"
        :key="tab"
        type="button"
        role="tab"
        class="catalog-tab"
        :class="{ 'is-active': active === tab }"
        :aria-selected="active === tab"
        @click="select(tab)"
      >{{ SECTION_LABELS[tab] }}</button>
    </div>

    <template v-if="tabs.length && active === 'top'">
      <section class="catalog-section">
        <div class="artwork-rail catalog-rail" tabindex="0" aria-label="最佳结果">
          <CatalogCard
            v-for="item in result.top"
            :key="itemKey(item)"
            :item="item"
            with-kind
            :pending="pending === itemKey(item)"
            @open="emit('open', $event)"
            @play="playItem"
            @menu="menuFor(item, $event)"
          />
        </div>
      </section>
      <section v-if="songs.length" class="catalog-section">
        <button class="catalog-section__head" type="button" @click="select('songs')">
          <h2 class="section-title">歌曲</h2><ChevronRight :size="18" aria-hidden="true" />
        </button>
        <SongList
          :items="songs.slice(0, 6)"
          @play="(track) => emit('play', track)"
          @queue="emit('queue', $event)"
          @queue-next="emit('queue-next', $event)"
          @menu="emit('menu', $event)"
        />
      </section>
      <section v-for="key in shelves" :key="key" class="catalog-section">
        <button class="catalog-section__head" type="button" @click="select(key)">
          <h2 class="section-title">{{ SECTION_LABELS[key] }}</h2><ChevronRight :size="18" aria-hidden="true" />
        </button>
        <div class="artwork-rail catalog-rail" tabindex="0" :aria-label="SECTION_LABELS[key]">
          <CatalogCard
            v-for="item in result[key].slice(0, 12)"
            :key="itemKey(item)"
            :item="item"
            :pending="pending === itemKey(item)"
            @open="emit('open', $event)"
            @play="playItem"
          />
        </div>
      </section>
    </template>

    <template v-else-if="!tabs.length || active === 'songs'">
      <div class="search-results__head">
        <p>{{ loading ? "正在搜索…" : `${songs.length} 首` }}</p>
        <div>
          <button class="ghost-action" :disabled="!songs.length" @click="emit('save')">存为歌单</button>
          <button class="primary-action" :disabled="!songs.length" @click="emit('play-all')"><Play :size="15" />播放</button>
        </div>
      </div>
      <SongList
        :items="songs"
        :loading="loading"
        empty-text="没有找到匹配的歌曲"
        @play="(track) => emit('play', track)"
        @queue="emit('queue', $event)"
        @queue-next="emit('queue-next', $event)"
        @menu="emit('menu', $event)"
      />
    </template>

    <div v-else class="catalog-grid" :class="{ 'catalog-grid--artists': active === 'artists' }">
      <CatalogCard
        v-for="item in result[active]"
        :key="itemKey(item)"
        :item="item"
        :pending="pending === itemKey(item)"
        @open="emit('open', $event)"
        @play="playItem"
      />
    </div>
  </div>
</template>

<style scoped>
.catalog-results { display: grid; gap: 30px; min-width: 0; }
/* Apple Music's search tabs: a row of text pills under the search field. */
.catalog-tabs { display: flex; gap: 6px; margin-top: -8px; overflow-x: auto; scrollbar-width: none; }
.catalog-tabs::-webkit-scrollbar { display: none; }
.catalog-tab { flex: 0 0 auto; min-height: 30px; padding: 0 14px; border: 0; border-radius: 999px; background: transparent; color: var(--text-secondary); font-size: 14px; font-weight: 600; cursor: pointer; }
.catalog-tab:hover { background: var(--surface-muted); color: var(--text); }
.catalog-tab.is-active { background: var(--accent); color: #fff; }
.catalog-section { display: grid; gap: 12px; min-width: 0; }
.catalog-section__head { display: inline-flex; align-items: center; gap: 2px; justify-self: start; padding: 0; border: 0; background: transparent; color: var(--text); cursor: pointer; }
.catalog-section__head:hover { color: var(--accent); }
.catalog-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr)); gap: 22px 18px; }
.catalog-grid--artists { grid-template-columns: repeat(auto-fill, minmax(140px, 1fr)); }
@media (max-width: 700px) {
  .catalog-results { gap: 24px; }
  .catalog-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 18px 14px; }
  .catalog-grid--artists { grid-template-columns: repeat(3, minmax(0, 1fr)); }
}
</style>
