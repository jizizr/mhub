<script setup>
import { computed } from "vue";
import { ChevronLeft, ChevronRight, Play, RefreshCcw, Shuffle, UserRound } from "lucide-vue-next";
import SongList from "../list/SongList.vue";
import CatalogCard from "./CatalogCard.vue";

// An artist as Apple Music shows one: photo and name, a play button for the
// top songs, then the top songs, albums, and singles & EPs.
const props = defineProps({
  page: { type: Object, required: true },
  backLabel: { type: String, default: "返回" },
  shuffle: { type: Boolean, default: false },
  pending: { type: String, default: "" },
});
const emit = defineEmits(["back", "retry", "play", "play-all", "queue", "queue-next", "menu", "open", "play-item"]);

const artist = computed(() => props.page.data || null);
const name = computed(() => artist.value?.name || props.page.title || "艺人");
const cover = computed(() => artist.value?.cover || props.page.cover || "");
const songs = computed(() => artist.value?.top_songs || []);
const shelves = computed(() => [
  { key: "albums", title: "专辑", items: artist.value?.albums || [] },
  { key: "singles", title: "单曲与 EP", items: artist.value?.singles || [] },
].filter((shelf) => shelf.items.length));
</script>

<template>
  <div class="artist-page" :aria-busy="page.loading">
    <nav class="artist-page__nav" aria-label="艺人导航">
      <button type="button" class="page-back" @click="emit('back')"><ChevronLeft :size="20" aria-hidden="true" />{{ backLabel }}</button>
    </nav>
    <header class="artist-page__header">
      <div class="artist-page__photo" :class="{ 'is-empty': !cover }">
        <img v-if="cover" :key="cover" :src="cover" alt="" decoding="async" />
        <UserRound v-else :size="56" :stroke-width="1.3" aria-hidden="true" />
      </div>
      <div class="artist-page__heading">
        <h1>{{ name }}</h1>
        <button type="button" class="primary-action artist-page__play" :disabled="page.loading || !songs.length" @click="emit('play-all')">
          <Shuffle v-if="shuffle" :size="17" aria-hidden="true" /><Play v-else :size="17" aria-hidden="true" />
          {{ shuffle ? "随机播放" : "播放" }}
        </button>
      </div>
    </header>

    <div v-if="page.loading" class="list-state" role="status">正在读取艺人…</div>
    <div v-else-if="page.error" class="artist-page__state" role="alert">
      <strong>暂时无法打开这个艺人</strong>
      <p>{{ page.error }}</p>
      <button type="button" class="ghost-action" @click="emit('retry')"><RefreshCcw :size="15" aria-hidden="true" />重新加载</button>
    </div>
    <template v-else>
      <section v-if="songs.length" class="artist-page__section">
        <h2 class="section-title">热门歌曲</h2>
        <SongList
          :items="songs.slice(0, 10)"
          @play="(track, index) => emit('play', track, index)"
          @queue="emit('queue', $event)"
          @queue-next="emit('queue-next', $event)"
          @menu="emit('menu', $event)"
        />
      </section>
      <section v-for="shelf in shelves" :key="shelf.key" class="artist-page__section">
        <h2 class="section-title">{{ shelf.title }}</h2>
        <div class="artwork-rail" tabindex="0" :aria-label="shelf.title">
          <CatalogCard
            v-for="item in shelf.items"
            :key="`${item.type}:${item.id}`"
            :item="item"
            :pending="pending === `${item.type}:${item.id}`"
            @open="emit('open', $event)"
            @play="emit('play-item', $event)"
          />
        </div>
      </section>
      <p v-if="!songs.length && !shelves.length" class="list-state">这位艺人暂时没有可播放的作品</p>
    </template>
  </div>
</template>

<style scoped>
.artist-page { display: grid; gap: 28px; min-width: 0; }
.artist-page__nav { margin-bottom: -18px; }
.artist-page__header { display: grid; grid-template-columns: clamp(150px, 18vw, 220px) minmax(0, 1fr); align-items: end; gap: 30px; }
.artist-page__photo { aspect-ratio: 1; overflow: hidden; border-radius: 50%; background: var(--placeholder); box-shadow: 0 0 0 .5px var(--am-divider), 0 6px 20px rgba(0, 0, 0, .14); }
.artist-page__photo img { display: block; width: 100%; height: 100%; object-fit: cover; }
.artist-page__photo.is-empty { display: grid; place-items: center; color: var(--text-muted); }
.artist-page__heading { display: grid; justify-items: start; gap: 16px; min-width: 0; }
.artist-page__heading h1 { margin: 0; color: var(--am-primary); font-size: clamp(30px, 3.4vw, 44px); font-weight: 800; line-height: 1.1; overflow-wrap: anywhere; }
.artist-page__play { min-width: 130px; }
.artist-page__section { display: grid; gap: 12px; min-width: 0; }
.artist-page__state { display: grid; justify-items: center; gap: 10px; padding: 40px 20px; color: var(--text-muted); text-align: center; }
.artist-page__state strong { color: var(--text); }
.artist-page__state p { margin: 0; font-size: 13px; }
@media (max-width: 700px) {
  .artist-page__nav { margin-bottom: -12px; }
  .artist-page__header { grid-template-columns: 1fr; justify-items: center; gap: 16px; text-align: center; }
  .artist-page__photo { width: min(52vw, 210px); }
  .artist-page__heading { justify-items: center; }
  .artist-page__heading h1 { font-size: 26px; }
}
</style>
