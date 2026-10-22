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

export function getContinuationRecommendations({ platform = "applemusic", seedId, exclude = [], limit = 10, signal } = {}) {
  // A long queue must not overflow the browser/proxy URL limit or silently
  // lose its exclusions. This read-only recommendation query uses a JSON body.
  return request(`${API}/recommendations/continue`, {
    method: "POST",
    signal,
    cache: "no-store",
    body: JSON.stringify({ platform, seed_id: String(seedId || ""), exclude: [...new Set(exclude.map(String))], limit }),
  }).then((res) => res.json());
}

export function resolveCollection({ platform, url, limit }) {
  const params = new URLSearchParams({ platform, url });
  // Omitting limit asks the server for the complete collection. Recommendation
  // previews can still pass a smaller explicit limit.
  if (limit != null) params.set("limit", String(limit));
  return request(`${API}/collections/resolve?${params}`).then((res) => res.json());
}

export async function matchCollection({ sourcePlatform, targetPlatform, items }) {
  const sourceItems = items || [];
  if (!sourceItems.length) throw new Error("歌单没有可匹配的歌曲");
  const result = {
    source_platform: sourcePlatform,
    target_platform: targetPlatform,
    matched_count: 0,
    review_count: 0,
    missing_count: 0,
    items: [],
  };
  // Keep each backend request bounded while preserving all tracks and their
  // original order. A failed/incomplete batch never becomes a partial import.
  for (let offset = 0; offset < sourceItems.length; offset += 200) {
    const batch = sourceItems.slice(offset, offset + 200);
    const response = await request(`${API}/collections/match`, {
      method: "POST",
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
    if (response.items?.length !== batch.length || response.items.some((item, index) => String(item.source?.id) !== String(batch[index].id))) {
      throw new Error("歌单匹配结果不完整，已停止导入，请重试");
    }
    result.source_platform = response.source_platform;
    result.target_platform = response.target_platform;
    result.items.push(...response.items);
    result.matched_count += response.matched_count;
    result.review_count += response.review_count;
    result.missing_count += response.missing_count;
  }
  return result;
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

export function getPlayback(id, { signal } = {}) {
  const abortReason = () => signal?.reason ?? new DOMException("Playback status request aborted", "AbortError");
  if (signal?.aborted) return Promise.reject(abortReason());

  const key = String(id);
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
    void request(`${API}/playbacks/${encodeURIComponent(key)}`, {
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
