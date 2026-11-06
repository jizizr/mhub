import { playbackAudioUrl, withTaskVersion } from "./media-routing.js";

const API = "/api/v1";
const playbackStatusRequests = new Map();

async function request(path, options = {}) {
  const response = await fetch(path, {
    ...options,
    headers: {
      ...(options.body ? { "content-type": "application/json" } : {}),
      ...(options.headers ?? {}),
    },
  });
  if (!response.ok) {
    const payload = await response.json().catch(() => ({}));
    throw new Error(payload.detail || `HTTP ${response.status}`);
  }
  return response;
}

export function getHealth() {
  return request("/health").then((res) => res.json());
}

export function getPlatforms() {
  return request(`${API}/platforms`).then((res) => res.json());
}

export function searchTracks({ platform, q, limit = 30, signal }) {
  const params = new URLSearchParams({ platform, q, limit: String(limit) });
  return request(`${API}/search?${params}`, { signal }).then((res) => res.json());
}

export function getRecommendations({ platform = "applemusic", limit = 30 } = {}) {
  const params = new URLSearchParams({ platform, limit: String(limit) });
  return request(`${API}/recommendations?${params}`).then((res) => res.json());
}

export function getRecommendationHome({ platform = "applemusic", limit = 40, refresh = false, exclude = [], signal } = {}) {
  const params = new URLSearchParams({ platform, limit: String(limit) });
  if (refresh) params.set("refresh", "true");
  const seen = [...new Set(exclude.map(String).filter((id) => /^\d+$/.test(id)))].slice(-300);
  if (seen.length) params.set("exclude", seen.join(","));
  return request(`${API}/recommendations/home?${params}`, { signal, cache: "no-store" }).then((res) => res.json());
}

export function getContinuationRecommendations({ platform = "applemusic", seedId, seedIds = [], station = "", exclude = [], limit = 10, signal } = {}) {
  // A long queue must not overflow the browser/proxy URL limit or silently
  // lose its exclusions. This read-only recommendation query uses a JSON body.
  return request(`${API}/recommendations/continue`, {
    method: "POST",
    signal,
    cache: "no-store",
    body: JSON.stringify({
      platform,
      seed_id: String(seedId || ""),
      // Apple AutoPlay is seeded with the queue's latest songs and continues
      // from the station the previous batch returned.
      seed_ids: seedIds.map(String),
      ...(station ? { station } : {}),
      exclude: [...new Set(exclude.map(String))],
      limit,
    }),
  }).then((res) => res.json());
}

// Catalog metadata for a shared song, looked up by platform and id only.
export function getTrackInfo({ platform, id, signal }) {
  return request(`${API}/tracks/${encodeURIComponent(platform)}/${encodeURIComponent(id)}`, { signal }).then((res) => res.json());
}

export function resolveCollection({ platform, url, limit }) {
  const params = new URLSearchParams({ platform, url });
  // Omitting limit asks the server for the complete collection. Recommendation
  // previews can still pass a smaller explicit limit.
  if (limit != null) params.set("limit", String(limit));
  return request(`${API}/collections/resolve?${params}`).then((res) => res.json());
}

const MATCH_BATCH = 25;
// Rounds in a row that may finish nothing before the rest is left deferred.
const MATCH_MAX_STALLS = 5;

function pause(ms, signal) {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(signal.reason);
    const timer = setTimeout(resolve, ms);
    signal?.addEventListener("abort", () => { clearTimeout(timer); reject(signal.reason); }, { once: true });
  });
}

// Matches a whole playlist in small batches. The server paces its platform
// searches and answers "deferred" for songs it could not search yet; those
// are resent after its suggested wait, so a throttled platform slows the
// import down instead of turning songs into "not found".
export async function matchCollection({
  sourcePlatform, targetPlatform, items, onProgress, signal,
  batchSize = MATCH_BATCH, maxStalls = MATCH_MAX_STALLS, wait = pause,
}) {
  const sourceItems = items || [];
  if (!sourceItems.length) throw new Error("歌单没有可匹配的歌曲");
  const results = new Array(sourceItems.length);
  let pending = sourceItems.map((_, index) => index);
  let platforms = { source_platform: sourcePlatform, target_platform: targetPlatform };
  let stalls = 0;
  const report = (waitMs = 0) => onProgress?.({ done: sourceItems.length - pending.length, total: sourceItems.length, waitMs });
  report();
  while (pending.length && stalls < maxStalls) {
    const indexes = pending.slice(0, batchSize);
    const batch = indexes.map((index) => sourceItems[index]);
    const response = await request(`${API}/collections/match`, {
      method: "POST",
      signal,
      body: JSON.stringify({
        source_platform: sourcePlatform,
        target_platform: targetPlatform,
        items: batch.map((item) => ({
          id: item.id,
          song: item.song,
          singer: item.singer,
          cover: item.cover || "",
        })),
      }),
    }).then((res) => res.json());
    // A failed/incomplete batch never becomes a partial import.
    if (response.items?.length !== batch.length || response.items.some((item, index) => String(item.source?.id) !== String(batch[index].id))) {
      throw new Error("歌单匹配结果不完整，已停止导入，请重试");
    }
    platforms = { source_platform: response.source_platform, target_platform: response.target_platform };
    const deferred = [];
    response.items.forEach((item, position) => {
      if (item.status === "deferred") deferred.push(indexes[position]);
      else results[indexes[position]] = item;
    });
    // Deferred songs go first in the next round so the import stays in order.
    pending = [...deferred, ...pending.slice(batch.length)];
    stalls = deferred.length === batch.length ? stalls + 1 : 0;
    if (deferred.length && stalls < maxStalls) {
      const waitMs = Math.max(500, Number(response.retry_after_ms) || 3000);
      report(waitMs);
      await wait(waitMs, signal);
    }
    report();
  }
  // Left over only when the platform stayed throttled; the caller can retry.
  for (const index of pending) {
    results[index] = { source: sourceItems[index], status: "deferred", confidence: 0, candidates: [] };
  }
  const count = (status) => results.filter((item) => item.status === status).length;
  return {
    ...platforms,
    matched_count: count("matched"),
    review_count: count("review"),
    missing_count: count("missing"),
    deferred_count: count("deferred"),
    items: results,
  };
}

export function createPlayback({ platform, query, selectedId, quality, includeCover = false, streamingMode = null }) {
  return request(`${API}/playbacks`, {
    method: "POST",
    body: JSON.stringify({
      platform,
      query,
      selected_id: selectedId || null,
      quality,
      streaming_mode: streamingMode,
      // The caller already has artwork from search/playlist metadata. Avoid
      // making the backend wait for a second cover request before audio can
      // become playable, especially on a slow Telegram/provider path.
      include_cover: includeCover,
    }),
  }).then((res) => res.json());
}

/**
 * Read a playback task. With `since` (the status the caller already knows),
 * the server holds the request for up to `waitMs` until the task leaves that
 * status, so callers learn about a transition without polling. Servers that
 * do not support it simply answer immediately.
 */
export function getPlayback(id, { signal, since, waitMs = 0 } = {}) {
  const abortReason = () => signal?.reason ?? new DOMException("Playback status request aborted", "AbortError");
  if (signal?.aborted) return Promise.reject(abortReason());

  const path = `${API}/playbacks/${encodeURIComponent(String(id))}`;
  const query = since && waitMs > 0
    ? `?${new URLSearchParams({ since: String(since), wait_ms: String(Math.round(waitMs)) })}`
    : "";
  const key = `${id}${query}`;
  let pending = playbackStatusRequests.get(key);
  if (!pending) {
    pending = { controller: new AbortController(), subscribers: new Set() };
    playbackStatusRequests.set(key, pending);
    const entry = pending;
    const settle = (ok, value) => {
      // A cancelled fetch may finish after another caller started a new one.
      if (playbackStatusRequests.get(key) === entry) playbackStatusRequests.delete(key);
      for (const subscriber of [...entry.subscribers]) subscriber(ok, value);
    };
    void request(`${path}${query}`, {
      signal: entry.controller.signal,
      cache: "no-store",
    }).then((res) => res.json()).then(
      (value) => settle(true, value),
      (error) => settle(false, error),
    );
  }

  // Only overlapping reads share a fetch. Every later poll stays fresh, and
  // cancelling one observer must not interrupt another observer of this task.
  const entry = pending;
  return new Promise((resolve, reject) => {
    const finish = (ok, value) => {
      if (!entry.subscribers.delete(finish)) return;
      signal?.removeEventListener("abort", abort);
      if (!ok) { reject(value); return; }
      try {
        // Responses are JSON; keep each caller's transport edits independent.
        resolve(JSON.parse(JSON.stringify(value)));
      } catch (error) { reject(error); }
    };
    const abort = () => {
      finish(false, abortReason());
      if (!entry.subscribers.size) {
        if (playbackStatusRequests.get(key) === entry) playbackStatusRequests.delete(key);
        entry.controller.abort();
      }
    };
    entry.subscribers.add(finish);
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
  });
}

export function getPlaybackLyrics(id, options = {}) {
  return request(`${API}/playbacks/${encodeURIComponent(id)}/lyrics`, options).then((res) => res.json());
}

// Lyrics do not depend on audio preparation or its selected quality. Search
// and playlist metadata already identify the song, even before a playback
// task exists, so this request can run alongside the first audio request.
export function getTrackLyrics(track, options = {}) {
  const sourceId = track?.source_id || track?.track_id || track?.selected_id;
  if (!track?.platform || !sourceId) {
    const id = track?.playback_id || track?.id;
    return id ? getPlaybackLyrics(id, options) : Promise.resolve({ available: false });
  }
  const params = new URLSearchParams({ platform: track.platform, track_id: sourceId });
  if (track.title) params.set("title", track.title);
  if (track.artist) params.set("artist", track.artist);
  if (track.album) params.set("album", track.album);
  if (track.duration > 0) params.set("duration", String(Math.round(track.duration / 1000)));
  return request(`${API}/lyrics?${params}`, options).then((res) => res.json());
}

export function audioUrl(task) {
  // wasm_alac tasks use the dedicated stream endpoint. It serves the
  // same self-describing ALAC packet transport while growing and after
  // completion; ordinary tasks keep the existing compatible /audio path.
  return playbackAudioUrl(task);
}

export function coverUrl(task) {
  return withTaskVersion(task?.cover_url, task?.id);
}
