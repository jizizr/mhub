// Keeping imported playlists in step with their source playlist.
//
// Only same-platform imports sync: the songs are the source's own songs, so
// no fuzzy cross-platform matching is involved. Each sync is a three-way
// merge of the source as last seen (`source_keys`), the source now, and the
// local playlist:
//   - songs new in the source are inserted next to their source neighbours;
//   - songs the source removed are removed locally;
//   - songs the user added or removed locally keep the user's choice.

export const AUTO_SYNC_INTERVAL_MS = 24 * 60 * 60 * 1000;

/** Platform of a playlist link, or "" when unknown. */
export function platformFromUrl(value = "") {
  if (/music\.163\.com\//i.test(value) || /163cn\.tv\//i.test(value)) return "netease";
  if (/music\.apple\.com\//i.test(value)) return "applemusic";
  if (/y\.qq\.com\//i.test(value)) return "tencent";
  return "";
}

export function sourcePlatformOf(playlist) {
  return playlist?.source_platform || platformFromUrl(playlist?.source_url);
}

/** Imported from a link on the same platform the playlist plays from. */
export function canSyncPlaylist(playlist) {
  return Boolean(playlist?.source_url && playlist.platform && sourcePlatformOf(playlist) === playlist.platform);
}

export function isSyncDue(playlist, now = Date.now()) {
  return canSyncPlaylist(playlist) && now - (playlist.synced_at || playlist.created_at || 0) >= AUTO_SYNC_INTERVAL_MS;
}

/**
 * @param local    keys in the local playlist, in order
 * @param base     source keys at the last sync (null for playlists imported
 *                 before syncing existed: nothing is removed, and source
 *                 songs missing locally are added back once)
 * @param upstream source keys now, in source order
 * @returns {{ remove: string[], insert: { key: string, after: string | null }[], next: string[] }}
 *          `after: null` inserts at the start.
 */
export function planPlaylistSync({ local, base, upstream }) {
  const localSet = new Set(local);
  const upstreamSet = new Set(upstream);
  const baseSet = new Set(base ?? local.filter((key) => upstreamSet.has(key)));
  const removed = new Set(local.filter((key) => baseSet.has(key) && !upstreamSet.has(key)));
  const next = local.filter((key) => !removed.has(key));
  const present = new Set(next);
  const insert = [];
  upstream.forEach((key, index) => {
    if (baseSet.has(key) || localSet.has(key) || present.has(key)) return;
    // Place the new song after the closest earlier source song that is in
    // the playlist, so songs added at the top of the source land at the top.
    let after = null;
    for (let previous = index - 1; previous >= 0; previous -= 1) {
      if (present.has(upstream[previous])) { after = upstream[previous]; break; }
    }
    next.splice(after === null ? 0 : next.indexOf(after) + 1, 0, key);
    present.add(key);
    insert.push({ key, after });
  });
  return { remove: [...removed], insert, next };
}

/** The synced track list for a local (guest) playlist; local entries win. */
export function syncedTracks(local, upstream, plan, keyOf) {
  const byKey = new Map([...upstream, ...local].map((track) => [keyOf(track), track]));
  return plan.next.map((key) => byKey.get(key)).filter(Boolean);
}
