<script setup>
import { computed } from "vue";
import { ChevronLeft, ChevronRight, Ellipsis, ListMusic, LoaderCircle, Play, RefreshCcw, Shuffle, Sparkles } from "lucide-vue-next";
import { trackIdentity } from "../../stores/queue";

// Per-song actions (next, queue, like, less, add to playlist) live in the
// song menu: `menu` opens it, `more` opens the batch menu for the whole pool.
// As in Apple Music, a card opens its playlist and its play button plays it;
// a song plays where it is, without opening the full player.
const emit = defineEmits(["play", "menu", "more", "refresh", "mood", "play-all", "playlist", "play-playlist"]);

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
  pendingPlaylist: { type: String, default: "" },
});

// Keep horizontal rails discoverable and usable without relying on the OS
// overlay scrollbar. A vertical mouse wheel is never captured: the home page
// is mostly rails, so it always scrolls the page; trackpad swipes, Shift+wheel,
// the arrow buttons and mouse dragging move a rail. Only mouse dragging is
// handled here: touch/pen scrolling stays native.
const railDragState = new WeakMap();
const railTimers = new WeakMap();

// A mouse drag moves the rail with CSS snapping paused, then glides to the
// nearest card once. Snapping while the position is being set by script made
// every drag end jump.
function pauseSnap(rail) {
  window.clearTimeout(railTimers.get(rail));
  rail.classList.add("is-free-scrolling");
}

function settleRail(rail, velocity = 0) {
  const cards = [...rail.children];
  if (!cards.length) { rail.classList.remove("is-free-scrolling"); return; }
  const origin = cards[0].offsetLeft;
  const max = rail.scrollWidth - rail.clientWidth;
  // Project the release velocity (px/ms) a little, like native momentum.
  // Capped like native momentum, so a fast flick moves about two cards.
  const capped = Math.max(-2.5, Math.min(2.5, velocity));
  const projected = Math.min(max, Math.max(0, rail.scrollLeft + capped * 220));
  let target = projected >= max - 2 ? max : 0;
  if (target !== max) {
    let best = Infinity;
    for (const card of cards) {
      const left = Math.min(max, card.offsetLeft - origin);
      if (Math.abs(left - projected) < best) { best = Math.abs(left - projected); target = left; }
    }
  }
  rail.scrollTo({ left: target, behavior: "smooth" });
  const release = () => rail.classList.remove("is-free-scrolling");
  rail.addEventListener("scrollend", release, { once: true });
  railTimers.set(rail, window.setTimeout(release, 600));
}

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
    lastX: event.clientX,
    lastTime: event.timeStamp,
    velocity: 0,
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
    pauseSnap(rail);
    rail.setPointerCapture?.(event.pointerId);
  }
  if (!state.moved) return;
  event.preventDefault();
  const elapsed = event.timeStamp - state.lastTime;
  if (elapsed > 0) {
    // Smoothed scroll velocity; pointer movement is opposite to scrollLeft.
    state.velocity = 0.7 * ((state.lastX - event.clientX) / elapsed) + 0.3 * state.velocity;
    state.lastX = event.clientX;
    state.lastTime = event.timeStamp;
  }
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
  // A pause before release means the user stopped: no momentum.
  if (state.moved) settleRail(rail, event.timeStamp - state.lastTime > 80 ? 0 : state.velocity);
}

function handleRailClick(event) {
  const rail = event.currentTarget;
  if (rail.dataset.suppressClick !== "true") return;
  event.preventDefault();
  event.stopPropagation();
  delete rail.dataset.suppressClick;
}

function scrollRail(button, direction) {
  const rail = button?.parentElement?.querySelector(".artwork-rail");
  if (!rail) return;
  rail.scrollBy({ left: direction * Math.max(rail.clientWidth * 0.72, 220), behavior: "smooth" });
}

const GENERIC_TAG = /^(为你精选|精选推荐\s*\d+)$/;

const dynamicMoods = computed(() => {
  if (props.platform === "applemusic") {
    // Apple shelf titles are never copied (they can carry account names), so
    // numbered placeholders say nothing; show only tags with a real name.
    return props.recommendationTags.filter((item) => item.playlist_ids?.length && !GENERIC_TAG.test(item.label || ""));
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
    <div class="home-actions">
      <button class="primary-action" :disabled="loading || !tracks.length" @click="emit('play-all')">
        <Play :size="16" />{{ loading && !tracks.length ? "正在挑选" : "播放推荐" }}
      </button>
      <button class="ghost-action" :disabled="loading" @click="emit('refresh')">
        <Shuffle :size="16" />换一批
      </button>
      <button class="ghost-action home-actions__more" type="button" title="更多" aria-label="更多推荐操作" aria-haspopup="menu" :disabled="!tracks.length" @click="emit('more', $event)">
        <Ellipsis :size="18" />
      </button>
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
      >{{ item.label }}</button>
    </div>

    <section v-if="recommendationPlaylists.length" class="recommend-section">
      <h2 class="section-title">为你推荐</h2>
      <div class="recommend-rail-shell">
        <button class="recommend-rail-control recommend-rail-control--prev" type="button" aria-label="向左浏览" @click="scrollRail($event.currentTarget, -1)"><ChevronLeft :size="18" /></button>
        <div
          class="artwork-rail artwork-rail--featured"
          tabindex="0"
          aria-label="推荐歌单"
          @pointerdown="beginRailDrag"
          @pointermove="moveRailDrag"
          @pointerup="endRailDrag"
          @pointercancel="endRailDrag"
          @pointerleave="endRailDrag"
          @click.capture="handleRailClick"
          @dragstart.prevent
        >
          <article v-for="playlist in recommendationPlaylists" :key="playlist.id" class="artwork-card">
            <div class="artwork-card__art">
              <button class="artwork-card__cover" :data-recommendation-id="playlist.id" type="button" :aria-label="`查看${playlist.resource_type === 'albums' ? '专辑' : '歌单'}：${playlist.name}`" @click="emit('playlist', playlist)">
                <img v-if="playlist.cover" :src="playlist.cover" alt="" loading="lazy" />
                <Sparkles v-else :size="24" />
              </button>
              <button class="artwork-card__play" :class="{ 'is-pending': pendingPlaylist === playlist.id }" type="button" title="播放" :aria-label="`播放${playlist.resource_type === 'albums' ? '专辑' : '歌单'}：${playlist.name}`" @click="emit('play-playlist', playlist)">
                <LoaderCircle v-if="pendingPlaylist === playlist.id" class="is-spinning" :size="16" />
                <Play v-else :size="15" fill="currentColor" />
              </button>
            </div>
            <button class="artwork-card__text" type="button" tabindex="-1" aria-hidden="true" @click="emit('playlist', playlist)">
              <strong>{{ playlist.name }}</strong>
              <small>{{ playlist.curator || "Apple Music" }}</small>
            </button>
          </article>
        </div>
        <button class="recommend-rail-control recommend-rail-control--next" type="button" aria-label="向右浏览" @click="scrollRail($event.currentTarget, 1)"><ChevronRight :size="18" /></button>
      </div>
    </section>

    <section v-if="recentTracks.length" class="recommend-section">
      <h2 class="section-title">最近播放</h2>
      <div
        class="artwork-rail"
        tabindex="0"
        aria-label="最近播放"
        @pointerdown="beginRailDrag"
        @pointermove="moveRailDrag"
        @pointerup="endRailDrag"
        @pointercancel="endRailDrag"
        @pointerleave="endRailDrag"
        @click.capture="handleRailClick"
        @dragstart.prevent
      >
        <article v-for="(track, index) in recentTracks.slice(0, 10)" :key="keyFor(track, index)" class="artwork-card" @contextmenu.prevent="emit('menu', { event: $event, track })">
          <div class="artwork-card__art">
            <button class="artwork-card__cover" type="button" :aria-label="`播放 ${track.title}`" @click="emit('play', track)">
              <img v-if="cover(track)" :src="cover(track)" alt="" loading="lazy" />
              <Play v-else :size="24" />
            </button>
            <span class="artwork-card__play" aria-hidden="true"><Play :size="15" fill="currentColor" /></span>
          </div>
          <div class="artwork-card__meta">
            <button class="artwork-card__text" type="button" tabindex="-1" @click="emit('play', track)">
              <strong>{{ track.title }}</strong>
              <small>{{ track.artist || "未知艺人" }}</small>
            </button>
            <button class="artwork-card__more" type="button" aria-label="更多操作" aria-haspopup="menu" @click="emit('menu', { event: $event, track })"><Ellipsis :size="18" /></button>
          </div>
        </article>
      </div>
    </section>

    <section v-if="playlists.length" class="recommend-section">
      <h2 class="section-title">我的歌单</h2>
      <div
        class="artwork-rail"
        tabindex="0"
        aria-label="我的歌单"
        @pointerdown="beginRailDrag"
        @pointermove="moveRailDrag"
        @pointerup="endRailDrag"
        @pointercancel="endRailDrag"
        @pointerleave="endRailDrag"
        @click.capture="handleRailClick"
        @dragstart.prevent
      >
        <article v-for="playlist in playlists.slice(0, 10)" :key="playlist.id" class="artwork-card">
          <div class="artwork-card__art">
            <button class="artwork-card__cover" type="button" :aria-label="`查看歌单：${playlist.name}`" @click="emit('playlist', playlist)">
              <img v-if="playlist.cover" :src="playlist.cover" alt="" loading="lazy" />
              <ListMusic v-else :size="24" />
            </button>
            <button v-if="playlist.tracks?.length" class="artwork-card__play" :class="{ 'is-pending': pendingPlaylist === playlist.id }" type="button" title="播放" :aria-label="`播放歌单：${playlist.name}`" @click="emit('play-playlist', playlist)">
              <LoaderCircle v-if="pendingPlaylist === playlist.id" class="is-spinning" :size="16" />
              <Play v-else :size="15" fill="currentColor" />
            </button>
          </div>
          <button class="artwork-card__text" type="button" tabindex="-1" aria-hidden="true" @click="emit('playlist', playlist)">
            <strong>{{ playlist.name }}</strong>
            <small>{{ playlist.tracks?.length || 0 }} 首</small>
          </button>
        </article>
      </div>
    </section>

    <section class="recommend-section">
      <div class="section-head">
        <h2 class="section-title">{{ mood ? dynamicMoods.find((item) => item.id === mood)?.label || "推荐歌曲" : "推荐歌曲" }}</h2>
        <button class="mini-button" title="换一批" aria-label="换一批推荐歌曲" :disabled="loading" @click="emit('refresh')"><RefreshCcw :size="16" /></button>
      </div>
      <div v-if="loading && !tracks.length" class="recommend-loading">正在挑选…</div>
      <div v-else-if="!tracks.length" class="recommend-loading">暂无推荐，点“换一批”重试</div>
      <div v-else class="recommend-track-list" :aria-busy="loading">
        <article v-for="(track, index) in tracks" :key="keyFor(track, index)" class="recommend-list-row" @contextmenu.prevent="emit('menu', { event: $event, track })">
          <button class="recommend-list-row__cover" type="button" :aria-label="`播放 ${track.title}`" @click="emit('play', track)">
            <img v-if="cover(track)" :src="cover(track)" alt="" loading="lazy" />
            <Play class="recommend-list-row__play" :size="16" />
          </button>
          <button class="recommend-list-row__text" type="button" @click="emit('play', track)"><strong>{{ track.title }}</strong><small>{{ track.artist || "未知艺人" }}</small></button>
          <button class="mini-button" type="button" aria-label="更多操作" aria-haspopup="menu" @click="emit('menu', { event: $event, track })"><Ellipsis :size="18" /></button>
        </article>
      </div>
    </section>
  </section>
</template>
