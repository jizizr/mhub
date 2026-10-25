// Plan how guest data (made before this device was linked to the account)
// joins the account library. Pure data in, plan out: the UI shows the plan,
// library.applyMergePlan() executes it.
import { trackIdentity } from "../../stores/queue.js";

/** Playlists with the same normalized name and this much track overlap merge. */
export const MERGE_OVERLAP = 0.6;

export const normalizeName = (name) => String(name || "")
  .normalize("NFKC").toLowerCase().replace(/[\s\p{P}\p{S}]+/gu, "");

function keysOf(tracks) {
  return [...new Set((tracks || []).map(trackIdentity).filter(Boolean))];
}

function overlap(guestKeys, cloudKeys) {
  if (!guestKeys.length) return 1;
  const cloud = new Set(cloudKeys);
  return guestKeys.filter((key) => cloud.has(key)).length / guestKeys.length;
}

/**
 * @param guest  { playlists: [{ id, name, tracks, importedTo? }], liked: [key], less: [key] }
 * @param cloud  readLibrary() result
 * @param accountId  playlists already imported into this account are skipped
 */
export function planGuestMerge(guest, cloud, accountId) {
  const plan = { create: [], merge: [], liked: [], less: [], importedIds: [], counts: { created: 0, merged: 0, tracks: 0, taste: 0 } };
  const byName = new Map();
  for (const playlist of cloud.playlists) {
    const name = normalizeName(playlist.name);
    if (!byName.has(name)) byName.set(name, []);
    byName.get(name).push(playlist);
  }
  for (const playlist of guest.playlists || []) {
    if (playlist.importedTo === accountId) continue;
    plan.importedIds.push(playlist.id);
    const guestKeys = keysOf(playlist.tracks);
    // Same playlist that already reached the cloud (e.g. imported twice from
    // one source link, or recreated by hand): merge instead of duplicating.
    const match = (byName.get(normalizeName(playlist.name)) || [])
      .map((candidate) => ({ candidate, score: overlap(guestKeys, keysOf(candidate.tracks)) }))
      .filter(({ score }) => score >= MERGE_OVERLAP)
      .sort((left, right) => right.score - left.score)[0]?.candidate
      || (playlist.source_url && cloud.playlists.find((candidate) => candidate.source_url === playlist.source_url));
    if (match) {
      const present = new Set(keysOf(match.tracks));
      const missing = (playlist.tracks || []).filter((track) => {
        const key = trackIdentity(track);
        return key && !present.has(key) && present.add(key);
      });
      if (missing.length) {
        plan.merge.push({ cloudId: match.id, name: match.name, tracks: missing });
        plan.counts.tracks += missing.length;
      }
      plan.counts.merged += 1;
    } else {
      plan.create.push({
        name: playlist.name,
        platform: playlist.platform || "",
        sourceId: playlist.source_id || "",
        sourceUrl: playlist.source_url || "",
        sourcePlatform: playlist.source_platform || "",
        sourceKeys: playlist.source_keys ?? null,
        cover: playlist.cover || "",
        tracks: playlist.tracks || [],
      });
      plan.counts.created += 1;
      plan.counts.tracks += guestKeys.length;
    }
  }
  const cloudLiked = new Set(cloud.liked);
  const cloudLess = new Set(cloud.less);
  plan.liked = (guest.liked || []).filter((key) => key && !cloudLiked.has(key));
  plan.less = (guest.less || []).filter((key) => key && !cloudLess.has(key));
  plan.counts.taste = plan.liked.length + plan.less.length;
  return plan;
}

export const planIsEmpty = (plan) => !plan.create.length && !plan.merge.length && !plan.liked.length && !plan.less.length;
