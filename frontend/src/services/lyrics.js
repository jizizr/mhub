import { getTrackLyrics } from "../apis/music.js";

const loadLyricParser = () => import("../utils/lyrics.js").then((module) => module.buildLyricLines);

const STORAGE_KEY = "music-web-lyrics-v1";
const MAX_ENTRIES = 100;
const MAX_STORAGE_CHARS = 500_000;
const MAX_LYRIC_CHARS = 100_000;
const HIT_TTL = 30 * 24 * 60 * 60 * 1000;
const EMPTY_TTL = 5 * 60 * 1000;

export function lyricIdentity(track) {
  if (!track) return "";
  const sourceId = track.source_id || track.track_id || track.selected_id;
  // Quality, task ID, audio transport and script preference do not change
  // the original lyrics. Script conversion remains lazy in the full player.
  if (track.platform && sourceId) return JSON.stringify([track.platform, String(sourceId)]);
  const id = track.playback_id || track.id;
  return id ? JSON.stringify(["playback", String(id)]) : "";
}

function cleanPayload(payload) {
  if (!payload || typeof payload.available !== "boolean") throw new Error("歌词响应不完整");
  const plain = typeof payload.plain === "string" ? payload.plain : "";
  const translation = typeof payload.translation === "string" ? payload.translation : "";
  if (plain.length + translation.length > MAX_LYRIC_CHARS) throw new Error("歌词内容过长");
  return { available: payload.available && Boolean(plain.trim() || translation.trim()), plain, translation };
}

export function createLyricsService({
  fetchLyrics = getTrackLyrics,
  parseLyrics,
  loadParser = loadLyricParser,
  storage = () => globalThis.localStorage,
  now = Date.now,
  retryDelay = 1000,
  requestTimeout = 25000,
} = {}) {
  const memory = new Map();
  const inflight = new Map();
  let parser = parseLyrics;
  let parserPromise;

  function ensureParser() {
    if (parser) return Promise.resolve(parser);
    if (!parserPromise) {
      parserPromise = Promise.resolve().then(loadParser).then((loaded) => {
        if (typeof loaded !== "function") throw new Error("歌词解析器暂不可用");
        parser = loaded;
        return loaded;
      }).catch((error) => {
        parserPromise = null;
        throw error;
      });
    }
    return parserPromise;
  }

  function readEntries() {
    try {
      const raw = storage()?.getItem(STORAGE_KEY);
      if (!raw || raw.length > MAX_STORAGE_CHARS) return [];
      const saved = JSON.parse(raw);
      if (saved?.version !== 1 || !Array.isArray(saved.entries)) return [];
      return saved.entries.filter((entry) => {
        if (typeof entry.key !== "string" || entry.key.length > 1024 ||
            !Number.isFinite(entry.expiresAt) || entry.expiresAt <= now() ||
            entry.expiresAt > now() + HIT_TTL || !Number.isFinite(entry.usedAt)) return false;
        try { cleanPayload(entry.payload); return true; } catch { return false; }
      }).slice(-MAX_ENTRIES);
    } catch { return []; }
  }

  function remember(key, entry) {
    memory.delete(key);
    memory.set(key, entry);
    while (memory.size > MAX_ENTRIES) memory.delete(memory.keys().next().value);
  }

  function cached(key) {
    const current = memory.get(key);
    if (current && current.expiresAt > now()) {
      remember(key, current);
      return current.lines;
    }
    memory.delete(key);
    // Synchronous probes can use memory immediately. Persisted originals are
    // parsed by load() once the optional parser chunk is available.
    if (!parser) return null;
    const stored = readEntries().find((entry) => entry.key === key);
    if (!stored) return null;
    try {
      const lines = parser(cleanPayload(stored.payload));
      remember(key, { lines, expiresAt: stored.expiresAt });
      return lines;
    } catch { return null; }
  }

  function save(key, payload, lines) {
    const timestamp = now();
    const entry = { key, payload, usedAt: timestamp, expiresAt: timestamp + (lines.length ? HIT_TTL : EMPTY_TTL) };
    remember(key, { lines, expiresAt: entry.expiresAt });
    try {
      let entries = readEntries().filter((item) => item.key !== key).sort((a, b) => a.usedAt - b.usedAt);
      entries.push(entry);
      entries = entries.slice(-MAX_ENTRIES);
      let encoded = JSON.stringify({ version: 1, entries });
      while (encoded.length > MAX_STORAGE_CHARS && entries.length > 1) {
        entries.shift();
        encoded = JSON.stringify({ version: 1, entries });
      }
      if (encoded.length <= MAX_STORAGE_CHARS) storage()?.setItem(STORAGE_KEY, encoded);
    } catch {
      // Private mode, quota limits or unavailable storage cannot block audio
      // or lyrics. The bounded session cache still serves the next opening.
    }
  }

  async function request(track) {
    let lastError;
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), requestTimeout);
      try {
        return cleanPayload(await fetchLyrics(track, {
          signal: controller.signal,
          priority: "low",
          credentials: "same-origin",
        }));
      } catch (error) { lastError = error; }
      finally { clearTimeout(timer); }
      if (attempt === 0) await new Promise((resolve) => setTimeout(resolve, retryDelay));
    }
    throw lastError;
  }

  function load(track) {
    const key = lyricIdentity(track);
    if (!key) return Promise.resolve([]);
    if (inflight.has(key)) return inflight.get(key);
    const lines = cached(key);
    if (lines !== null) return Promise.resolve(lines);
    // All callers (early playback, resolved playback and opening the full
    // player) join this one request. Failures are never stored as "no lyrics".
    const stored = readEntries().find((entry) => entry.key === key);
    // Start the API immediately on playback, alongside downloading the parser.
    // Restored originals need no API call. Both paths share the same in-flight
    // result, including parser startup, so an expanded player cannot race it.
    const payloadRequest = stored ? Promise.resolve(cleanPayload(stored.payload)) : request(track);
    // A fast chunk failure must not release inflight while the API is still
    // running; another opening would otherwise submit the same lyric request.
    const parserResult = ensureParser().then(
      (parse) => ({ ok: true, parse }),
      (error) => ({ ok: false, error }),
    );
    const pending = Promise.all([payloadRequest, parserResult]).then(async ([payload, loaded]) => {
      if (!loaded.ok) throw loaded.error;
      let result;
      try { result = loaded.parse(payload); }
      catch (error) {
        if (!stored) throw error;
        // A valid-looking but unparseable stored payload must not permanently
        // hide corrected lyrics from the provider.
        const fresh = await request(track);
        result = loaded.parse(fresh);
        save(key, fresh, result);
        return result;
      }
      if (stored) remember(key, { lines: result, expiresAt: stored.expiresAt });
      else save(key, payload, result);
      return result;
    }).finally(() => { if (inflight.get(key) === pending) inflight.delete(key); });
    inflight.set(key, pending);
    return pending;
  }

  return { load, getCached: (track) => cached(lyricIdentity(track)) };
}

const lyrics = createLyricsService();
export const loadTrackLyrics = lyrics.load;
