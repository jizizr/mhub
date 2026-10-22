<script setup>
import { computed, onBeforeUnmount, onBeforeUpdate, ref, watch } from "vue";
import { ArrowUpRight, GripVertical, Infinity as InfinityIcon, ListMusic, LoaderCircle, Trash2, X } from "lucide-vue-next";
import * as player from "../../core/player";
import { trackIdentity, useQueueStore } from "../../stores/queue";
import { useStatusStore } from "../../stores/status";
import ListeningSettings from "./ListeningSettings.vue";
import { queueDropIndex, queueScrollStep } from "../../utils/queue-drag";

defineProps({
  embedded: { type: Boolean, default: false },
});

const queue = useQueueStore();
const status = useStatusStore();
const draggingIndex = ref(-1);
const dragOverIndex = ref(-1);
const dragOffset = ref(0);
const queueList = ref(null);
const dragAnnouncement = ref("");
let drag = null;
let dragFrame = 0;
let lastLayoutOrder = "";
const rowPositions = new WeakMap();
const sourcePlaylist = computed(() => queue.sourcePlaylist);
const autoplayMessage = computed(() => {
  if (!status.autoplayEnabled) return "队列结束后，继续发现新歌曲";
  if (status.autoplayState === "loading") return "正在挑选接下来听的歌曲…";
  if (status.autoplayState === "error") return status.autoplayError || "推荐暂时不可用，可以重试";
  if (status.autoplayState === "empty") return status.autoplayError || "暂时没有新的推荐，可以稍后重试";
  if (status.autoplayState === "ready") return "已准备好推荐，接在当前队列后播放";
  return "已开启 · 播完当前队列后自动继续";
});

function liveRows() {
  return [...(queueList.value?.querySelectorAll(".queue-item:not(.queue-leave-active)") || [])];
}

function scrollParent(element) {
  for (let parent = element?.parentElement; parent; parent = parent.parentElement) {
    if (/(auto|scroll)/.test(getComputedStyle(parent).overflowY) && parent.scrollHeight > parent.clientHeight) return parent;
  }
  return document.scrollingElement;
}

function beginDrag(index, event) {
  if (event.button !== 0 || !event.isPrimary || queue.queueLength < 2) return;
  endDrag();
  const handle = event.currentTarget;
  const row = handle.closest(".queue-item");
  const scroller = scrollParent(queueList.value);
  if (!row || !scroller) return;
  // Only the handle owns touch-action:none. A swipe on cover/title still scrolls.
  event.preventDefault();
  handle.focus({ preventScroll: true });
  drag = { id: event.pointerId, key: trackIdentity(queue.queue[index]), index, handle, scroller,
    startY: event.clientY, y: event.clientY, startX: event.clientX, startScroll: scroller.scrollTop,
    centerOffset: row.getBoundingClientRect().top + row.offsetHeight / 2 - event.clientY,
    active: false, lastFrame: 0 };
  handle.setPointerCapture(event.pointerId);
  window.addEventListener("blur", endDrag);
  window.addEventListener("keydown", cancelDragOnEscape);
}

function updateDrag(event) {
  if (!drag || drag.id !== event.pointerId) return;
  drag.y = event.clientY;
  if (!drag.active && Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) >= 6) {
    drag.active = true;
    draggingIndex.value = drag.index;
    dragOverIndex.value = drag.index;
    dragFrame = requestAnimationFrame(animateDrag);
  }
  if (drag.active) {
    event.preventDefault();
  }
}

function updateDragPosition() {
  if (!drag?.active) return;
  dragOffset.value = drag.y - drag.startY + drag.scroller.scrollTop - drag.startScroll;
  const rows = liveRows().map(row => row.getBoundingClientRect());
  dragOverIndex.value = queueDropIndex(rows, drag.index, drag.y + drag.centerOffset);
}

function animateDrag(timestamp) {
  if (!drag?.active) return;
  const isDocument = drag.scroller === document.scrollingElement;
  const bounds = isDocument ? { top: 0, bottom: window.innerHeight } : drag.scroller.getBoundingClientRect();
  const viewport = window.visualViewport;
  const top = Math.max(viewport?.offsetTop || 0, bounds.top);
  let bottom = Math.min((viewport?.offsetTop || 0) + (viewport?.height || window.innerHeight), bounds.bottom);
  const dock = document.querySelector(queueList.value.closest(".queue-panel--full") ? ".full-player__bottom" : ".player-bar");
  if (dock?.getClientRects().length) bottom = Math.min(bottom, dock.getBoundingClientRect().top);
  const step = queueScrollStep(drag.y, top, bottom, drag.lastFrame ? timestamp - drag.lastFrame : 16);
  drag.lastFrame = timestamp;
  if (step) drag.scroller.scrollTop += step;
  updateDragPosition();
  dragFrame = requestAnimationFrame(animateDrag);
}

function dropTrack(event) {
  if (!drag || drag.id !== event.pointerId) return;
  drag.y = event.clientY;
  updateDragPosition();
  const fromIndex = queue.queue.findIndex(track => trackIdentity(track) === drag.key);
  const toIndex = dragOverIndex.value;
  const shouldMove = drag.active && fromIndex >= 0 && toIndex >= 0 && fromIndex !== toIndex;
  endDrag();
  if (shouldMove) {
    player.moveInQueue(fromIndex, toIndex);
    announceMove(toIndex);
  }
}

function endDrag() {
  const current = drag;
  drag = null;
  cancelAnimationFrame(dragFrame);
  dragFrame = 0;
  if (current?.handle.hasPointerCapture(current.id)) current.handle.releasePointerCapture(current.id);
  window.removeEventListener("blur", endDrag);
  window.removeEventListener("keydown", cancelDragOnEscape);
  draggingIndex.value = -1;
  dragOverIndex.value = -1;
  dragOffset.value = 0;
}

function cancelDragOnEscape(event) {
  if (event.key === "Escape") { event.preventDefault(); endDrag(); }
}

function announceMove(index) {
  dragAnnouncement.value = `已移至第 ${index + 1} 首：${queue.queue[index]?.title || ""}`;
}

function moveWithKeyboard(index, event) {
  const moves = { ArrowUp: index - 1, ArrowDown: index + 1, Home: 0, End: queue.queueLength - 1 };
  if (!(event.key in moves)) return;
  event.preventDefault();
  endDrag();
  const target = Math.max(0, Math.min(queue.queueLength - 1, moves[event.key]));
  if (target !== index) { player.moveInQueue(index, target); announceMove(target); }
}

// Capture every row before Vue removes any of them, including bulk clear.
// Absolute leave rows otherwise collapse to the first flex position and flash.
onBeforeUpdate(() => {
  const order = queue.queue.map(trackIdentity).join("\0");
  if (order === lastLayoutOrder) return;
  lastLayoutOrder = order;
  for (const row of liveRows()) rowPositions.set(row, {
    top: row.offsetTop, left: row.offsetLeft, width: row.offsetWidth, height: row.offsetHeight,
  });
});
function pinLeavingRow(row) {
  const bounds = rowPositions.get(row);
  if (!bounds) return;
  for (const [name, value] of Object.entries(bounds)) row.style[name] = `${value}px`;
}
function clearLeavingRow(row) {
  for (const name of ["top", "left", "width", "height"]) row.style.removeProperty(name);
}

// An external queue change, closing the drawer, or unmount must not leave
// pointer capture/auto-scroll alive or commit stale indices.
watch(() => queue.queue.map(trackIdentity).join("\0"), endDrag, { flush: "sync" });
watch(() => status.queueOpen, endDrag);
onBeforeUnmount(endDrag);

async function clearQueue() {
  if (!queue.queueLength) return;
  if (!window.confirm("清空当前播放队列？正在播放的歌曲也会停止。")) return;
  await player.clearQueue();
}

function removeTrack(track) {
  endDrag();
  // Resolve the identity at click time, including consecutive removals before
  // Vue has flushed the next DOM frame; captured indices may already be stale.
  const index = queue.findTrackIndex(track);
  if (index >= 0) player.removeFromQueue(index);
}

function openQueuePage() {
  status.activeView = "queue";
  status.activePlaylistId = null;
  status.queueOpen = false;
  status.persist();
}

function openSourcePlaylist() {
  if (!sourcePlaylist.value?.id) return;
  if (sourcePlaylist.value.type === "recommendation") {
    status.activeView = "discover";
    status.activePlaylistId = null;
    status.queueOpen = false;
    status.persist();
    return;
  }
  status.activeView = "playlists";
  status.activePlaylistId = sourcePlaylist.value.id;
  status.queueOpen = false;
  status.persist();
}
</script>

<template>
  <aside class="queue-panel" :class="{ 'queue-panel--embedded': embedded, 'is-open': status.queueOpen || embedded }">
    <div class="panel-head">
      <div>
        <h2 v-if="!embedded">播放队列</h2>
        <p>{{ queue.queueLength }} 首音乐 · 拖动左侧手柄排序</p>
      </div>
      <div class="panel-head__actions">
        <button v-if="!embedded" class="mini-button" title="打开队列页" @click="openQueuePage">
          <ArrowUpRight :size="16" />
        </button>
        <button class="mini-button" title="清空队列" :disabled="!queue.queueLength" @click="clearQueue">
          <Trash2 :size="16" />
        </button>
        <button v-if="!embedded" class="mini-button" title="关闭" @click="status.queueOpen = false">
          <X :size="18" />
        </button>
      </div>
    </div>

    <div class="queue-panel__body">
      <button v-if="sourcePlaylist" class="queue-source" type="button" @click="openSourcePlaylist">
        <ListMusic :size="15" />
        <span>{{ sourcePlaylist.type === "recommendation" ? "来自推荐：" : "来自歌单：" }}{{ sourcePlaylist.name }}</span>
        <ArrowUpRight :size="14" />
      </button>

      <div class="queue-autoplay">
        <button class="queue-autoplay__toggle" type="button" :class="{ 'is-active': status.autoplayEnabled }" :aria-pressed="status.autoplayEnabled" @click="player.toggleAutoplay()">
          <InfinityIcon :size="21" />
          <span>自动续播</span>
          <span class="queue-autoplay__switch" aria-hidden="true"><i /></span>
        </button>
        <div class="queue-autoplay__status" role="status" aria-live="polite">
          <LoaderCircle v-if="status.autoplayEnabled && status.autoplayState === 'loading'" :size="14" class="is-spinning" />
          <span>{{ autoplayMessage }}</span>
          <button v-if="status.autoplayEnabled && ['error', 'empty'].includes(status.autoplayState)" class="queue-autoplay__retry" type="button" @click="player.retryAutoplay()">重试</button>
        </div>
      </div>

      <ListeningSettings />

      <p class="queue-drag-announcement" role="status" aria-live="polite">{{ dragAnnouncement }}</p>
      <div ref="queueList" class="queue-list">
        <TransitionGroup name="queue" tag="div" class="queue-list__rows" @before-leave="pinLeavingRow" @after-leave="clearLeavingRow" @leave-cancelled="clearLeavingRow">
          <div
            v-for="(track, index) in queue.queue"
            :key="trackIdentity(track)"
            :data-track-key="trackIdentity(track)"
            class="queue-item"
            :class="{ 'is-current': index === status.playIndex, 'is-dragging': index === draggingIndex, 'is-drag-target-before': index === dragOverIndex && index < draggingIndex, 'is-drag-target-after': index === dragOverIndex && index > draggingIndex }"
            :style="index === draggingIndex ? { transform: `translateY(${dragOffset}px)` } : null"
            @dblclick="player.playAtIndex(index)"
          >
            <button class="queue-item__grip" type="button" :aria-label="`调整 ${track.title} 的顺序，当前第 ${index + 1} 首`" title="拖动排序，也可使用上下方向键" @pointerdown="beginDrag(index, $event)" @pointermove="updateDrag" @pointerup="dropTrack" @pointercancel="endDrag" @lostpointercapture="endDrag" @keydown="moveWithKeyboard(index, $event)" @click.stop @dblclick.stop>
              <GripVertical :size="18" aria-hidden="true" />
            </button>
            <span class="cover-thumb is-small">
              <img v-if="track.cover" :src="track.cover" alt="" loading="lazy" />
            </span>
            <button class="queue-item__text" type="button" @click="player.playAtIndex(index)">
              <span>{{ track.title }}</span>
              <small>{{ track.artist || "未知艺人" }}<span v-if="track.autoplayRecommended" class="queue-item__recommended"> · ∞ 推荐</span></small>
            </button>
            <button class="mini-button queue-item__remove" title="从队列移除" @click.stop="removeTrack(track)">
              <Trash2 :size="15" />
            </button>
          </div>
        </TransitionGroup>
        <div v-if="!queue.queueLength" class="queue-empty">
          <ListMusic :size="24" />
          <strong>队列是空的</strong>
          <span>在搜索结果或歌单中点击“加入播放队列”来添加音乐</span>
        </div>
      </div>
    </div>
  </aside>
</template>

<style scoped>
.queue-autoplay { flex: 0 0 auto; padding: 10px 12px; margin: 0 0 12px; border: 1px solid var(--line); border-radius: 14px; background: var(--surface); }
.queue-autoplay__toggle { display: flex; align-items: center; gap: 9px; width: 100%; padding: 0; border: 0; color: var(--text-muted); background: transparent; font-size: 14px; font-weight: 700; text-align: left; cursor: pointer; }
.queue-autoplay__toggle.is-active { color: var(--accent); }
.queue-autoplay__switch { display: flex; align-items: center; margin-left: auto; width: 32px; height: 20px; padding: 3px; border-radius: 12px; background: var(--surface-muted); transition: background var(--motion-fast); }
.queue-autoplay__switch i { width: 14px; height: 14px; border-radius: 50%; background: white; box-shadow: 0 1px 3px #0003; transition: transform var(--motion-fast) var(--ease-standard); }
.is-active .queue-autoplay__switch { background: var(--accent); }
.is-active .queue-autoplay__switch i { transform: translateX(12px); }
.queue-autoplay__status { display: flex; align-items: center; gap: 6px; min-height: 20px; margin-top: 6px; color: var(--text-muted); font-size: 12px; line-height: 1.5; }
.queue-autoplay__status > span { flex: 1; min-width: 0; }
.queue-autoplay__status > svg { flex: 0 0 auto; }
.queue-autoplay__retry { flex: 0 0 auto; padding: 3px 0 3px 6px; border: 0; background: transparent; color: var(--accent); cursor: pointer; }
.queue-item__recommended { color: var(--accent); font-size: 11px; }
@media (prefers-reduced-motion: reduce) { .queue-autoplay__switch, .queue-autoplay__switch i { transition: none; } }
</style>
