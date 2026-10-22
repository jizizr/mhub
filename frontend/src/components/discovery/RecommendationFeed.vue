<script setup>
import { computed } from "vue";
import { Ban, ChevronLeft, ChevronRight, Heart, ListPlus, Play, RefreshCcw, Shuffle, Sparkles } from "lucide-vue-next";
import { trackIdentity } from "../../stores/queue";

const emit = defineEmits(["play", "queue", "open", "refresh", "mood", "play-all", "queue-all", "playlist", "like", "less"]);

const moods = [
  { id: "relax", label: "放松一下", query: "chill" },
  { id: "focus", label: "专注工作", query: "focus" },
  { id: "commute", label: "通勤路上", query: "night drive" },
  { id: "discover", label: "探索新声", query: "indie pop" },
  { id: "chinese", label: "华语流行", query: "华语流行" },
  { id: "jazz", label: "爵士时光", query: "jazz" },
];

const props = defineProps({
  tracks: { type: Array, default: () => [] },
  recentTracks: { type: Array, default: () => [] },
  playlists: { type: Array, default: () => [] },
  recommendationPlaylists: { type: Array, default: () => [] },
  recommendationTags: { type: Array, default: () => [] },
  loading: { type: Boolean, default: false },
  error: { type: String, default: "" },
  platform: { type: String, default: "applemusic" },
  platformLabel: { type: String, default: "当前平台" },
  mood: { type: String, default: "" },
  personalized: { type: Boolean, default: false },
});

// Keep horizontal rails discoverable and usable without relying on the OS
// overlay scrollbar. A wheel over a rail maps vertical wheel input to the
// horizontal axis. Only mouse dragging is handled here: touch/pen scrolling
// stays native so vertical page scrolling, momentum and pinch zoom can coexist.
const railDragState = new WeakMap();

function beginRailDrag(event) {
  const rail = event.currentTarget;
  if (!rail || event.pointerType !== "mouse" || event.button !== 0) return;
  delete rail.dataset.suppressClick;
  railDragState.set(rail, {
    pointerId: event.pointerId,
    startX: event.clientX,
    startY: event.clientY,
    startScroll: rail.scrollLeft,
    moved: false,
  });
}

function moveRailDrag(event) {
  const rail = event.currentTarget;
  const state = railDragState.get(rail);
  if (!state || state.pointerId !== event.pointerId) return;
  const delta = event.clientX - state.startX;
  const verticalDelta = event.clientY - state.startY;
  if (!state.moved && Math.abs(verticalDelta) > 4 && Math.abs(verticalDelta) > Math.abs(delta)) {
    railDragState.delete(rail);
    return;
  }
  if (Math.abs(delta) > 4 && !state.moved) {
    state.moved = true;
    // Keep ordinary button clicks on the card. Only take pointer capture
    // after the gesture has clearly become a horizontal drag.
    rail.classList.add("is-dragging");
    rail.setPointerCapture?.(event.pointerId);
  }
  if (!state.moved) return;
  event.preventDefault();
  rail.scrollLeft = state.startScroll - delta;
}

function endRailDrag(event) {
  const rail = event.currentTarget;
  const state = railDragState.get(rail);
  if (!state || state.pointerId !== event.pointerId) return;
  if (state.moved) {
    // A drag emits a click after pointerup. Keep the card from opening as a
    // side effect of scrolling, while still allowing normal clicks.
    rail.dataset.suppressClick = "true";
    window.setTimeout(() => {
      if (rail.dataset.suppressClick === "true") delete rail.dataset.suppressClick;
    }, 0);
  }
  if (rail.hasPointerCapture?.(event.pointerId)) {
    rail.releasePointerCapture?.(event.pointerId);
  }
  rail.classList.remove("is-dragging");
  railDragState.delete(rail);
}

function handleRailClick(event) {
  const rail = event.currentTarget;
  if (rail.dataset.suppressClick !== "true") return;
  event.preventDefault();
  event.stopPropagation();
  delete rail.dataset.suppressClick;
}

function handleRailWheel(event) {
  const rail = event.currentTarget;
  if (rail.scrollWidth <= rail.clientWidth) return;
  // Trackpads already provide deltaX. A vertical mouse wheel should still
  // move a horizontal rail, which makes the affordance useful on desktop.
  const delta = Math.abs(event.deltaX) >= Math.abs(event.deltaY) ? event.deltaX : event.deltaY;
  if (!delta) return;
  const before = rail.scrollLeft;
  rail.scrollLeft += delta;
  if (rail.scrollLeft !== before) event.preventDefault();
}

function scrollRail(button, direction) {
  const rail = button?.parentElement?.querySelector(".recommend-personal-rail");
  if (!rail) return;
  rail.scrollBy({ left: direction * Math.max(rail.clientWidth * 0.72, 220), behavior: "smooth" });
}

const dynamicMoods = computed(() => {
  if (props.platform === "applemusic") {
    return props.recommendationTags.filter((item) => item.playlist_ids?.length);
  }
  return moods;
});

function cover(track) {
  return track?.cover || track?.cover_url || "";
}

function emitMood(item) {
  emit("mood", item);
}

function keyFor(track, index) {
  return `${trackIdentity(track) || track?.title || "track"}:${index}`;
}
</script>

<template>
  <section class="recommend-feed" aria-label="个性推荐">
    <div class="recommend-hero">
      <div class="recommend-hero__copy">
        <span class="recommend-kicker"><Sparkles :size="15" /> 随便听一点</span>
        <h2>{{ recentTracks.length ? "接着听，也留一点新鲜感" : "给你挑一段现在就能听的音乐" }}</h2>
        <p>
          {{ recentTracks.length
            ? (personalized
              ? `从 Apple Music 推荐池中发现音乐，也可以接着听此设备最近播放的歌曲。`
              : `可以接着听此设备最近播放的歌曲，也可以从本地歌单中选择。`)
            : (personalized
              ? `来自 Apple Music 推荐池，适合现在不想思考时直接播放。`
              : (platform === "applemusic" ? `发现 Apple Music 的公开歌单和新鲜歌曲，换一批继续探索。` : `先从 ${platformLabel} 的探索池开始，找一段适合现在的音乐。`)) }}
        </p>
        <div class="recommend-hero__actions">
          <button class="primary-action" :disabled="loading || !tracks.length" @click="emit('play-all')">
            <Play :size="16" />{{ loading ? "正在挑选" : "开始播放" }}
          </button>
          <button class="ghost-action" :disabled="loading" @click="emit('refresh')">
            <Shuffle :size="16" />换一批
          </button>
          <button class="ghost-action" :disabled="!tracks.length" @click="emit('queue-all')">
            <ListPlus :size="16" />加入播放队列
          </button>
        </div>
      </div>
      <div class="recommend-hero__stack" aria-hidden="true">
        <span v-for="(track, index) in tracks.slice(0, 4)" :key="keyFor(track, index)" class="recommend-hero__cover" :style="{ '--i': index }">
          <img v-if="cover(track)" :src="cover(track)" alt="" loading="lazy" />
          <Sparkles v-else :size="22" />
        </span>
      </div>
    </div>

    <div v-if="error" class="error-banner" role="status">{{ error }}</div>

    <div v-if="dynamicMoods.length" class="recommend-moods" aria-label="选择聆听场景">
      <button
        v-for="item in dynamicMoods"
        :key="item.id"
        class="recommend-mood"
        :class="{ 'is-active': mood === item.id }"
        type="button"
        @click="emitMood(item)"
      >
        <span>{{ item.label }}</span>
        <small v-if="item.subtitle">{{ item.subtitle }}</small>
      </button>
    </div>

    <section v-if="recentTracks.length" class="recommend-section">
      <div class="recommend-section__head">
        <div><h3>继续播放</h3><p>从最近听过的音乐接上</p></div>
        <button class="mini-button" title="刷新推荐" @click="emit('refresh')"><RefreshCcw :size="16" /></button>
      </div>
      <div
        class="recommend-rail"
        tabindex="0"
        aria-label="最近播放，可横向滚动"
        @pointerdown="beginRailDrag"
        @pointermove="moveRailDrag"
        @pointerup="endRailDrag"
        @pointercancel="endRailDrag"
        @pointerleave="endRailDrag"
        @click.capture="handleRailClick"
        @dragstart.prevent
        @wheel="handleRailWheel"
      >
        <article v-for="(track, index) in recentTracks.slice(0, 6)" :key="keyFor(track, index)" class="recommend-track-card">
          <button class="recommend-track-card__cover" type="button" @click="emit('open', track)">
            <img v-if="cover(track)" :src="cover(track)" alt="" loading="lazy" />
            <Play v-else :size="22" />
          </button>
          <button class="recommend-track-card__name" type="button" @click="emit('play', track)">
            <strong>{{ track.title }}</strong><small>{{ track.artist || "未知艺人" }}</small>
          </button>
          <div class="recommend-track-card__actions">
            <button class="mini-button" title="喜欢" @click="emit('like', track)"><Heart :size="15" /></button>
            <button class="mini-button" title="加入播放队列" @click="emit('queue', track)"><ListPlus :size="15" /></button>
            <button class="mini-button" title="少推荐类似音乐" @click="emit('less', track)"><Ban :size="15" /></button>
          </div>
        </article>
      </div>
    </section>

    <section v-if="recommendationPlaylists.length" class="recommend-section">
      <div class="recommend-section__head">
        <div><h3>专属推荐歌单</h3><p>来自 Apple Music · 点开查看曲目</p></div>
        <span class="recommend-scroll-hint" aria-hidden="true">横向浏览 · 拖动或滚轮</span>
      </div>
      <div class="recommend-rail-shell">
        <button class="recommend-rail-control recommend-rail-control--prev" type="button" title="向左浏览" aria-label="向左浏览推荐歌单" @click="scrollRail($event.currentTarget, -1)"><ChevronLeft :size="18" /></button>
        <div
          class="recommend-personal-rail"
          tabindex="0"
          aria-label="专属推荐歌单，可横向滚动"
          @pointerdown="beginRailDrag"
          @pointermove="moveRailDrag"
          @pointerup="endRailDrag"
          @pointercancel="endRailDrag"
          @pointerleave="endRailDrag"
          @click.capture="handleRailClick"
          @dragstart.prevent
          @wheel="handleRailWheel"
        >
          <button v-for="playlist in recommendationPlaylists" :key="playlist.id" class="recommend-personal-card" :data-recommendation-id="playlist.id" type="button" :aria-label="`查看${playlist.resource_type === 'albums' ? '专辑' : '歌单'}：${playlist.name}`" @click="emit('playlist', playlist)">
            <span class="recommend-personal-card__cover">
              <img v-if="playlist.cover" :src="playlist.cover" alt="" loading="lazy" />
              <Sparkles v-else :size="22" />
            </span>
            <span class="recommend-personal-card__copy">
              <strong>{{ playlist.name }}</strong>
              <small>{{ playlist.resource_type === "albums" ? `Apple Music 专辑 · ${playlist.curator || "未知艺人"}` : (playlist.curator || "Apple Music") }}</small>
              <em v-if="playlist.description">{{ playlist.description }}</em>
            </span>
          </button>
        </div>
        <button class="recommend-rail-control recommend-rail-control--next" type="button" title="向右浏览" aria-label="向右浏览推荐歌单" @click="scrollRail($event.currentTarget, 1)"><ChevronRight :size="18" /></button>
      </div>
    </section>

    <section v-if="playlists.length" class="recommend-section">
      <div class="recommend-section__head">
        <div><h3>从你的歌单继续</h3><p>熟悉的音乐，适合不想思考的时候</p></div>
      </div>
      <div
        class="recommend-playlist-rail"
        tabindex="0"
        aria-label="本地歌单，可横向滚动"
        @pointerdown="beginRailDrag"
        @pointermove="moveRailDrag"
        @pointerup="endRailDrag"
        @pointercancel="endRailDrag"
        @pointerleave="endRailDrag"
        @click.capture="handleRailClick"
        @dragstart.prevent
        @wheel="handleRailWheel"
      >
        <button v-for="playlist in playlists.slice(0, 4)" :key="playlist.id" class="recommend-playlist-card" type="button" @click="emit('playlist', playlist)">
          <span class="recommend-playlist-card__cover">
            <img v-if="playlist.cover" :src="playlist.cover" alt="" loading="lazy" />
            <Sparkles v-else :size="22" />
          </span>
          <strong>{{ playlist.name }}</strong>
          <small>{{ playlist.tracks?.length || 0 }} 首 · 查看歌单</small>
        </button>
      </div>
    </section>

    <section class="recommend-section">
      <div class="recommend-section__head">
        <div><h3>{{ mood ? "为这个感觉挑的歌" : "探索新声音" }}</h3><p>{{ mood ? "可以随时换一个场景" : "熟悉之外，留一点偶遇" }}</p></div>
        <button class="mini-button" title="换一批" :disabled="loading" @click="emit('refresh')"><RefreshCcw :size="16" /></button>
      </div>
      <div v-if="loading && !tracks.length" class="recommend-loading">正在准备一组适合现在的音乐…</div>
      <div v-else-if="!tracks.length" class="recommend-loading">还没有推荐曲目，可以点“换一批”重新获取。</div>
      <div v-else class="recommend-track-list" :aria-busy="loading">
        <article v-for="(track, index) in tracks" :key="keyFor(track, index)" class="recommend-list-row">
          <button class="recommend-list-row__play" type="button" title="播放" @click="emit('play', track)"><Play :size="15" /></button>
          <span class="cover-thumb is-small"><img v-if="cover(track)" :src="cover(track)" alt="" loading="lazy" /></span>
          <button class="recommend-list-row__text" type="button" @click="emit('open', track)"><strong>{{ track.title }}</strong><small>{{ track.artist || "未知艺人" }} · {{ track.platform || platformLabel }}</small></button>
          <button class="mini-button" title="喜欢" @click="emit('like', track)"><Heart :size="15" /></button>
          <button class="mini-button" title="加入播放队列" @click="emit('queue', track)"><ListPlus :size="15" /></button>
          <button class="mini-button" title="少推荐类似音乐" @click="emit('less', track)"><Ban :size="15" /></button>
        </article>
      </div>
    </section>
  </section>
</template>
