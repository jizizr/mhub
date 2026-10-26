function supportsNativeFlac() {
  if (typeof document === "undefined" || !document.createElement) return false;
  const audio = document.createElement("audio");
  const support = audio.canPlayType?.("audio/flac");
  return support === "probably" || support === "maybe";
}

// Android needs an actual native media player to own system playback controls.
// Audio Session controls focus/category, not notification ownership; exposing
// that optional API alone is not evidence that a worklet can play in the
// background. Select the existing native lossless path;
// this policy never changes the requested quality or a track's metadata.
// Offline playback keeps access to already cached WASM packet streams.
export function shouldPreferNativeBackgroundPlayback({
  navigatorObject = globalThis.navigator,
  canPlayNative = supportsNativeFlac,
} = {}) {
  try {
    if (!navigatorObject || navigatorObject.onLine === false) return false;
    const platform = String(navigatorObject.userAgentData?.platform || "");
    const userAgent = String(navigatorObject.userAgent || "");
    if (!/\bandroid\b/i.test(platform) && !/\bandroid\b/i.test(userAgent)) return false;
    const support = canPlayNative();
    return support === true || support === "probably" || support === "maybe";
  } catch {
    // A restricted capability accessor or codec probe cannot make an
    // otherwise playable cached/streamed song fail.
    return false;
  }
}
