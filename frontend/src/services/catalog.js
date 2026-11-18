import { getAlbum, getArtist, getTrackInfo, resolveCollection } from "../apis/music.js";
import { trackFromCatalog, trackFromSearch } from "../utils/track.js";

// Apple Music's search tabs, in its order.
export const SECTION_LABELS = {
  top: "最佳结果",
  artists: "艺人",
  albums: "专辑",
  songs: "歌曲",
  playlists: "播放列表",
};

export const KIND_LABELS = { song: "歌曲", album: "专辑", artist: "艺人", playlist: "播放列表" };

/** Whether a platform (from `/platforms`) has album or artist pages. */
export function platformHas(platforms, platformId, section) {
  const platform = (platforms || []).find((item) => item.id === platformId);
  return Boolean(platform?.catalog?.includes(section));
}

/**
 * Tabs to show for a search result: the platform's sections that found
 * something. A songs-only platform gets no tabs at all.
 */
export function visibleSections(result) {
  const sections = Array.isArray(result?.sections) ? result.sections : ["songs"];
  if (sections.length <= 1) return [];
  return sections.filter((section) => SECTION_LABELS[section] && result?.[section]?.length);
}

/** The line under a card: what it is and who made it, as Apple Music shows. */
export function cardSubtitle(item, { withKind = false } = {}) {
  const parts = [];
  if (withKind) parts.push(KIND_LABELS[item?.type] || "");
  if (item?.type === "album" && !withKind && item.release_date) parts.push(String(item.release_date).slice(0, 4));
  if (item?.subtitle) parts.push(item.subtitle);
  return parts.filter(Boolean).join(" · ");
}

export function pageKey(page) {
  return [page.kind, page.platform, page.id || `track:${page.trackId || ""}`].join(":");
}

/** The page entry a catalog card opens. */
export function pageForItem(item) {
  return {
    kind: item.type,
    platform: item.platform,
    id: item.id,
    url: item.url || "",
    title: item.title || "",
    subtitle: item.subtitle || "",
    cover: item.cover || "",
  };
}

/** The album or artist page of a playing or listed song, found through its track info. */
export function pageForTrack(kind, track) {
  const sourceId = track?.source_id || track?.selected_id || track?.track_id || "";
  return {
    kind,
    platform: track?.platform || "",
    id: "",
    trackId: String(sourceId),
    title: kind === "album" ? track?.album || "" : (track?.artists?.[0] || track?.artist || ""),
    subtitle: kind === "album" ? track?.artist || "" : "",
    cover: kind === "album" ? track?.cover || "" : "",
  };
}

const defaultApi = { getAlbum, getArtist, getTrackInfo, resolveCollection };

/**
 * Loads what a catalog page shows. Pages opened from a song first look up
 * the album or artist id through the song's track info.
 */
export async function loadCatalogPage(page, { quality = "high", signal, api = defaultApi } = {}) {
  if (page.kind === "playlist") {
    const collection = await api.resolveCollection({ platform: page.platform, url: page.url || page.id });
    return {
      id: collection.id || page.id,
      title: collection.title || page.title,
      source_url: collection.source_url || page.url || "",
      tracks: (collection.items || []).map((item) => trackFromSearch(item, quality)),
    };
  }
  let id = page.id;
  if (!id && page.trackId) {
    const info = await api.getTrackInfo({ platform: page.platform, id: page.trackId, signal });
    id = page.kind === "album" ? info?.album_id : info?.artist_id;
    if (!id) throw new Error(page.kind === "album" ? "没有找到这首歌所在的专辑" : "没有找到这首歌的艺人页");
  }
  if (page.kind === "album") {
    const album = await api.getAlbum({ platform: page.platform, id, signal });
    return { ...album, tracks: (album.tracks || []).map((item) => trackFromCatalog(item, quality)) };
  }
  if (page.kind === "artist") {
    const artist = await api.getArtist({ platform: page.platform, id, signal });
    return { ...artist, top_songs: (artist.top_songs || []).map((item) => trackFromCatalog(item, quality)) };
  }
  throw new Error("无法打开这个页面");
}

/** "2001-09-14" → "2001年9月14日"; a bare year stays as it is. */
export function formatReleaseDate(value) {
  const match = /^(\d{4})(?:-(\d{1,2})(?:-(\d{1,2}))?)?/.exec(String(value || ""));
  if (!match) return "";
  const [, year, month, day] = match;
  if (!month) return `${year}年`;
  return day ? `${year}年${Number(month)}月${Number(day)}日` : `${year}年${Number(month)}月`;
}

/** "10 首歌曲，41 分钟" for an album's footer. */
export function albumSummary(tracks) {
  const count = tracks?.length || 0;
  const totalMs = (tracks || []).reduce((sum, track) => sum + (Number(track.duration) || 0), 0);
  const minutes = Math.round(totalMs / 60000);
  if (!count) return "";
  if (!minutes) return `${count} 首歌曲`;
  if (minutes < 60) return `${count} 首歌曲，${minutes} 分钟`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return `${count} 首歌曲，${hours} 小时${rest ? ` ${rest} 分钟` : ""}`;
}

/** Disc groups for a multi-disc album; a single disc is one unlabeled group. */
export function discGroups(tracks) {
  const groups = [];
  for (const track of tracks || []) {
    const disc = track.disc_number || 1;
    let group = groups.find((item) => item.disc === disc);
    if (!group) groups.push(group = { disc, tracks: [] });
    group.tracks.push(track);
  }
  return groups.length > 1 ? groups : groups.map((group) => ({ ...group, disc: 0 }));
}
