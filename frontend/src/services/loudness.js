// Whole-track analysis is optional metadata. It must never create a playback
// task, download audio, or delay a user's request to start listening.
const VERSION = 1;
const STORAGE_KEY = "music-web-loudness-v1";
const MAX_ENTRIES = 300;
const POLL_INTERVAL_MS = 5000;
const POLL_BUDGET_MS = 60000;

const state = (value) => ({ state: value, version: VERSION });

function playbackId(track) {
  const id = String(track?.playback_id || track?.id || "").trim();
  return id && id.length <= 512 ? id : null;
}

function cacheKey(track) {
  const id = playbackId(track);
  const quality = String(track?.quality || "").trim().toLowerCase();
  const format = String(track?.format || "").trim().toLowerCase();
  const bytes = track?.byte_size;
  // A streaming task can initially have neither its final format nor size.
  // Do not let that unknown representation reuse an old FLAC/ALAC result.
  if (!id || !quality || !format || !Number.isSafeInteger(bytes) || bytes <= 0) return null;
  return JSON.stringify([
    VERSION, id, quality, format, bytes,
    track?.streaming_mode || "", track?.streaming_codec || "", track?.streaming_protocol || "",
  ]);
}

function readyAnalysis(value) {
  if (value?.state !== "ready" || value.version !== VERSION) return null;
  const { integrated_lufs: integrated, true_peak_dbfs: peak, gain_db: gain } = value;
  if (![integrated, peak, gain].every(Number.isFinite)
    || integrated <= -70 || integrated > 10 || peak < -100 || peak > 20) return null;
  // Match the server's conservative -16 LUFS target, +6 dB boost limit, and
  // -1 dB true-peak ceiling. A corrupt stored gain must not amplify playback.
  const expectedGain = Math.min(-16 - integrated, -1 - peak, 6);
  if (Math.abs(gain - expectedGain) >= 0.000001) return null;
  return { state: "ready", version: VERSION, integrated_lufs: integrated, true_peak_dbfs: peak, gain_db: gain };
}

function readEntries() {
  try {
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY) || "null");
    if (saved?.version !== VERSION || !Array.isArray(saved.entries)) return [];
    return saved.entries.slice(-MAX_ENTRIES).flatMap((entry) => {
      const analysis = readyAnalysis(entry?.analysis);
      return analysis && typeof entry.key === "string" && Number.isFinite(entry.updatedAt)
        ? [{ key: entry.key, analysis, updatedAt: entry.updatedAt }]
        : [];
    });
  } catch {
    return [];
  }
}

function saveAnalysis(key, analysis) {
  if (!key) return;
  try {
    const entries = readEntries().filter((entry) => entry.key !== key);
    entries.push({ key, analysis, updatedAt: Date.now() });
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ version: VERSION, entries: entries.slice(-MAX_ENTRIES) }));
  } catch {
    // Private browsing, full storage, or storage being disabled must not
    // change playback or turn a completed analysis into an error.
  }
}

/** Read synchronously when selecting a source; keep this gain for that play. */
export function getCachedLoudness(track) {
  const key = cacheKey(track);
  if (!key) return null;
  return readEntries().findLast((entry) => entry.key === key)?.analysis || null;
}

function abortError() {
  return new DOMException("Loudness analysis request was cancelled", "AbortError");
}

function waitForPoll(signal) {
  return new Promise((resolve, reject) => {
    const finish = () => {
      signal.removeEventListener("abort", cancel);
      resolve();
    };
    const cancel = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", cancel);
      reject(abortError());
    };
    const timer = setTimeout(finish, POLL_INTERVAL_MS);
    signal.addEventListener("abort", cancel, { once: true });
    if (signal.aborted) cancel();
  });
}

/**
 * Warm only an existing task's metadata. Call without awaiting the playback
 * path. A ready result is for subsequent playback, not an in-song gain jump.
 * Explicit cancellation rejects with AbortError; network/analysis failures
 * resolve safely as unavailable and an expired polling budget stays pending.
 */
export async function warmLoudness(track, { signal } = {}) {
  if (signal?.aborted) throw abortError();
  const id = playbackId(track);
  if (!id) return state("unavailable");
  const cached = getCachedLoudness(track);
  if (cached) return cached;
  const key = cacheKey(track);
  const controller = new AbortController();
  const cancel = () => controller.abort();
  signal?.addEventListener("abort", cancel, { once: true });
  const budget = setTimeout(cancel, POLL_BUDGET_MS);
  let result = state("pending");
  try {
    while (!controller.signal.aborted) {
      const response = await fetch(`/api/v1/playbacks/${encodeURIComponent(id)}/loudness`, {
        method: "GET", credentials: "same-origin", cache: "no-store", signal: controller.signal,
      });
      if (!response.ok) return state("unavailable");
      const payload = await response.json();
      if (controller.signal.aborted) break;
      if (payload?.version !== VERSION) return state("unavailable");
      if (payload.state === "ready") {
        const analysis = readyAnalysis(payload);
        if (!analysis) return state("unavailable");
        saveAnalysis(key, analysis);
        return analysis;
      }
      if (payload.state !== "pending") return state("unavailable");
      result = state("pending");
      await waitForPoll(controller.signal);
    }
    if (signal?.aborted) throw abortError();
    return result;
  } catch (error) {
    if (signal?.aborted) throw abortError();
    if (controller.signal.aborted) return result;
    return state("unavailable");
  } finally {
    clearTimeout(budget);
    signal?.removeEventListener("abort", cancel);
  }
}
