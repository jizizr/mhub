<script setup>
import { computed } from "vue";
import { Disc3, ListMusic, LoaderCircle, Music2, Play, UserRound } from "lucide-vue-next";
import { cardSubtitle, KIND_LABELS } from "../../services/catalog";

// An Apple Music lockup for any catalog item: the card opens its page (a song
// plays instead), the round button plays an album or playlist in place.
const props = defineProps({
  item: { type: Object, required: true },
  // Best results name each card's kind, as Apple Music's top hits do.
  withKind: { type: Boolean, default: false },
  pending: { type: Boolean, default: false },
});
const emit = defineEmits(["open", "play", "menu"]);

const isArtist = computed(() => props.item.type === "artist");
const isSong = computed(() => props.item.type === "song");
const playable = computed(() => ["album", "playlist", "song"].includes(props.item.type));
const subtitle = computed(() => cardSubtitle(props.item, { withKind: props.withKind }) || (isArtist.value ? "艺人" : ""));
const fallbackIcon = computed(() => ({ artist: UserRound, album: Disc3, playlist: ListMusic })[props.item.type] || Music2);
const label = computed(() => `${isSong.value ? "播放" : "查看"}${KIND_LABELS[props.item.type] || ""}：${props.item.title}`);

// Songs share the song menu (right click); other cards have none.
function openMenu(event) {
  if (!isSong.value) return;
  event.preventDefault();
  emit("menu", event);
}

function activate() {
  if (isSong.value) emit("play", props.item);
  else emit("open", props.item);
}
</script>

<template>
  <article class="artwork-card catalog-card" :class="{ 'catalog-card--artist': isArtist }" @contextmenu="openMenu">
    <div class="artwork-card__art">
      <button class="artwork-card__cover" type="button" :aria-label="label" @click="activate">
        <img v-if="item.cover" :src="item.cover" alt="" loading="lazy" />
        <component :is="fallbackIcon" v-else :size="28" :stroke-width="1.5" aria-hidden="true" />
      </button>
      <button
        v-if="playable"
        class="artwork-card__play"
        :class="{ 'is-pending': pending }"
        type="button"
        title="播放"
        :aria-label="`播放${KIND_LABELS[item.type] || ''}：${item.title}`"
        @click="emit('play', item)"
      >
        <LoaderCircle v-if="pending" class="is-spinning" :size="16" />
        <Play v-else :size="15" fill="currentColor" />
      </button>
    </div>
    <button class="artwork-card__text" type="button" tabindex="-1" aria-hidden="true" @click="activate">
      <strong>{{ item.title }}</strong>
      <small v-if="subtitle">{{ subtitle }}</small>
    </button>
  </article>
</template>

<style scoped>
/* Artists are round with centred names, as in Apple Music. */
.catalog-card--artist .artwork-card__cover { border-radius: 50%; }
.catalog-card--artist .artwork-card__text { justify-items: center; text-align: center; }
</style>
