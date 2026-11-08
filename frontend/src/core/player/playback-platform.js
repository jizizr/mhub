// Android shows the system media notification only for a playing media
// element: Web Audio output, and Media Session metadata on their own, never
// create it. Android therefore plays lossless Apple Music through the WASM
// decoder like other browsers, and keeps a silent companion element playing
// beside it (`media-companion.js`) to own the session.
export function needsMediaCompanion({ navigatorObject = globalThis.navigator } = {}) {
  try {
    if (!navigatorObject) return false;
    const platform = String(navigatorObject.userAgentData?.platform || "");
    const userAgent = String(navigatorObject.userAgent || "");
    return /\bandroid\b/i.test(platform) || /\bandroid\b/i.test(userAgent);
  } catch {
    // A restricted accessor cannot make playback fail.
    return false;
  }
}
