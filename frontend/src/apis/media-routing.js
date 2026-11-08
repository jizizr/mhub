// Public deployment configuration, never derived from a response or user input.
// Production and local development use the API origin unless explicitly configured.
const buildMediaBase = import.meta.env?.VITE_MEDIA_BASE_URL || "";
const buildLegacyMediaBase = import.meta.env?.VITE_LEGACY_MEDIA_BASE_URL || "";

export function mediaOrigin(value = buildMediaBase) {
  if (!value) return "";
  try {
    const url = new URL(value);
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password ||
        url.pathname !== "/" || url.search || url.hash) return "";
    return url.origin;
  } catch { return ""; }
}

export function trustedMediaOrigins() {
  const origin = mediaOrigin();
  return origin ? [origin] : [];
}

export function withTaskVersion(endpoint, id) {
  if (!endpoint) return "";
  const relative = String(endpoint).startsWith("/") && !String(endpoint).startsWith("//");
  try {
    const url = new URL(endpoint, "http://same-origin.invalid");
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password) return "";
    // v is a task identifier only. The server's rev identifies its generation
    // and representation; all existing query parameters must be preserved.
    url.searchParams.set("v", String(id));
    return relative ? `${url.pathname}${url.search}${url.hash}` : url.href;
  } catch { return ""; }
}

export function playbackAudioUrl(task, { mediaBaseUrl = buildMediaBase, legacyMediaBaseUrl = buildLegacyMediaBase, packets = false, original = false } = {}) {
  // A completed lossless song is read as its stored M4A, exactly as
  // Telegram keeps it: the WASM player demuxes it, and `original` (a
  // download) saves it as is. `packets` keeps the SPLALAC1 endpoint for the
  // growing-stream handoff, whose byte offsets belong to that transport.
  const wasm = task?.streaming_mode === "wasm_alac";
  const source = task?.status === "completed" && task?.alac_source_url;
  const endpoint = source && (original || (wasm && !packets))
    ? task.alac_source_url
    : wasm && task?.streaming_url ? task.streaming_url : task?.audio_url;
  if (!endpoint) return "";
  const versioned = withTaskVersion(endpoint, task.id);
  if (!versioned) return "";
  const url = new URL(versioned, "http://same-origin.invalid");
  const expected = `/api/v1/playbacks/${encodeURIComponent(task.id)}/audio`;
  // Only these two audio representations may use a configured media origin. API, artwork,
  // lyrics, modules and unknown paths must never be rewritten here.
  if (url.pathname !== expected && url.pathname !== `${expected}/stream` && url.pathname !== `${expected}/source`) return versioned;
  // A growing stream also uses the media origin, so audio never competes with
  // the API origin's page bandwidth. The server marks it `no-store,
  // no-transform` and the CDN bypasses it; once completed, the same URL serves
  // the cacheable file. Tasks without playable audio stay on the API origin.
  if (task.status !== "completed" && task.status !== "streaming") return `${url.pathname}${url.search}${url.hash}`;
  const origin = mediaOrigin(mediaBaseUrl);
  if (origin) return `${origin}${url.pathname}${url.search}${url.hash}`;
  // Persisted tasks and in-flight old status responses may still contain the
  // previous production media host. New playback uses the API origin without
  // changing the task revision, representation or local cache identity. Direct
  // legacy download links remain valid on the server; this is not a redirect.
  return url.origin === mediaOrigin(legacyMediaBaseUrl)
    ? `${url.pathname}${url.search}${url.hash}` : versioned;
}
