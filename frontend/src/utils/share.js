// A shared song travels as one query parameter on the site's own address:
//   /?song=<platform>:<source id>
// Nothing else is trusted from the link: title, artist and cover are looked
// up from the platform by that id when the link is opened.

// Platforms whose catalog the backend can look a song up in by id.
export const SHAREABLE_PLATFORMS = new Set(["applemusic", "netease", "tencent", "soda"]);
const ID = /^[A-Za-z0-9._-]{1,64}$/;
// Older links also carried labels; they are ignored and removed.
const SHARE_KEYS = ["song", "title", "artist", "cover"];

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

export function canShareTrack(track) {
  return Boolean(track && SHAREABLE_PLATFORMS.has(String(track.platform || "")) && ID.test(shareSourceId(track)));
}

export function buildShareUrl(track, base) {
  if (!canShareTrack(track)) return "";
  const url = new URL("/", base);
  url.searchParams.set("song", `${track.platform}:${shareSourceId(track)}`);
  return url.href;
}

export function parseShareParams(search) {
  const song = new URLSearchParams(search || "").get("song") || "";
  const split = song.indexOf(":");
  if (split <= 0) return null;
  const platform = song.slice(0, split);
  const id = song.slice(split + 1).trim();
  if (!SHAREABLE_PLATFORMS.has(platform) || !ID.test(id)) return null;
  return { platform, id };
}

// The address without the share parameters, so a reload or bookmark does not
// open the same song again.
export function stripShareParams(href) {
  const url = new URL(href);
  for (const key of SHARE_KEYS) url.searchParams.delete(key);
  return `${url.pathname}${url.search}${url.hash}`;
}

export function shareText(track) {
  const title = track?.title || "一首歌";
  return track?.artist ? `${title} — ${track.artist}` : title;
}

// Plain text of one lyric line as the lyric views store it.
export function lyricLineText(line) {
  if (!line) return "";
  const words = Array.isArray(line.words) ? line.words.map((word) => String(word.word || "")).join("") : "";
  return (words || String(line.text || "")).replace(/\s+/g, " ").trim();
}
