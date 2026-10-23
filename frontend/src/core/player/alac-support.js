import { shouldPreferNativeBackgroundPlayback } from "./playback-platform.js";

// Capability checks stay in the first-screen bundle; the decoder, transport
// and WebAssembly glue in alac-worklet.js load only when lossless playback
// actually needs them.
export function isAlacWorkletSupported() {
  const Context = typeof window !== "undefined" ? (window.AudioContext || window.webkitAudioContext) : null;
  return typeof WebAssembly !== "undefined" && typeof Context === "function" &&
    Boolean(Context.prototype && "audioWorklet" in Context.prototype) && typeof AudioWorkletNode !== "undefined";
}

export function isLosslessTrack(track) {
  const quality = String(track?.quality || "").toLowerCase();
  const format = String(track?.format || track?.content_type || "").toLowerCase();
  return quality === "lossless" || quality === "hires" || quality === "hi-res" || format.includes("alac");
}

export function canUseAlacWorklet(track) {
  return isAlacWorkletSupported() && track?.platform === "applemusic" && isLosslessTrack(track) &&
    track?.streaming_mode === "wasm_alac";
}

export function canRequestAlacWorklet(track) {
  return isAlacWorkletSupported() && track?.platform === "applemusic" && isLosslessTrack(track) &&
    !shouldPreferNativeBackgroundPlayback();
}
