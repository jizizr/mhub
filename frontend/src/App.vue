<script setup>
import { computed, defineAsyncComponent, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { Ban, ChevronLeft, ChevronRight, Disc3, FolderOpen, HardDrive, Heart, ListMusic, ListPlus, ListStart, MoreHorizontal, Pencil, Play, Plus, RefreshCcw, Search, Trash2, X } from "lucide-vue-next";
import {
  audioUrl,
  createPlayback,
  getPlayback,
  getPlatforms,
  getRecommendationHome,
  getContinuationRecommendations,
  resolveCollection,
  searchTracks,
} from "./apis/music";
import { trustedMediaOrigins } from "./apis/media-routing.js";
import Sidebar from "./components/layout/Sidebar.vue";
import PageHeader from "./components/layout/PageHeader.vue";
import SongList from "./components/list/SongList.vue";
import PlaylistDetail from "./components/list/PlaylistDetail.vue";
import RecommendationFeed from "./components/discovery/RecommendationFeed.vue";
import PlayerBar from "./components/player/PlayerBar.vue";
import QueuePanel from "./components/player/QueuePanel.vue";
import * as player from "./core/player";
import { prepareOutput, preparePlaybackSession, createNativeOutput } from "./core/player/audio-output";
import { createMediaSessionController } from "./core/player/media-session";
import { createPlaybackLifecycle } from "./core/player/playback-lifecycle";
import { getCachedLoudness, warmLoudness } from "./services/loudness";
import { loadTrackLyrics, lyricIdentity } from "./services/lyrics";
import { canRequestAlacWorklet, canUseAlacWorklet } from "./core/player/alac-support";
import {
  cacheAudioBlob,
  cachedAudioUrl,
  cacheKey,
  cacheEntryMatches,
  cacheStats,
  clearCache,
  deleteCachedTrack,
  getCachedTrack,
  hasCachedAudio,
  reusableNativeFlacCache,
  listCachedTracks,
  putCachedTrack,
  releaseCachedAudioUrl,
} from "./services/localCache";
import { useMediaStore } from "./stores/media";
import { trackIdentity, useQueueStore } from "./stores/queue";
import { usePlaylistStore } from "./stores/playlists";
import { useAccountStore } from "./stores/account";
import HandoffCard from "./components/account/HandoffCard.vue";
import { useStatusStore } from "./stores/status";
import { formatSize } from "./utils/format";
import { createThemeController } from "./utils/theme.js";
import { trackFromPlayback, trackFromSearch } from "./utils/track";

const audioRef = ref(null);
const platforms = ref([]);
const query = ref("");
const searchedQuery = ref("");
const results = ref([]);
const recommendationTracks = ref([]);
const recommendationLoading = ref(false);
const recommendationError = ref("");
const recommendationMood = ref("");
const recommendationMoodLabel = ref("");
const recommendationPersonal = ref(false);
const recommendationPlaylists = ref([]);
const recommendationDetail = ref(null);
const recommendationDetailLoading = ref(false);
const recommendationDetailError = ref("");
const playlistActionBusy = ref(false);
const playlistActionNotice = ref("");
const recommendationTags = ref([]);
const recommendationSeedIndex = ref(0);
const recentTracks = ref([]);
const searchLoading = ref(false);
const searchError = ref("");
const cacheItems = ref([]);
const cacheLoading = ref(false);
const importOpen = ref(false);
const playlistMenu = ref({ open: false, x: 0, y: 0, playlistId: "" });
const songMenu = ref({ open: false, x: 0, y: 0, track: null, playlistId: "", recommend: false });
const feedMenu = ref({ open: false, x: 0, y: 0 });
const status = useStatusStore();
const media = useMediaStore();
const queue = useQueueStore();
const playlists = usePlaylistStore();
const account = useAccountStore();
const AccountDialog = defineAsyncComponent(() => import("./components/account/AccountDialog.vue"));
let handoffTimer = 0;
const FullPlayer = defineAsyncComponent(() => import("./components/player/FullPlayer.vue"));
const ImportDialog = defineAsyncComponent(() => import("./components/import/ImportDialog.vue"));
// Keep the instance after its first opening so its own leave transition and
// per-song controls survive closing. Before that, don't request its chunk.
const fullPlayerMounted = ref(media.expanded);

let uiSyncTimer = 0;
let appUnmounted = false;
let searchAbort = null;
let recommendationAbort = null;
let recommendationPoolExhausted = false;
let fullPlayerHistoryActive = false;
let queueHistoryActive = false;
let closingFromPopstate = false;
let recommendationRequestId = 0;
let recommendationDetailVersion = 0;
let playlistActionVersion = 0;
let recommendationHistoryActive = false;
let recommendationReturnScroll = 0;
let recommendationReturnFocus = null;
let recommendationReturnId = "";
let pendingPageScroll = null;
let pendingUiHistoryBack = 0;
// A remote detail is transient; a reload returns to its browsable parent.
if (status.activeView === "recommendation") {
  status.activeView = "discover";
  if (window.history.state?.musicUi === "recommendation") {
    window.history.replaceState({ ...window.history.state, musicUi: null }, "");
  }
}
// Search results are transient too; a reload always starts at the feed.
if (window.history.state?.musicUi === "search") {
  window.history.replaceState({ ...window.history.state, musicUi: null }, "");
}
// De-duplicate concurrent playback-task creation for the same track and
// transport. A double click or a prefetch racing with a manual play should
// share the backend task instead of opening two Telegram/provider streams.
const inflightTrackResolutions = new Map();

const UI_SYNC_INTERVAL_MS = 250;
const RECOMMENDATION_SEEDS = ["chill", "focus", "night drive", "indie pop", "华语流行", "jazz", "ambient"];
const RECENT_TRACKS_KEY = "music-web-splayer-recent-tracks-v1";
const RECOMMENDATION_SEEN_KEY = "music-web-splayer-recommendation-seen-v1";
let recommendationSeenIds = [];
try {
  const saved = JSON.parse(sessionStorage.getItem(RECOMMENDATION_SEEN_KEY) || "[]");
  if (Array.isArray(saved)) recommendationSeenIds = saved.filter((id) => /^\d+$/.test(String(id))).slice(-120).map(String);
} catch { /* Storage may be unavailable in a private session. */ }

// The ALAC decoder (worklet transport + WebAssembly glue) is a separate chunk.
// It is preloaded at idle for lossless listeners and on the first lossless
// play otherwise, so the first screen never waits for it.
let alacModule = null;
let alacModuleRequest = null;
function loadAlacModule() {
  alacModuleRequest ||= import("./core/player/alac-worklet").then((module) => {
    alacModule = module;
    return module;
  }, (error) => {
    alacModuleRequest = null;
    throw error;
  });
  return alacModuleRequest;
}

// Resolve the transport before looking up IndexedDB.  The same Apple Music
// lossless track can exist as a legacy FLAC/fMP4 blob and as the packet based
// WASM ALAC stream; those representations must never share a cache key.
function requestedStreamingMode(track) {
  const quality = status.quality || track?.quality || "high";
  return canRequestAlacWorklet({ ...track, quality }) ? "wasm_alac" : "";
}

function loadRecentTracks() {
  try {
    const value = JSON.parse(localStorage.getItem(RECENT_TRACKS_KEY) || "[]");
    return Array.isArray(value) ? value : [];
  } catch {
    return [];
  }
}

recentTracks.value = loadRecentTracks();

function uniqueTracks(items) {
  const seen = new Set();
  return (items || []).filter((track) => {
    const key = trackIdentity(track);
    if (!key || seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function publicRecommendationPlaylists(items) {
  return (items || [])
    .filter((item) => item?.id && !String(item.id).startsWith("pl.u-"))
    .map((item) => ({
      ...item,
      curator: item.resource_type === "albums" ? item.curator : "Apple Music",
    }));
}

// The sidebar lists platforms on desktop; phones pick from the top bar.
const platformOptions = computed(() => platforms.value.length
  ? platforms.value
  : [{ id: status.searchPlatform, label: status.searchPlatform }]);
const platformLabel = computed(
  () => platforms.value.find((item) => item.id === status.searchPlatform)?.label || status.searchPlatform,
);
const cacheSummary = computed(() => cacheStats(cacheItems.value));
const activePlaylist = computed(() =>
  status.activePlaylistId ? playlists.getById(status.activePlaylistId) : null,
);
const themeController = createThemeController(() => status.theme);

watch(() => status.theme, () => themeController.sync(), { immediate: true });

async function runSearch() {
  const keyword = query.value.trim();
  if (!keyword) {
    clearSearchState();
    return;
  }
  searchAbort?.abort?.();
  const controller = new AbortController();
  searchAbort = controller;
  const platform = status.searchPlatform;
  searchLoading.value = true;
  searchError.value = "";
  searchedQuery.value = keyword;
  results.value = [];
  status.activeView = "search";
  status.activePlaylistId = null;
  status.persist();
  try {
    const items = await searchTracks({ platform, q: keyword, limit: 20, signal: controller.signal });
    if (controller.signal.aborted || searchAbort !== controller || platform !== status.searchPlatform) return;
    results.value = items.map((item) => trackFromSearch(item, status.quality));
  } catch (error) {
    if (!controller.signal.aborted) searchError.value = error?.message || "搜索失败";
  } finally {
    if (!controller.signal.aborted) searchLoading.value = false;
  }
}

function clearSearchState() {
  searchAbort?.abort?.();
  searchAbort = null;
  searchLoading.value = false;
  searchError.value = "";
  query.value = "";
  searchedQuery.value = "";
  results.value = [];
}

// Top-level destinations are tabs: switching keeps each one's state (search
// results stay when you visit 主页 and come back), like a native tab bar.
function goHome() {
  status.activeView = "discover";
  status.activePlaylistId = null;
  status.persist();
  if (!recommendationTracks.value.length && !recommendationLoading.value) void loadRecommendations();
}

function navigate(key, id = null) {
  if (key === "discover") return goHome();
  if (key === "playlist") return openPlaylist(id);
  status.activeView = key === "library" ? "playlists" : key;
  status.activePlaylistId = null;
  status.queueOpen = false;
  status.persist();
}

function focusSearch() {
  navigate("search");
  void nextTick(() => document.querySelector(".search-box--main input")?.focus());
}

function handleSearchInput() {
  if (!query.value.trim() && searchedQuery.value) clearSearchState();
}

async function waitForReady(task, { allowAlacStream = false } = {}) {
  let current = task;
  let delay = 120;
  const deadline = Date.now() + 240_000;
  while (Date.now() < deadline) {
    if (current.status === "failed") throw new Error(current.error || "音频准备失败");
    // Stable playback IDs are shared between browsers. A native client can
    // join a task started by a WASM-capable client; later polls may expose the
    // server's ALAC mode again even when the create response cleared it.
    const quality = String(current.quality || task.quality || "").trim().toLowerCase();
    const needsCompletedAudio = !allowAlacStream && (
      current.streaming_mode === "wasm_alac" || ["lossless", "hires", "hi-res"].includes(quality)
    );
    if (current.status === "completed" || (current.status === "streaming" && !needsCompletedAudio)) {
      if (allowAlacStream) return current;
      // Only sanitize this caller's copy. /audio serves the compatible
      // completed derivative; /audio/stream belongs to the packet decoder.
      return {
        ...current,
        streaming_mode: null,
        streaming_url: null,
        streaming_codec: null,
        streaming_protocol: null,
      };
    }
    // Long-poll: the server answers as soon as the task leaves its current
    // status. A server or proxy that answers at once falls back to the
    // tight-then-backing-off polling interval.
    const started = Date.now();
    const next = await getPlayback(current.id, { since: current.status, waitMs: 8000 });
    if (next.status === current.status) {
      const remaining = delay - (Date.now() - started);
      if (remaining > 0) await new Promise((resolve) => window.setTimeout(resolve, remaining));
      delay = Math.min(900, Math.round(delay * 1.32));
    }
    current = next;
  }
  throw new Error("音频准备超时");
}

/** Observe backend completion without coupling packet decoding to the API. */
async function waitForCompletedSource(track, { signal } = {}) {
  if (track?.status !== "streaming") return null;
  const id = track.playback_id || track.id;
  if (!id || signal?.aborted) return null;
  const waitForPoll = () => new Promise((resolve) => {
    const done = () => { window.clearTimeout(timer); signal?.removeEventListener("abort", done); resolve(); };
    const timer = window.setTimeout(done, 1000);
    signal?.addEventListener("abort", done, { once: true });
    if (signal?.aborted) done();
  });
  // Startup has just observed this packet task as streaming. Give the
  // original transport time to finish before asking for the same status
  // again. EOF cancels this wait; native seek recovery still checks at once.
  if (track.streaming_mode === "wasm_alac") await waitForPoll();
  for (let attempt = 0; attempt < 240 && !signal?.aborted; attempt += 1) {
    const started = Date.now();
    try {
      const task = await getPlayback(id, { signal, since: "streaming", waitMs: 15000 });
      if (signal?.aborted) return null;
      if (task.status === "completed") {
        if (track.streaming_mode === "wasm_alac") return task.streaming_mode === "wasm_alac" ? audioUrl(task) : null;
        return audioUrl({ ...task, streaming_mode: null, streaming_url: null });
      }
      if (task.status === "failed") return null;
    } catch {
      if (signal?.aborted) return null;
      // The original audio stream remains usable when a status poll fails.
    }
    // A held long-poll already waited; only an immediate answer needs a pause.
    if (Date.now() - started < 900) await waitForPoll();
  }
  return null;
}

/**
 * Resolve a playable source and persist it in IndexedDB.  The player uses the
 * same function for the active track and for the next-track prefetch path.
 * Prefetching is deliberately silent and waits for the blob write to finish;
 * this means the next transition can use a blob URL immediately instead of
 * opening another network request at the end of the current song.
 */
async function resolveTrackSource(track, { prefetch = false } = {}) {
  const streamingMode = requestedStreamingMode(track);
  const requestTrack = {
    ...track,
    quality: status.quality || track.quality || "high",
    streaming_mode: streamingMode || null,
  };
  const requestKey = cacheKey(requestTrack);
  let pending = inflightTrackResolutions.get(requestKey);
  if (!pending) {
    pending = (async () => {
      const createTask = async () => (await createPlayback({
        platform: track.platform,
        query: track.query || track.title,
        selectedId: track.selected_id || track.source_id,
        quality: requestTrack.quality,
        streamingMode: streamingMode || null,
      })).playback;
      let cached = await getCachedTrack(requestKey).catch(() => null);
      if (hasCachedAudio(cached) && !cacheEntryMatches(cached, requestTrack)) {
        // Remove the old bytes before the metadata merge below. Updating its
        // transport fields alone would make a legacy mismatched Blob appear
        // valid again under the same cache key.
        await deleteCachedTrack(requestKey);
        cached = null;
      }
      if (hasCachedAudio(cached) && cacheEntryMatches(cached, requestTrack)) {
        const readyTrack = cached.track || track;
        void putCachedTrack(requestKey, {
          task: cached.task,
          track: readyTrack,
          byte_size: cached.blob.size,
        }).then(refreshLocalCache);
        return { cached, track: readyTrack, key: requestKey, task: cached.task };
      }
      if (streamingMode === "wasm_alac") {
        // Native FLAC from the older player remains usable, including offline.
        // Keep its own representation/key and validate bytes + browser support
        // before any task POST; metadata-only records still need negotiation.
        const nativeKey = cacheKey({ ...requestTrack, streaming_mode: null });
        const native = await getCachedTrack(nativeKey)
          .then((item) => reusableNativeFlacCache(item, requestTrack)).catch(() => null);
        if (native) {
          void putCachedTrack(nativeKey, native).then(refreshLocalCache).catch(() => {});
          return { cached: native, track: native.track, key: nativeKey, task: native.task };
        }
      }
      if (cached?.task?.id && cacheEntryMatches(cached, requestTrack)) {
        try {
          // A restarted server can restore completed metadata without its
          // optional packet representation. POST renegotiates this browser's
          // capability and lets the server rebuild/upgrade that derivative.
          const joined = streamingMode === "wasm_alac"
            ? await createTask()
            : await getPlayback(cached.task.id);
          const task = await waitForReady(joined, {
            allowAlacStream: streamingMode === "wasm_alac",
          });
          const readyTrack = trackFromPlayback(task, cached.track || requestTrack);
          const actualKey = cacheKey(readyTrack);
          const stored = await putCachedTrack(actualKey, { task, track: readyTrack, byte_size: readyTrack.byte_size });
          if (actualKey !== requestKey) await deleteCachedTrack(requestKey).catch(() => {});
          void refreshLocalCache();
          if (hasCachedAudio(stored) && cacheEntryMatches(stored, readyTrack)) {
            return { cached: stored, track: readyTrack, key: actualKey, task };
          }
          return { source: audioUrl(task), track: readyTrack, key: actualKey, task };
        } catch (error) {
          await deleteCachedTrack(requestKey).catch(() => {});
          // This POST already retried/restored the task by stable song ID.
          // Do not issue an identical second preparation on a failed join.
          if (streamingMode === "wasm_alac") throw error;
        }
      }

      const task = await waitForReady(await createTask(), {
        allowAlacStream: streamingMode === "wasm_alac",
      });
      const readyTrack = trackFromPlayback(task, requestTrack);
      const source = audioUrl(task);
      // The server may decline the optional WASM transport and fall back to
      // its regular derivative. Key by the actual representation returned by
      // the task rather than leaving a normal blob under the wasm key.
      const actualKey = cacheKey(readyTrack);
      const stored = await putCachedTrack(actualKey, { task, track: readyTrack, byte_size: readyTrack.byte_size });
      if (actualKey !== requestKey) await deleteCachedTrack(requestKey).catch(() => {});
      void refreshLocalCache();
      if (hasCachedAudio(stored) && cacheEntryMatches(stored, readyTrack)) {
        return { cached: stored, track: readyTrack, key: actualKey, task };
      }
      return { source, track: readyTrack, key: actualKey, task };
    })();
    inflightTrackResolutions.set(requestKey, pending);
    pending.finally(() => {
      if (inflightTrackResolutions.get(requestKey) === pending) inflightTrackResolutions.delete(requestKey);
    }).catch(() => {});
  }

  const resolved = await pending;
  if (prefetch && resolved?.source && resolved?.task) {
    await cacheAudioBlob(resolved.key, resolved.source, resolved.task, resolved.track, { priority: "auto" });
  }
  if (!prefetch && resolved?.cached) {
    status.setNotice(`${resolved.track?.title || "音乐"} 使用本地缓存`);
  }
  const source = resolved?.cached ? cachedAudioUrl(resolved.cached) : resolved?.source;
  return source ? { source, track: resolved.track } : null;
}

// Cold playback starts from the stream immediately. Once the player has a
// safe buffer it calls this function to finish the active track's complete
// IndexedDB cache; only then may the next-track cache start.
async function resolveCachedTrackSource(track) {
  if (!track) return null;
  // A quality preference change must not reinterpret the still-playing file.
  // Candidate tracks are already normalized by the player to the new quality.
  const request = { ...track, streaming_mode: canRequestAlacWorklet(track) ? "wasm_alac" : null };
  let cached = await getCachedTrack(cacheKey(request)).catch(() => null);
  if (!hasCachedAudio(cached) || !cacheEntryMatches(cached, request)) {
    cached = request.streaming_mode === "wasm_alac"
      ? await getCachedTrack(cacheKey({ ...request, streaming_mode: null }))
        .then((item) => reusableNativeFlacCache(item, request)).catch(() => null)
      : null;
  }
  if (!hasCachedAudio(cached)) return null;
  return { source: cachedAudioUrl(cached), track: { ...(cached.track || track), byte_size: cached.blob.size } };
}

async function cacheActiveTrackSource(track, { signal, blob } = {}) {
  if (!track) return false;
  const key = cacheKey(track);
  const cached = await getCachedTrack(key).catch(() => null);
  if (hasCachedAudio(cached) && cacheEntryMatches(cached, track)) return true;
  let task = cached?.task;
  if (!task) return false;
  if (!blob && task.status !== "completed") {
    // A native stream may already have finished on the server while its
    // original IndexedDB metadata still says streaming. Resolve the current
    // media URL before opening the one full-file cache request.
    const latest = await getPlayback(task.id, { signal });
    if (latest.status === "failed") return false;
    task = track.streaming_mode === "wasm_alac" ? latest : {
      ...latest, streaming_mode: null, streaming_url: null,
      streaming_codec: null, streaming_protocol: null,
    };
    if (cacheKey(task) !== key) return false;
  }
  const source = audioUrl(task);
  if (!source && !blob) return false;
  const stored = await cacheAudioBlob(key, source, task, trackFromPlayback(task, cached.track || track), { signal, blob });
  void refreshLocalCache();
  return hasCachedAudio(stored) && cacheEntryMatches(stored, track);
}

/**
 * Prepare the next queue item without changing the active player state. The
 * callback is passed into the player core so queue transitions can start this
 * work as soon as the current track is ready.
 */
async function prefetchTrackSource(track) {
  if (!track) return false;
  let resolved = null;
  try {
    resolved = await resolveTrackSource(track, { prefetch: true });
    return Boolean(resolved?.source);
  } finally {
    // A prefetch only needs the IndexedDB blob. Do not retain an object URL
    // that is never assigned to the <audio> element.
    if (resolved?.source) releaseCachedAudioUrl(resolved.source);
  }
}

async function loadLyricsForTrack(track) {
  return loadTrackLyrics(track);
}

async function ensureLyrics(track = media.track) {
  if (!track || media.lyricLines.length) return;
  const key = lyricIdentity(track);
  media.beginLyricLoad();
  try {
    const lines = await loadLyricsForTrack(track);
    if (lyricIdentity(media.track) === key) media.setLyric(lines);
  } catch (error) {
    console.warn("[lyrics] load failed", error);
    if (lyricIdentity(media.track) === key) media.clearLyric();
  }
}

async function playTrack(track) {
  await player.playNow(trackForCurrentQuality(track));
}

async function openTrack(track) {
  if (await player.playNow(trackForCurrentQuality(track))) media.expanded = true;
}

async function openQueueTrack(track) {
  if (await player.playAtIndex(queue.findTrackIndex(track))) media.expanded = true;
}

// Every "add to the end" entry point has a matching "play next" one; both
// routes go through these two handlers so feedback stays identical.
function queueTrack(track) {
  const added = player.insertManyToQueue([trackForCurrentQuality(track)], queue.queueLength);
  status.toast(added ? `${track.title} 已加入播放队列末尾` : `${track.title} 已在播放队列中`);
}

function queueTrackNext(track) {
  const arranged = player.queueNextMany([trackForCurrentQuality(track)]);
  status.toast(arranged
    ? `${track.title} 将${status.playIndex >= 0 ? "在下一首播放" : "排在队列最前"}`
    : `${track.title} 正在播放`);
}

function trackForCurrentQuality(track) {
  if (!track) return track;
  const sourceId = track.source_id || track.selected_id || track.track_id ||
    (typeof track.id === "string" && track.id.includes("--")
      ? track.id.split("--").slice(1, -1).join("--")
      : track.id);
  return trackFromSearch(
    {
      platform: track.platform,
      id: sourceId,
      song: track.title,
      singer: track.artist,
      cover: track.cover,
    },
    status.quality,
  );
}

function recordRecentTrack(track) {
  if (!track || !trackIdentity(track)) return;
  const normalized = trackFromSearch(
    {
      platform: track.platform,
      id: track.source_id || track.selected_id || track.track_id || track.id,
      song: track.title,
      singer: track.artist,
      cover: track.cover || track.cover_url,
    },
    status.quality,
  );
  recentTracks.value = uniqueTracks([normalized, ...recentTracks.value]).slice(0, 12);
  localStorage.setItem(RECENT_TRACKS_KEY, JSON.stringify(recentTracks.value));
}

async function loadRecommendations(queryText = "", { refresh = false } = {}) {
  const keyword = String(queryText || "").trim();
  const platform = status.searchPlatform;
  const personal = platform === "applemusic" && !keyword;
  // Revisit older recommendations only after an explicit refresh following an
  // empty response. Never silently issue another request or repeat this batch.
  const revisitEarlier = personal && refresh && recommendationPoolExhausted;
  const excludeIds = revisitEarlier
    ? recommendationTracks.value.map((track) => String(track.source_id || track.selected_id || "")).filter((id) => /^\d+$/.test(id))
    : recommendationSeenIds;
  const searchKeyword = keyword || RECOMMENDATION_SEEDS[recommendationSeedIndex.value];
  if (!searchKeyword && !personal) return;
  recommendationAbort?.abort();
  const controller = new AbortController();
  recommendationAbort = controller;
  const requestId = ++recommendationRequestId;
  recommendationLoading.value = true;
  recommendationError.value = "";
  try {
    let items;
    let personalResult = false;
    let nextPlaylists = [];
    let nextTags = [];
    if (personal) {
      const home = await getRecommendationHome({ platform, limit: 40, refresh, exclude: excludeIds, signal: controller.signal });
      items = home?.tracks || [];
      nextPlaylists = publicRecommendationPlaylists(home?.playlists);
      nextTags = Array.isArray(home?.tags) ? home.tags : [];
      personalResult = Array.isArray(items) && items.length > 0;
    } else {
      items = await searchTracks({ platform, q: searchKeyword, limit: 24, signal: controller.signal });
    }
    if (requestId !== recommendationRequestId || controller.signal.aborted || platform !== status.searchPlatform) return;
    const nextTracks = uniqueTracks(
      (Array.isArray(items) ? items : [])
        .map((item) => trackFromSearch(item, status.quality))
        .filter((track) => !playlists.less.includes(trackIdentity(track))),
    ).slice(0, 12);
    // Keep the previous batch usable if the upstream pool is temporarily
    // exhausted instead of replacing the whole feed with an empty page.
    if (!nextTracks.length) {
      recommendationPoolExhausted = personal;
      recommendationError.value = "暂时没有更多新推荐，再换一批可以重新探索较早的歌曲。";
      return;
    }
    recommendationPoolExhausted = false;
    recommendationPersonal.value = personalResult;
    recommendationPlaylists.value = nextPlaylists;
    recommendationTags.value = nextTags;
    recommendationTracks.value = nextTracks;
    if (personal) {
      const ids = nextTracks.map((track) => String(track.source_id || track.selected_id || "")).filter((id) => /^\d+$/.test(id));
      recommendationSeenIds = [...new Set([...excludeIds, ...ids])].slice(-120);
      try { sessionStorage.setItem(RECOMMENDATION_SEEN_KEY, JSON.stringify(recommendationSeenIds)); } catch { /* Optional history. */ }
    }
  } catch (error) {
    if (requestId === recommendationRequestId && !controller.signal.aborted) recommendationError.value = error?.message || "推荐暂时不可用，请稍后重试。";
  } finally {
    if (requestId === recommendationRequestId) recommendationLoading.value = false;
  }
}

function refreshRecommendations() {
  recommendationSeedIndex.value = (recommendationSeedIndex.value + 1) % RECOMMENDATION_SEEDS.length;
  recommendationMood.value = "";
  recommendationMoodLabel.value = "";
  void loadRecommendations("", { refresh: true });
}

function chooseRecommendationMood(item) {
  recommendationMood.value = item.id;
  recommendationMoodLabel.value = item.label || item.id;
  if (item.playlist_ids?.length) {
    const playlist = recommendationPlaylists.value.find((entry) => item.playlist_ids.includes(entry.id));
    if (playlist) {
      void loadRecommendationPlaylist(playlist);
      return;
    }
  }
  if (status.searchPlatform === "applemusic") return;
  if (!item.query) return;
  const index = RECOMMENDATION_SEEDS.indexOf(item.query);
  recommendationSeedIndex.value = index >= 0 ? index : recommendationSeedIndex.value;
  void loadRecommendations(item.query);
}

async function playRecommendations() {
  if (!recommendationTracks.value.length) return;
  await player.playFrom(recommendationTracks.value.map(trackForCurrentQuality), 0, {
    id: "recommendation-session",
    name: recommendationMoodLabel.value ? `随便听 · ${recommendationMoodLabel.value}` : "随便听 · 为你生成",
    type: "recommendation",
  });
}

// Browsing a recommendation never mutates playback. Only explicit detail
// actions below can append, replace, or schedule the collection.
async function openRecommendedPlaylist(playlist) {
  if (!playlist?.id) return;
  const local = playlists.getById(playlist.id);
  if (local) {
    openPlaylist(local.id);
    return;
  }
  closePlaylistMenu();
  if (status.activeView !== "recommendation") {
    recommendationReturnScroll = window.scrollY;
    recommendationReturnFocus = document.activeElement;
    recommendationReturnId = playlist.id;
    pushUiHistory("recommendation");
    recommendationHistoryActive = true;
  }
  recommendationDetail.value = {
    id: playlist.id,
    name: playlist.name || "推荐歌单",
    cover: playlist.cover || "",
    resource_type: playlist.resource_type || "playlists",
    url: playlist.url || "",
    platform: "applemusic",
    type: "recommendation",
    tracks: [],
  };
  recommendationDetailError.value = "";
  playlistActionNotice.value = "";
  recommendationDetailLoading.value = true;
  pendingPageScroll = 0;
  status.activeView = "recommendation";
  status.persist();
  await loadRecommendedDetail();
}

async function loadRecommendedDetail() {
  const playlist = recommendationDetail.value;
  if (!playlist) return;
  const version = ++recommendationDetailVersion;
  recommendationDetailLoading.value = true;
  recommendationDetailError.value = "";
  try {
    // Full detail must not inherit the feed's 50-track preview limit.
    const tracks = await resolveRecommendationPlaylist(playlist);
    if (version !== recommendationDetailVersion || status.activeView !== "recommendation") return;
    recommendationDetail.value = { ...playlist, tracks: uniqueTracks(tracks) };
  } catch (error) {
    if (version === recommendationDetailVersion && status.activeView === "recommendation") {
      recommendationDetailError.value = error?.message || "推荐歌单暂时无法打开，请重试";
    }
  } finally {
    if (version === recommendationDetailVersion) recommendationDetailLoading.value = false;
  }
}

function leaveRecommendedPlaylist() {
  recommendationDetailVersion += 1;
  pendingPageScroll = recommendationReturnScroll;
  status.activeView = "discover";
  status.persist();
  if (recommendationHistoryActive && !closingFromPopstate && window.history.state?.musicUi === "recommendation") {
    backUiHistory();
  }
  recommendationHistoryActive = false;
}

function handlePageEntered(element) {
  if (pendingPageScroll == null) return;
  window.scrollTo({ top: pendingPageScroll, behavior: "instant" });
  pendingPageScroll = null;
  if (status.activeView === "recommendation") {
    const heading = element.querySelector("h1");
    heading?.setAttribute("tabindex", "-1");
    heading?.focus({ preventScroll: true });
  } else if (status.activeView === "discover") {
    const card = element.querySelector(`[data-recommendation-id="${CSS.escape(recommendationReturnId)}"]`);
    const target = card || (recommendationReturnFocus?.isConnected ? recommendationReturnFocus : null) || element.querySelector("h1");
    if (target?.tagName === "H1") target.setAttribute("tabindex", "-1");
    target?.focus({ preventScroll: true });
  }
}

async function resolveRecommendationPlaylist(playlist, { preview = false } = {}) {
  if (!playlist) return [];
  if (playlist.tracks?.length) return playlist.tracks;
  const collection = await resolveCollection({
    platform: "applemusic",
    url: playlist.url || (playlist.resource_type === "albums" ? `album:${playlist.id}` : playlist.id),
    ...(preview ? { limit: 50 } : {}),
  });
  return (collection?.items || []).map((item) => trackFromSearch({
    platform: item.platform || "applemusic",
    id: item.id,
    song: item.song,
    singer: item.singer,
    cover: item.cover,
  }, status.quality));
}

async function loadRecommendationPlaylist(playlist) {
  recommendationAbort?.abort();
  const requestId = ++recommendationRequestId;
  recommendationLoading.value = true;
  recommendationError.value = "";
  try {
    const tracks = await resolveRecommendationPlaylist(playlist, { preview: true });
    if (requestId === recommendationRequestId) recommendationTracks.value = uniqueTracks(tracks).slice(0, 24);
  } catch (error) {
    if (requestId === recommendationRequestId) status.toast(error?.message || "推荐歌单暂时无法打开");
  } finally {
    if (requestId === recommendationRequestId) recommendationLoading.value = false;
  }
}

async function replacePlaylistQueue(playlist) {
  if (playlistActionBusy.value || !playlist?.tracks?.length) return;
  const version = ++playlistActionVersion;
  playlistActionBusy.value = true;
  playlistActionNotice.value = "";
  status.queueOpen = false;
  try {
    await player.playFrom(playlist.tracks.map(trackForCurrentQuality), 0, {
      id: playlist.id,
      name: playlist.name,
      type: playlist.type || "playlist",
      resource_type: playlist.resource_type,
    });
    if (version === playlistActionVersion) {
      playlistActionNotice.value = `队列已替换为“${playlist.name}”的 ${queue.queueLength} 首歌曲`;
    }
  } finally {
    if (version === playlistActionVersion) playlistActionBusy.value = false;
  }
}

function appendPlaylistQueue(playlist) {
  if (!playlist?.tracks?.length) return;
  const added = player.insertManyToQueue(playlist.tracks.map(trackForCurrentQuality), queue.queueLength);
  playlistActionNotice.value = added ? `已将“${playlist.name}”的 ${added} 首歌曲加入队列末尾` : "歌单中的歌曲已在当前队列";
  status.toast(playlistActionNotice.value);
}

function nextPlaylistQueue(playlist) {
  if (!playlist?.tracks?.length) return;
  const wasRepeatingOne = status.repeatMode === "one";
  const arranged = player.queueNextMany(playlist.tracks.map(trackForCurrentQuality));
  playlistActionNotice.value = arranged
    ? `已安排 ${arranged} 首歌曲${status.playIndex >= 0 ? "接下来播放" : "到队列最前"}${wasRepeatingOne ? "，已关闭单曲循环" : ""}`
    : "歌单中只有当前歌曲，无需重复加入";
  status.toast(playlistActionNotice.value);
}

function queueRecommendations() {
  const tracks = recommendationTracks.value.map(trackForCurrentQuality);
  if (!tracks.length) return;
  const added = player.insertManyToQueue(tracks, queue.queueLength);
  status.toast(added ? `已将 ${added} 首推荐加入播放队列末尾` : "这些推荐歌曲已在播放队列中");
}

function queueRecommendationsNext() {
  const tracks = recommendationTracks.value.map(trackForCurrentQuality);
  if (!tracks.length) return;
  const arranged = player.queueNextMany(tracks);
  status.toast(arranged ? `已安排 ${arranged} 首推荐接下来播放` : "这些推荐歌曲已在播放");
}

function likeRecommendation(track) {
  const key = trackIdentity(track);
  if (!key) return;
  // Liked / less lists live in the playlist store, so they sync with an account.
  playlists.addTaste("liked", key);
  recordRecentTrack(track);
  status.toast(`已记录喜欢，之后会更多保留类似音乐：${track.title}`);
}

function suggestLessRecommendation(track) {
  const key = trackIdentity(track);
  if (!key) return;
  playlists.addTaste("less", key);
  recommendationTracks.value = recommendationTracks.value.filter((item) => trackIdentity(item) !== key);
  status.toast("会减少类似推荐");
}

function saveResultsAsPlaylist() {
  if (!results.value.length) return;
  const playlist = playlists.create({
    name: searchedQuery.value || "搜索结果",
    tracks: results.value.map(trackForCurrentQuality),
    platform: status.searchPlatform,
  });
  status.activeView = "playlists";
  status.activePlaylistId = playlist.id;
  status.persist();
}

async function playAllResults() {
  if (!results.value.length) return;
  await player.playFrom(results.value, 0);
}

function openImportDialog() {
  importOpen.value = true;
}

function finishImport(playlistId) {
  importOpen.value = false;
  status.activeView = "playlists";
  status.activePlaylistId = playlistId;
  status.persist();
}

function openPlaylist(id) {
  closePlaylistMenu();
  playlistActionNotice.value = "";
  status.activeView = "playlists";
  status.activePlaylistId = id;
  status.persist();
}

function closePlaylistMenu() {
  playlistMenu.value = { open: false, x: 0, y: 0, playlistId: "" };
  songMenu.value = { open: false, x: 0, y: 0, track: null, playlistId: "", recommend: false };
  feedMenu.value = { open: false, x: 0, y: 0 };
}

function menuPosition(event, width, height) {
  return {
    x: Math.max(8, Math.min(event?.clientX || 8, window.innerWidth - width - 8)),
    y: Math.max(8, Math.min(event?.clientY || 8, window.innerHeight - height - 8)),
  };
}

// One menu for every song row: right-click on desktop, the "⋯" button
// everywhere (iOS has no long-press contextmenu).
function openSongMenu({ event, track, recommend = false }, playlistId = "") {
  event?.preventDefault?.();
  event?.stopPropagation?.();
  if (!track) return;
  const targets = playlists.items.filter((item) => item.id !== playlistId).length;
  closePlaylistMenu();
  songMenu.value = {
    open: true,
    ...menuPosition(event, 230, 190 + Math.min(targets, 6) * 38 + (playlistId ? 44 : 0) + (recommend ? 86 : 0)),
    track,
    playlistId,
    recommend,
  };
}

function openFeedMenu(event) {
  event?.stopPropagation?.();
  closePlaylistMenu();
  feedMenu.value = { open: true, ...menuPosition(event, 210, 100) };
}

function runFeedMenu(action) {
  closePlaylistMenu();
  if (action === "next") queueRecommendationsNext();
  if (action === "queue") queueRecommendations();
}

const songMenuPlaylists = computed(() => playlists.items
  .filter((item) => item.id !== songMenu.value.playlistId)
  .slice(0, 6));

function runSongMenu(action, targetId = "") {
  const { track, playlistId } = songMenu.value;
  closePlaylistMenu();
  if (!track) return;
  if (action === "play") void playTrack(track);
  if (action === "next") queueTrackNext(track);
  if (action === "queue") queueTrack(track);
  if (action === "like") likeRecommendation(track);
  if (action === "less") suggestLessRecommendation(track);
  if (action === "add") {
    const playlist = playlists.getById(targetId);
    const before = playlist?.tracks.length || 0;
    const merged = playlists.merge(targetId, [trackForCurrentQuality(track)]);
    if (merged) status.toast(merged.tracks.length > before ? `已添加到“${merged.name}”` : `“${merged.name}”中已有这首歌`);
  }
  if (action === "create") {
    const playlist = playlists.create({ name: track.title, tracks: [trackForCurrentQuality(track)], platform: track.platform });
    status.toast(`已创建歌单“${playlist.name}”`);
  }
  if (action === "remove" && playlistId) {
    const playlist = playlists.removeTrack(playlistId, track);
    if (playlist) status.toast(`已从“${playlist.name}”移除`);
  }
}

function openPlaylistMenu(event, playlist) {
  event?.preventDefault?.();
  event?.stopPropagation?.();
  closePlaylistMenu();
  playlistMenu.value = { open: true, ...menuPosition(event, 210, 270), playlistId: playlist.id };
}

function renamePlaylist(id) {
  const playlist = playlists.getById(id);
  if (!playlist) return;
  const name = window.prompt("重命名歌单", playlist.name)?.trim();
  if (name && name !== playlist.name) {
    playlists.rename(id, name);
    status.toast(`已将歌单重命名为“${name}”`);
  }
}

function deletePlaylist(id) {
  const playlist = playlists.getById(id);
  if (!playlist) return;
  if (!window.confirm(`确定删除歌单“${playlist.name}”吗？歌单中的歌曲不会被删除。`)) return;
  playlists.remove(id);
  if (status.activePlaylistId === id) leavePlaylist();
  status.toast(`已删除歌单“${playlist.name}”`);
}

async function playPlaylist(id) {
  await replacePlaylistQueue(playlists.getById(id));
}

function queuePlaylist(id) {
  appendPlaylistQueue(playlists.getById(id));
}

function runPlaylistMenu(action) {
  const id = playlistMenu.value.playlistId;
  closePlaylistMenu();
  if (!id) return;
  if (action === "open") openPlaylist(id);
  if (action === "play") void playPlaylist(id);
  if (action === "queue") queuePlaylist(id);
  if (action === "next") nextPlaylistQueue(playlists.getById(id));
  if (action === "rename") renamePlaylist(id);
  if (action === "delete") deletePlaylist(id);
}

function handleDocumentPointerDown() {
  closePlaylistMenu();
}

function handlePageHide() {
  account.publishPlayback({ flush: true });
}

function handleVisibility() {
  if (document.visibilityState === "visible") account.resume();
  else account.publishPlayback({ flush: true });
}

function handleAudioGesture() {
  player.prepareAudio();
}

function handleDocumentKeydown(event) {
  if (event.key === "Escape") closePlaylistMenu();
  if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || importOpen.value) return;
  // Leave keys alone wherever they already mean something (typing, buttons,
  // the seek slider, menus).
  const interactive = event.target?.closest?.("input, textarea, select, button, a, [contenteditable], [role='menu'], [role='slider']");
  if (interactive) return;
  if (event.code === "Space") {
    event.preventDefault();
    if (!event.repeat) player.togglePlay();
  } else if (event.key === "/") {
    event.preventDefault();
    focusSearch();
  }
}

function leavePlaylist() {
  status.activePlaylistId = null;
  status.persist();
}

function deleteActivePlaylist() {
  if (activePlaylist.value) deletePlaylist(activePlaylist.value.id);
}

async function refreshLocalCache() {
  cacheLoading.value = true;
  try {
    const items = await listCachedTracks();
    if (!appUnmounted) cacheItems.value = items;
  } finally {
    if (!appUnmounted) cacheLoading.value = false;
  }
}

async function removeCacheItem(item) {
  await deleteCachedTrack(item.key);
  await refreshLocalCache();
}

async function clearLocalCache() {
  await clearCache();
  await refreshLocalCache();
}

function syncUiPlaybackTime() {
  player.tickPlaybackTime();
}

function pushUiHistory(kind) {
  // These entries represent overlays on the same page, not navigation. Browser
  // restoration would otherwise rewind scrolling when an overlay is closed.
  window.history.scrollRestoration = "manual";
  window.history.pushState({ ...(window.history.state || {}), musicUi: kind }, "", window.location.href);
}

function backUiHistory() {
  // The button already closed its own UI. Its eventual popstate must not
  // close the underlying player/detail a second time.
  pendingUiHistoryBack += 1;
  window.history.back();
}

function syncFullPlayerHistory(expanded) {
  document.body.classList.toggle("is-full-player-open", expanded);
  if (expanded && !fullPlayerHistoryActive) {
    pushUiHistory("full-player");
    fullPlayerHistoryActive = true;
  } else if (!expanded && fullPlayerHistoryActive) {
    fullPlayerHistoryActive = false;
    if (!closingFromPopstate && window.history.state?.musicUi === "full-player") {
      backUiHistory();
    }
  }
}

function syncQueueHistory(open) {
  if (open && !queueHistoryActive) {
    pushUiHistory("queue");
    queueHistoryActive = true;
  } else if (!open && queueHistoryActive) {
    queueHistoryActive = false;
    if (!closingFromPopstate && window.history.state?.musicUi === "queue") {
      backUiHistory();
    }
  }
}

function handlePopstate(event) {
  if (pendingUiHistoryBack) {
    pendingUiHistoryBack -= 1;
    return;
  }
  closingFromPopstate = true;
  if (status.queueOpen) {
    status.queueOpen = false;
    queueHistoryActive = false;
  } else if (media.expanded) {
    media.expanded = false;
    fullPlayerHistoryActive = false;
  } else if (status.activeView === "recommendation" && event.state?.musicUi !== "recommendation") {
    leaveRecommendedPlaylist();
  }
  closingFromPopstate = false;
}

watch(
  () => status.activePlaylistId,
  () => {
    playlistActionVersion += 1;
    playlistActionBusy.value = false;
    playlistActionNotice.value = "";
  },
);

watch(
  () => status.activeView,
  (view, previous) => {
    playlistActionVersion += 1;
    playlistActionBusy.value = false;
    playlistActionNotice.value = "";
    if (previous !== "recommendation" || view === "recommendation") return;
    recommendationDetailVersion += 1;
    recommendationDetailLoading.value = false;
    recommendationDetailError.value = "";
    recommendationDetail.value = null;
    if (recommendationHistoryActive && window.history.state?.musicUi === "recommendation") {
      window.history.replaceState({ ...window.history.state, musicUi: null }, "");
    }
    recommendationHistoryActive = false;
  },
);

watch(
  () => status.quality,
  () => {
    player.onQualityChanged();
    results.value = results.value.map((track) =>
      trackFromSearch(
        {
          platform: track.platform,
          id: track.source_id || track.selected_id,
          song: track.title,
          singer: track.artist,
          cover: track.cover,
        },
        status.quality,
      ),
    );
    status.persist();
  },
);

watch(
  () => status.searchPlatform,
  () => {
    recommendationAbort?.abort();
    recommendationPoolExhausted = false;
    recommendationRequestId += 1;
    recommendationLoading.value = false;
    recommendationTracks.value = [];
    recommendationPlaylists.value = [];
    recommendationTags.value = [];
    recommendationMood.value = "";
    recommendationMoodLabel.value = "";
    recommendationError.value = "";
    status.persist();
    if (searchedQuery.value) void runSearch();
    void loadRecommendations();
  },
);

watch(
  () => media.expanded,
  (expanded) => {
    if (expanded) {
      fullPlayerMounted.value = true;
      void ensureLyrics();
    }
  },
);

watch(
  () => media.track,
  (track) => recordRecentTrack(track),
);

onMounted(() => {
  window.addEventListener("popstate", handlePopstate);
  document.addEventListener("pointerdown", handleDocumentPointerDown);
  document.addEventListener("keydown", handleDocumentKeydown);
  document.addEventListener("click", handleAudioGesture, true);
  document.addEventListener("keydown", handleAudioGesture, true);
  player.initPlayer({
    trustedMediaOrigins: trustedMediaOrigins(),
    audio: audioRef.value,
    resolveTrackSource,
    resolveCachedTrackSource,
    outputFactory: { prepare: prepareOutput, prepareSession: preparePlaybackSession, createNative: createNativeOutput },
    mediaSessionFactory: createMediaSessionController,
    lifecycleFactory: createPlaybackLifecycle,
    lookupLoudness: getCachedLoudness,
    warmLoudness,
    waitForCompletedSource,
    prefetchTrackSource,
    cacheActiveTrackSource,
    loadLyricsForTrack,
    getContinuationRecommendations,
    // The backend opts in per task with streaming_mode="wasm_alac". Native
    // <audio> remains the default for every existing task and browser.
    alacPlayerFactory: {
      canHandle: (track) => canUseAlacWorklet(track),
      canRequest: (track) => canRequestAlacWorklet(track),
      isLoaded: () => Boolean(alacModule),
      load: loadAlacModule,
      warm: (sampleRate) => {
        prepareOutput(sampleRate);
        void loadAlacModule().catch(() => {});
      },
      create: () => alacModule.createAlacWorkletPlayer({
        onError: (error) => status.toast(error?.message || "ALAC 解码失败"),
      }),
    },
  });
  // These three reads are independent. In particular the recommendation feed
  // already knows the selected platform from local preferences; waiting for
  // the platform labels adds a full network round trip to its first render.
  // The playback clock must also start if IndexedDB is slow or unavailable.
  syncUiPlaybackTime();
  uiSyncTimer = window.setInterval(syncUiPlaybackTime, UI_SYNC_INTERVAL_MS);
  void getPlatforms().catch(() => [
    { id: "applemusic", label: "Apple Music" },
    { id: "tencent", label: "QQ音乐" },
    { id: "netease", label: "网易云音乐" },
  ]).then((items) => { if (!appUnmounted) platforms.value = items; });
  void loadRecommendations();
  void account.init();
  window.addEventListener("pagehide", handlePageHide);
  document.addEventListener("visibilitychange", handleVisibility);
  // Keep other devices' hand-off position fresh while this one plays.
  handoffTimer = window.setInterval(() => { if (status.isPlaying) account.publishPlayback(); }, 15_000);
  if (canRequestAlacWorklet({ platform: "applemusic", quality: status.quality })) {
    const preload = () => void loadAlacModule().catch(() => {});
    if ("requestIdleCallback" in window) window.requestIdleCallback(preload, { timeout: 4000 });
    else window.setTimeout(preload, 2000);
  }
  void refreshLocalCache().catch((error) => {
    if (!appUnmounted) console.warn("[cache] local cache listing unavailable", error);
  });
});

onBeforeUnmount(() => {
  themeController.dispose();
  window.removeEventListener("pagehide", handlePageHide);
  document.removeEventListener("visibilitychange", handleVisibility);
  window.clearInterval(handoffTimer);
  appUnmounted = true;
  player.destroyPlayer();
  window.removeEventListener("popstate", handlePopstate);
  document.removeEventListener("pointerdown", handleDocumentPointerDown);
  document.removeEventListener("keydown", handleDocumentKeydown);
  document.removeEventListener("click", handleAudioGesture, true);
  document.removeEventListener("keydown", handleAudioGesture, true);
  if (uiSyncTimer) window.clearInterval(uiSyncTimer);
  searchAbort?.abort?.();
  recommendationAbort?.abort();
  recommendationRequestId += 1;
  recommendationDetailVersion += 1;
  document.body.classList.remove("is-full-player-open");
});

watch(
  () => media.expanded,
  syncFullPlayerHistory,
  { immediate: true },
);

watch(
  () => status.queueOpen,
  syncQueueHistory,
);

// Report this device's queue for hand-off whenever what plays changes.
watch(
  () => [status.playIndex, status.currentTrack && trackIdentity(status.currentTrack), status.isPlaying, queue.queueLength],
  () => account.publishPlayback(),
);
</script>

<template>
  <div class="app" :class="{ 'app--has-track': Boolean(media.track) }">
    <Sidebar @navigate="navigate" @playlist-contextmenu="({ event, playlist }) => openPlaylistMenu(event, playlist)" />
    <main class="main">
      <Transition name="page" mode="out-in" @after-enter="handlePageEntered">
      <section v-if="status.activeView === 'discover'" class="content" key="discover">
        <PageHeader title="主页" />
        <HandoffCard />
        <RecommendationFeed
          :tracks="recommendationTracks"
          :recent-tracks="recentTracks"
          :playlists="playlists.items"
          :recommendation-playlists="recommendationPlaylists"
          :recommendation-tags="recommendationTags"
          :loading="recommendationLoading"
          :error="recommendationError"
          :platform="status.searchPlatform"
          :platform-label="platformLabel"
          :mood="recommendationMood"
          :personalized="recommendationPersonal"
          @play="playTrack"
          @open="openTrack"
          @menu="openSongMenu({ ...$event, recommend: true })"
          @more="openFeedMenu"
          @refresh="refreshRecommendations"
          @mood="chooseRecommendationMood"
          @play-all="playRecommendations"
          @playlist="openRecommendedPlaylist"
        />
      </section>

      <section v-else-if="status.activeView === 'search'" class="content" key="search">
        <PageHeader title="搜索" />
        <form class="search-box search-box--main" role="search" @submit.prevent="runSearch">
          <Search :size="20" />
          <input v-model="query" type="search" enterkeyhint="search" aria-label="搜索歌曲、艺人或音乐链接" placeholder="歌曲、艺人或音乐链接" @input="handleSearchInput" @keydown.esc.prevent="clearSearchState" />
          <button v-if="query || searchedQuery" class="search-box__clear" type="button" aria-label="清空搜索" @click="clearSearchState"><X :size="18" /></button>
          <button type="submit" :disabled="searchLoading">搜索</button>
        </form>
        <div class="search-scopes" role="radiogroup" aria-label="搜索平台">
          <button
            v-for="item in platformOptions"
            :key="item.id"
            type="button"
            role="radio"
            class="search-scope"
            :class="{ 'is-active': status.searchPlatform === item.id }"
            :aria-checked="status.searchPlatform === item.id"
            @click="status.searchPlatform = item.id; status.persist()"
          >{{ item.label }}</button>
        </div>
        <div v-if="searchError" class="error-banner">{{ searchError }}</div>
        <template v-if="searchedQuery">
          <div class="search-results__head">
            <p>{{ searchLoading ? "正在搜索…" : `${results.length} 首` }}</p>
            <div>
              <button class="ghost-action" :disabled="!results.length" @click="saveResultsAsPlaylist">存为歌单</button>
              <button class="primary-action" :disabled="!results.length" @click="playAllResults"><Play :size="15" />播放</button>
            </div>
          </div>
          <SongList
            :items="results"
            :loading="searchLoading"
            empty-text="没有找到匹配的歌曲"
            @play="playTrack"
            @queue="queueTrack"
            @queue-next="queueTrackNext"
            @open="openTrack"
            @menu="openSongMenu"
          />
        </template>
        <p v-else class="search-hint">在 {{ platformLabel }} 中搜索</p>
      </section>

      <section v-else-if="status.activeView === 'recommendation' && recommendationDetail" class="content" :key="`recommendation:${recommendationDetail.id}`">
        <PlaylistDetail
          :playlist="recommendationDetail"
          :loading="recommendationDetailLoading"
          :error="recommendationDetailError"
          :quality="status.quality"
          :shuffle="status.shuffleMode === 'on'"
          :queue-length="queue.queueLength"
          :current-track="status.currentTrack"
          :busy="playlistActionBusy"
          :notice="playlistActionNotice"
          @back="leaveRecommendedPlaylist"
          @retry="loadRecommendedDetail"
          @replace="replacePlaylistQueue(recommendationDetail)"
          @append="appendPlaylistQueue(recommendationDetail)"
          @next="nextPlaylistQueue(recommendationDetail)"
          @play="playTrack"
          @queue="queueTrack"
          @queue-next="queueTrackNext"
          @open="openTrack"
          @menu="openSongMenu"
        />
      </section>

      <section v-else-if="status.activeView === 'queue'" class="content" key="queue">
        <button class="page-back page-back--phone" type="button" @click="navigate('library')"><ChevronLeft :size="20" />资料库</button>
        <PageHeader title="播放队列" />
        <HandoffCard />
        <QueuePanel embedded />
      </section>

      <section v-else-if="status.activeView === 'playlists'" class="content" key="playlists">
        <PlaylistDetail
          v-if="activePlaylist"
          :playlist="activePlaylist"
          local
          :quality="status.quality"
          :shuffle="status.shuffleMode === 'on'"
          :queue-length="queue.queueLength"
          :current-track="status.currentTrack"
          :busy="playlistActionBusy"
          :notice="playlistActionNotice"
          @back="leavePlaylist"
          @replace="replacePlaylistQueue(activePlaylist)"
          @append="appendPlaylistQueue(activePlaylist)"
          @next="nextPlaylistQueue(activePlaylist)"
          @delete="deleteActivePlaylist"
          @play="playTrack"
          @queue="queueTrack"
          @queue-next="queueTrackNext"
          @open="openTrack"
          @menu="openSongMenu($event, activePlaylist.id)"
        />
        <template v-else>
          <PageHeader title="资料库">
            <button class="ghost-action page-header__import" type="button" aria-label="导入歌单" @click="openImportDialog"><Plus :size="17" /><span>导入歌单</span></button>
          </PageHeader>
          <nav class="library-links" aria-label="资料库">
            <button type="button" @click="navigate('queue')"><Disc3 :size="22" />播放队列<ChevronRight :size="18" /></button>
            <button type="button" @click="navigate('cache')"><HardDrive :size="22" />本地缓存<ChevronRight :size="18" /></button>
          </nav>
          <div class="section-head library-head">
            <h2 class="section-title">歌单</h2>
          </div>
          <div v-if="!playlists.items.length" class="list-state">还没有歌单，点“导入歌单”从音乐平台导入</div>
          <TransitionGroup v-else name="playlist" tag="div" class="playlist-grid">
            <div
              v-for="playlist in playlists.items"
              :key="playlist.id"
              class="playlist-card-wrap"
              @contextmenu.prevent="openPlaylistMenu($event, playlist)"
            >
              <button class="playlist-card" @click="openPlaylist(playlist.id)">
                <span class="playlist-card__cover" :class="{ 'playlist-card__cover--empty': !playlist.cover }">
                  <img v-if="playlist.cover" :src="playlist.cover" alt="" loading="lazy" />
                  <ListMusic v-else :size="32" />
                </span>
                <strong class="playlist-card__name">{{ playlist.name }}</strong>
                <small class="playlist-card__meta">{{ playlist.tracks.length }} 首</small>
              </button>
              <button
                class="playlist-card__menu"
                type="button"
                title="歌单操作"
                aria-label="打开歌单操作菜单"
                @click="openPlaylistMenu($event, playlist)"
              >
                <MoreHorizontal :size="18" />
              </button>
            </div>
          </TransitionGroup>
        </template>
      </section>

      <section v-else class="content" key="cache">
        <button class="page-back page-back--phone" type="button" @click="navigate('library')"><ChevronLeft :size="20" />资料库</button>
        <PageHeader title="本地缓存">
          <button class="ghost-action" type="button" aria-label="刷新" title="刷新" @click="refreshLocalCache"><RefreshCcw :size="16" /></button>
          <button class="ghost-action" type="button" :disabled="!cacheItems.length" @click="clearLocalCache"><Trash2 :size="16" />清空</button>
        </PageHeader>
        <div class="cache-meter">
          <span :style="{ width: `${cacheSummary.ratio * 100}%` }"></span>
        </div>
        <p class="cache-summary">此浏览器已用 {{ formatSize(cacheSummary.used) }} / {{ formatSize(cacheSummary.limit) }}</p>
        <div v-if="cacheLoading" class="list-state">正在读取…</div>
        <div v-else class="cache-list">
          <TransitionGroup v-if="cacheItems.length" name="cache" tag="div" class="cache-list__rows">
            <div v-for="item in cacheItems" :key="item.key" class="cache-item">
              <span>
                <strong>{{ item.track?.title || item.task?.title || item.key }}</strong>
                <small>
                  {{ item.track?.artist || item.task?.artist || item.task?.platform }}
                  · {{ item.blob?.size ? formatSize(item.blob.size) : "仅任务记录" }}
                </small>
              </span>
              <button class="mini-button" title="删除" aria-label="删除缓存" @click="removeCacheItem(item)">
                <Trash2 :size="16" />
              </button>
            </div>
          </TransitionGroup>
          <div v-else class="list-state">暂无缓存</div>
        </div>
      </section>
      </Transition>
    </main>

    <Transition name="context">
      <div
        v-if="playlistMenu.open"
        class="context-menu playlist-context-menu"
        :style="{ left: `${playlistMenu.x}px`, top: `${playlistMenu.y}px` }"
        role="menu"
        @pointerdown.stop
        @contextmenu.prevent
      >
        <button type="button" role="menuitem" @click="runPlaylistMenu('open')"><FolderOpen :size="16" />打开歌单</button>
        <button type="button" role="menuitem" @click="runPlaylistMenu('play')"><Play :size="16" />替换队列并播放</button>
        <button type="button" role="menuitem" @click="runPlaylistMenu('queue')"><ListPlus :size="16" />加入播放队列</button>
        <button type="button" role="menuitem" @click="runPlaylistMenu('next')"><ListStart :size="16" />下一首播放</button>
        <button type="button" role="menuitem" @click="runPlaylistMenu('rename')"><Pencil :size="16" />重命名</button>
        <div class="context-menu__divider"></div>
        <button type="button" role="menuitem" class="is-danger" @click="runPlaylistMenu('delete')"><Trash2 :size="16" />删除歌单</button>
      </div>
    </Transition>

    <Transition name="context">
      <div
        v-if="songMenu.open"
        class="context-menu song-context-menu"
        :style="{ left: `${songMenu.x}px`, top: `${songMenu.y}px` }"
        role="menu"
        :aria-label="songMenu.track?.title"
        @pointerdown.stop
        @contextmenu.prevent
      >
        <p class="context-menu__title">{{ songMenu.track?.title }}</p>
        <button type="button" role="menuitem" @click="runSongMenu('play')"><Play :size="16" />立即播放</button>
        <button type="button" role="menuitem" @click="runSongMenu('next')"><ListStart :size="16" />下一首播放</button>
        <button type="button" role="menuitem" @click="runSongMenu('queue')"><ListPlus :size="16" />加入播放队列</button>
        <template v-if="songMenu.recommend">
          <div class="context-menu__divider"></div>
          <button type="button" role="menuitem" @click="runSongMenu('like')"><Heart :size="16" />喜欢</button>
          <button type="button" role="menuitem" @click="runSongMenu('less')"><Ban :size="16" />少推荐类似音乐</button>
        </template>
        <div class="context-menu__divider"></div>
        <p class="context-menu__label">添加到歌单</p>
        <button v-for="item in songMenuPlaylists" :key="item.id" type="button" role="menuitem" @click="runSongMenu('add', item.id)"><ListMusic :size="16" /><span>{{ item.name }}</span></button>
        <button type="button" role="menuitem" @click="runSongMenu('create')"><Plus :size="16" />新建歌单</button>
        <template v-if="songMenu.playlistId">
          <div class="context-menu__divider"></div>
          <button type="button" role="menuitem" class="is-danger" @click="runSongMenu('remove')"><Trash2 :size="16" />从此歌单移除</button>
        </template>
      </div>
    </Transition>

    <Transition name="context">
      <div
        v-if="feedMenu.open"
        class="context-menu"
        :style="{ left: `${feedMenu.x}px`, top: `${feedMenu.y}px` }"
        role="menu"
        aria-label="推荐歌曲"
        @pointerdown.stop
        @contextmenu.prevent
      >
        <button type="button" role="menuitem" @click="runFeedMenu('next')"><ListStart :size="16" />全部下一首播放</button>
        <button type="button" role="menuitem" @click="runFeedMenu('queue')"><ListPlus :size="16" />全部加入播放队列</button>
      </div>
    </Transition>

    <Transition name="drawer">
      <QueuePanel v-if="status.queueOpen && !media.expanded && status.activeView !== 'queue'" />
    </Transition>
    <PlayerBar />
    <FullPlayer v-if="fullPlayerMounted" />
    <Transition name="toast">
      <div v-if="status.toastMessage" :key="status.toastId" class="toast" role="status" aria-live="polite" @click="status.dismissToast()">
        {{ status.toastMessage }}
      </div>
    </Transition>
    <audio ref="audioRef" preload="auto" crossorigin="anonymous" />

    <Transition name="dialog">
      <AccountDialog v-if="account.dialogOpen" />
    </Transition>
    <Transition name="dialog">
      <ImportDialog v-if="importOpen" :platforms="platforms" @close="importOpen = false" @imported="finishImport" />
    </Transition>
  </div>
</template>
