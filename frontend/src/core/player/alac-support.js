// Capability checks stay in the first-screen bundle; the decoder, transport
// and WebAssembly glue in alac-player.js load only when lossless playback
// actually needs them. Decoded PCM plays through scheduled buffer sources, so
// plain Web Audio is enough (no AudioWorklet).
export function isAlacPlaybackSupported() {
  const Context = typeof window !== "undefined" ? (window.AudioContext || window.webkitAudioContext) : null;
  return typeof WebAssembly !== "undefined" && typeof Context === "function" &&
    typeof Context.prototype?.createBufferSource === "function";
}

export function isLosslessTrack(track) {
  const quality = String(track?.quality || "").toLowerCase();
  const format = String(track?.format || track?.content_type || "").toLowerCase();
  return quality === "lossless" || quality === "hires" || quality === "hi-res" || format.includes("alac");
}

export function canUseAlacPlayback(track) {
  return isAlacPlaybackSupported() && track?.platform === "applemusic" && isLosslessTrack(track) &&
    track?.streaming_mode === "wasm_alac";
}

export function canRequestAlacPlayback(track) {
  return isAlacPlaybackSupported() && track?.platform === "applemusic" && isLosslessTrack(track);
}
