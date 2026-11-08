// Save the playing song as an ordinary, decrypted audio file.
//
// The player's IndexedDB cache already holds the complete file for native
// tracks (AAC/M4A, FLAC, MP3), so those are saved without any network request.
// The WASM ALAC transport caches a private packet stream ("SPLALAC1") that no
// other player can open; for it, and for tracks not cached yet, the server's
// completed /audio derivative is fetched once. A file downloaded for a native
// track is the very object the player would cache, so it also fills the cache.

import { reactive } from "vue";
import { audioUrl, createPlayback, getPlayback } from "../apis/music.js";
import { cacheAudioBlob, cacheEntryMatches, cacheKey, getCachedTrack, hasCachedAudio } from "./localCache.js";
import { trackFromPlayback } from "../utils/track.js";

const TYPES = {
  flac: "audio/flac",
  m4a: "audio/mp4",
  mp3: "audio/mpeg",
  ogg: "audio/ogg",
  wav: "audio/wav",
};
const MIME_EXT = {
  "audio/flac": "flac", "audio/x-flac": "flac",
  "audio/mp4": "m4a", "audio/x-m4a": "m4a", "audio/m4a": "m4a", "audio/aac": "m4a",
  "audio/mpeg": "mp3", "audio/mp3": "mp3",
  "audio/ogg": "ogg", "audio/wav": "wav", "audio/x-wav": "wav", "audio/wave": "wav",
};
const READY_TIMEOUT_MS = 240_000;

/** Identify a playable container by its first bytes; null for anything else. */
export function sniffAudioExt(bytes) {
  const b = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes || []);
  const ascii = (start, end) => String.fromCharCode(...b.subarray(start, end));
  if (b.length >= 4 && ascii(0, 4) === "fLaC") return "flac";
  if (b.length >= 8 && ascii(0, 8) === "SPLALAC1") return null;
  if (b.length >= 8 && ascii(4, 8) === "ftyp") return "m4a";
  if (b.length >= 4 && ascii(0, 4) === "OggS") return "ogg";
  if (b.length >= 12 && ascii(0, 4) === "RIFF" && ascii(8, 12) === "WAVE") return "wav";
  if (b.length >= 3 && ascii(0, 3) === "ID3") return "mp3";
  if (b.length >= 2 && b[0] === 0xff && (b[1] & 0xe0) === 0xe0) return "mp3";
  return null;
}

async function blobAudioExt(blob) {
  if (!(blob instanceof Blob) || !blob.size) return null;
  return sniffAudioExt(new Uint8Array(await blob.slice(0, 16).arrayBuffer()));
}

export function downloadFilename(track, ext) {
  const clean = (value) => String(value || "")
    // Characters that Windows, macOS or Android refuse in file names.
    .replace(/[\u0000-\u001f\u007f/\\:*?"<>|]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  const title = clean(track?.title) || "未知歌曲";
  const artist = clean(track?.artist);
  let stem = artist ? `${title} - ${artist}` : title;
  // Leave room for the extension within common 255-byte name limits.
  if (stem.length > 120) stem = stem.slice(0, 120).trim();
  stem = stem.replace(/^\.+/, "").replace(/[. ]+$/, "") || "音乐";
  return `${stem}.${ext || "m4a"}`;
}

/** The requested track's native (non-packet) representation. */
function nativeRequest(track) {
  return { ...track, streaming_mode: null, streaming_url: null, streaming_codec: null, streaming_protocol: null };
}

/** A complete, playable Blob already in this browser's audio cache. */
export async function findCachedDownload(track, { lookup = getCachedTrack } = {}) {
  const native = nativeRequest(track);
  const keys = [...new Set([cacheKey(track), cacheKey(native)].filter(Boolean))];
  for (const key of keys) {
    const item = await Promise.resolve(lookup(key)).catch(() => null);
    if (!hasCachedAudio(item) || !cacheEntryMatches(item, native)) continue;
    const ext = await blobAudioExt(item.blob);
    if (!ext) continue;
    const blob = item.blob.type === TYPES[ext] ? item.blob : item.blob.slice(0, item.blob.size, TYPES[ext]);
    return { blob, ext, item };
  }
  return null;
}

async function completedTask(track, { signal, api }) {
  const id = track.playback_id || track.id;
  let task = null;
  if (id) {
    try {
      task = await api.getPlayback(id, { signal });
    } catch (error) {
      if (signal?.aborted) throw error;
      // A restarted or swept server forgets old ids; the POST below restores
      // the same stable song id from its caches.
    }
  }
  if (!task || task.status === "failed") {
    task = (await api.createPlayback({
      platform: track.platform,
      query: track.query || track.title,
      selectedId: track.selected_id || track.source_id,
      quality: track.quality,
      streamingMode: null,
    })).playback;
  }
  const deadline = Date.now() + READY_TIMEOUT_MS;
  while (task.status !== "completed") {
    if (task.status === "failed") throw new Error(task.error || "音频准备失败");
    if (Date.now() > deadline) throw new Error("音频准备超时");
    const started = Date.now();
    const next = await api.getPlayback(task.id, { signal, since: task.status, waitMs: 8000 });
    if (next.status === task.status && Date.now() - started < 900) {
      await new Promise((resolve) => globalThis.setTimeout(resolve, 900));
    }
    task = next;
  }
  return task;
}

async function readWithProgress(response, onProgress) {
  const total = Number(response.headers.get("content-length")) || 0;
  if (!response.body?.getReader || !total) return response.blob();
  const reader = response.body.getReader();
  const chunks = [];
  let received = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    received += value.byteLength;
    onProgress?.(Math.min(1, received / total));
  }
  return new Blob(chunks, { type: response.headers.get("content-type") || "" });
}

/**
 * Resolve a playable file for `track`: { blob, filename, fromCache }.
 * Dependencies are injectable for tests.
 */
export async function resolveTrackDownload(track, {
  signal,
  onProgress,
  onStage,
  lookup = getCachedTrack,
  api = { getPlayback, createPlayback },
  fetchImpl = (...args) => globalThis.fetch(...args),
  store = cacheAudioBlob,
} = {}) {
  if (!track) throw new Error("没有可下载的歌曲");
  const cached = await findCachedDownload(track, { lookup });
  if (cached) return { blob: cached.blob, filename: downloadFilename(track, cached.ext), fromCache: true };

  onStage?.("preparing");
  const task = await completedTask(track, { signal, api });
  // Lossless Apple Music downloads the M4A Telegram keeps (the file the bot
  // sends), never a server-side FLAC transcode.
  const url = audioUrl(nativeRequest(task), { original: true });
  if (!url) throw new Error("服务器没有返回音频地址");
  onStage?.("downloading");
  const response = await fetchImpl(url, { credentials: "omit", cache: "default", signal });
  if (!response.ok) throw new Error(`下载失败：HTTP ${response.status}`);
  const fetched = await readWithProgress(response, onProgress);
  const ext = await blobAudioExt(fetched);
  if (!ext) throw new Error("下载的文件不是可播放的音频");
  const blob = fetched.type === TYPES[ext] ? fetched : fetched.slice(0, fetched.size, TYPES[ext]);

  // Same representation the player caches for a native track: keep it, so a
  // replay (or another download) needs no request. Packet-transport tracks
  // keep their own cache entry and are not duplicated.
  const readyTrack = trackFromPlayback(nativeRequest(task), track);
  // The original ALAC M4A is not the native player's representation.
  const original = new URL(url, "http://same-origin.invalid").pathname.endsWith("/audio/source");
  if (!original && track.streaming_mode !== "wasm_alac" && cacheKey(readyTrack) === cacheKey(nativeRequest(track))) {
    void Promise.resolve(store(cacheKey(readyTrack), null, nativeRequest(task), readyTrack, { blob })).catch(() => {});
  }
  return { blob, filename: downloadFilename(track, ext), fromCache: false };
}

export function saveBlob(blob, filename, doc = globalThis.document) {
  const url = URL.createObjectURL(blob);
  const link = doc.createElement("a");
  link.href = url;
  link.download = filename;
  link.rel = "noopener";
  link.style.display = "none";
  doc.body.appendChild(link);
  link.click();
  link.remove();
  // Some browsers start reading the Blob after click() returns.
  globalThis.setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

/** Per-song download state shared by every download button. */
export const downloads = reactive({});
const inflight = new Map();

export function downloadKey(track) {
  return cacheKey(nativeRequest(track || {})) || String(track?.playback_id || track?.id || "");
}

export function downloadTrack(track, { notify } = {}) {
  const key = downloadKey(track);
  if (!key) return Promise.resolve(false);
  if (inflight.has(key)) return inflight.get(key);
  downloads[key] = { stage: "checking", progress: 0 };
  const run = (async () => {
    try {
      const result = await resolveTrackDownload(track, {
        onStage: (stage) => { downloads[key] = { ...downloads[key], stage }; },
        onProgress: (progress) => { downloads[key] = { stage: "downloading", progress }; },
      });
      saveBlob(result.blob, result.filename);
      notify?.(result.fromCache ? `已从本地缓存保存：${result.filename}` : `已下载：${result.filename}`);
      return true;
    } catch (error) {
      notify?.(error?.message ? `下载失败：${error.message.replace(/^下载失败：/, "")}` : "下载失败");
      return false;
    } finally {
      delete downloads[key];
      inflight.delete(key);
    }
  })();
  inflight.set(key, run);
  return run;
}
