export function idComponent(value) {
  const text = String(value || "").trim();
  let out = "";
  for (const char of text) {
    out += /[A-Za-z0-9._-]/.test(char) ? char : "_";
  }
  return out || "unknown";
}

export function playbackId(platform, trackId, quality) {
  return [platform, trackId, quality].map(idComponent).join("--");
}

export function trackFromSearch(item, quality = "high") {
  const id = playbackId(item.platform, item.id, quality);
  return {
    id,
    platform: item.platform,
    source_id: item.id,
    selected_id: item.id,
    query: item.song,
    title: item.song,
    artist: item.singer,
    artists: String(item.singer || "")
      .split(/[/,，、]/)
      .map((name) => name.trim())
      .filter(Boolean),
    album: "",
    cover: item.cover || "",
    quality,
    duration: 0,
    byte_size: null,
    bitrate: null,
    format: "",
  };
}

export function trackFromPlayback(task, fallback = {}) {
  const durationMs = Number.isFinite(task?.duration) ? task.duration * 1000 : fallback.duration || 0;
  // Transport is negotiated by the backend. A response without streaming
  // fields explicitly selects the native path, even if the request or an
  // older cached task asked for WASM. Inheriting that request would feed a
  // normal M4A/FLAC file to the packet decoder and poison its cache key.
  const transport = task ?? fallback;
  return {
    ...fallback,
    id: task?.id || fallback.id,
    playback_id: task?.id || fallback.playback_id,
    platform: task?.platform || fallback.platform,
    source_id: task?.track_id || task?.selected_id || fallback.source_id,
    selected_id: task?.selected_id || task?.track_id || fallback.selected_id,
    query: task?.query || fallback.query,
    title: task?.title || fallback.title || task?.query || "未知歌曲",
    artist: task?.artist || fallback.artist || "未知艺人",
    artists: String(task?.artist || fallback.artist || "")
      .split(/[/,，、]/)
      .map((name) => name.trim())
      .filter(Boolean),
    album: task?.album || fallback.album || "",
    cover: fallback.cover || "",
    quality: task?.quality || fallback.quality || "high",
    duration: durationMs,
    byte_size: task?.byte_size ?? fallback.byte_size ?? null,
    bitrate: task?.bitrate ?? fallback.bitrate ?? null,
    format: task?.format || fallback.format || "",
    content_type: task?.content_type || fallback.content_type || "",
    // The backend opts into the experimental WASM ALAC transport explicitly.
    // Keep this separate from `quality`: a lossless request may still be an
    // AAC/M4A fallback and must continue through the native path.
    streaming_mode: transport.streaming_mode || "",
    streaming_url: transport.streaming_url || "",
    streaming_codec: transport.streaming_codec || "",
    streaming_protocol: transport.streaming_protocol || "",
    audio_url: task?.audio_url || fallback.audio_url || "",
    cover_url: task?.cover_url || fallback.cover_url || "",
    lyrics_url: task?.lyrics_url || fallback.lyrics_url || "",
    status: task?.status || fallback.status || "queued",
  };
}
