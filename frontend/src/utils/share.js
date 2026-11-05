// A shared song travels as plain query parameters on the site's own address:
//   /?song=<platform>:<source id>&title=…&artist=…&cover=…
// The platform and source id identify the song for playback; title, artist
// and cover only label it before playback resolves, so a link stays usable
// even if they are missing.

const PLATFORM = /^[a-z0-9_-]{1,32}$/;
const MAX_FIELD = 200;
const MAX_COVER = 400;

export function shareSourceId(track) {
  if (!track) return "";
  const direct = track.source_id || track.selected_id || track.track_id;
  if (direct) return String(direct);
  // Playback ids look like "<platform>--<id>--<quality>".
  if (typeof track.id === "string" && track.id.includes("--")) {
    return track.id.split("--").slice(1, -1).join("--");
  }
  return track.id == null ? "" : String(track.id);
}

function httpUrl(value) {
  const text = String(value || "").trim();
  if (!text || text.length > MAX_COVER) return "";
  try {
    const url = new URL(text);
    return url.protocol === "https:" || url.protocol === "http:" ? url.href : "";
  } catch {
    return "";
  }
}

export function canShareTrack(track) {
  return Boolean(track && PLATFORM.test(String(track.platform || "")) && shareSourceId(track));
}

export function buildShareUrl(track, base) {
  if (!canShareTrack(track)) return "";
  const url = new URL("/", base);
  url.searchParams.set("song", `${track.platform}:${shareSourceId(track)}`);
  if (track.title) url.searchParams.set("title", String(track.title).slice(0, MAX_FIELD));
  if (track.artist) url.searchParams.set("artist", String(track.artist).slice(0, MAX_FIELD));
  // Only a public platform cover is worth carrying; this server's own cover
  // route belongs to a playback task the recipient does not have.
  const cover = httpUrl(track.cover);
  if (cover) url.searchParams.set("cover", cover);
  return url.href;
}

export function parseShareParams(search) {
  const params = new URLSearchParams(search || "");
  const song = params.get("song") || "";
  const split = song.indexOf(":");
  if (split <= 0) return null;
  const platform = song.slice(0, split);
  const id = song.slice(split + 1).trim();
  if (!PLATFORM.test(platform) || !id || id.length > MAX_FIELD) return null;
  return {
    platform,
    id,
    title: (params.get("title") || "").slice(0, MAX_FIELD),
    artist: (params.get("artist") || "").slice(0, MAX_FIELD),
    cover: httpUrl(params.get("cover")),
  };
}

// The address without the share parameters, so a reload or bookmark does not
// offer the same song again.
export function stripShareParams(href) {
  const url = new URL(href);
  for (const key of ["song", "title", "artist", "cover"]) url.searchParams.delete(key);
  return `${url.pathname}${url.search}${url.hash}`;
}

export function shareText(track) {
  const title = track?.title || "一首歌";
  return track?.artist ? `${title} — ${track.artist}` : title;
}
