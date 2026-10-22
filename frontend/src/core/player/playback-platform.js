function supportsNativeFlac() {
  if (typeof document === "undefined" || !document.createElement) return false;
  const audio = document.createElement("audio");
  const support = audio.canPlayType?.("audio/flac");
  return support === "probably" || support === "maybe";
}

// Android browsers without an Audio Session API need a native media element
// to own the system media session. Select the existing native lossless path;
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
    if (navigatorObject.audioSession) return false;
    const support = canPlayNative();
    return support === true || support === "probably" || support === "maybe";
  } catch {
    // A restricted capability accessor or codec probe cannot make an
    // otherwise playable cached/streamed song fail.
    return false;
  }
}
