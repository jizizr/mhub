//! Plain JSON view of an account library, for clients without a Yjs engine
//! (the native apps). Reads return a snapshot; writes are small operations
//! with the same meaning as the web client's `services/sync/library.js`. The
//! server applies them to a scratch replica and stores the resulting Yjs
//! update like any other client's, so web replicas merge them as usual.

use crate::ApiResult;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use yrs::{
    Any, Array, ArrayPrelim, ArrayRef, Doc, GetString, Map, MapPrelim, MapRef, Out, ReadTxn,
    StateVector, Transact, TransactionMut, Update, updates::decoder::Decode,
};

/// Same bound as the web client's taste lists.
const TASTE_LIMIT: u32 = 80;
const UNTITLED: &str = "未命名歌单";
/// One request may not carry more operations than this.
pub const MAX_OPS: usize = 500;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct TrackMeta {
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub source_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub cover: String,
}

impl TrackMeta {
    /// The web client's `trackIdentity()`: `platform:source_id`.
    pub fn key(&self) -> String {
        let platform = self.platform.trim().to_lowercase();
        [platform.as_str(), self.source_id.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(":")
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SnapshotTrack {
    pub key: String,
    #[serde(flatten)]
    pub meta: TrackMeta,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SnapshotPlaylist {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub source_id: String,
    pub source_url: String,
    pub source_platform: String,
    pub cover: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub tracks: Vec<SnapshotTrack>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Snapshot {
    /// Newest first, as the web client lists them.
    pub playlists: Vec<SnapshotPlaylist>,
    pub liked: Vec<String>,
    pub less: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Create {
        id: String,
        name: String,
        #[serde(default)]
        platform: String,
        #[serde(default)]
        source_id: String,
        #[serde(default)]
        source_url: String,
        #[serde(default)]
        source_platform: String,
        #[serde(default)]
        cover: String,
        #[serde(default)]
        tracks: Vec<TrackMeta>,
    },
    /// Append the tracks that are not in the playlist yet.
    Append {
        id: String,
        tracks: Vec<TrackMeta>,
    },
    Rename {
        id: String,
        name: String,
    },
    Delete {
        id: String,
    },
    /// Remove every occurrence of one track.
    Remove {
        id: String,
        key: String,
    },
    /// Move a track right after `after` (`None`: to the top).
    Move {
        id: String,
        key: String,
        #[serde(default)]
        after: Option<String>,
    },
    /// Most recent first, unique, bounded (`liked` or `less`).
    Taste {
        kind: TasteKind,
        key: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TasteKind {
    Liked,
    Less,
}

impl TasteKind {
    fn name(self) -> &'static str {
        match self {
            Self::Liked => "liked",
            Self::Less => "less",
        }
    }
}

/// A scratch replica holding a copy of the account document.
pub fn replica(state: &[u8]) -> ApiResult<Doc> {
    let doc = Doc::new();
    let update = Update::decode_v1(state).map_err(|_| invalid("资料库数据无效"))?;
    doc.transact_mut()
        .apply_update(update)
        .map_err(|_| invalid("资料库数据无效"))?;
    Ok(doc)
}

/// Apply `ops` to `doc`; returns the Yjs update they produced (`None` when
/// nothing changed).
pub fn apply_ops(doc: &Doc, ops: &[Op], now_ms: i64) -> ApiResult<Option<Vec<u8>>> {
    if ops.len() > MAX_OPS {
        return Err(invalid("一次提交的操作过多"));
    }
    let before = doc.transact().state_vector();
    let playlists = doc.get_or_insert_map("playlists");
    let tracks = doc.get_or_insert_map("tracks");
    let taste = doc.get_or_insert_map("taste");
    {
        let mut txn = doc.transact_mut();
        for op in ops {
            apply_op(&mut txn, &playlists, &tracks, &taste, op, now_ms as f64)?;
        }
    }
    let after = doc.transact().state_vector();
    if after == before {
        return Ok(None);
    }
    Ok(Some(doc.transact().encode_diff_v1(&before)))
}

fn apply_op(
    txn: &mut TransactionMut,
    playlists: &MapRef,
    tracks: &MapRef,
    taste: &MapRef,
    op: &Op,
    now: f64,
) -> ApiResult<()> {
    match op {
        Op::Create {
            id,
            name,
            platform,
            source_id,
            source_url,
            source_platform,
            cover,
            tracks: items,
        } => {
            if id.trim().is_empty() || id.len() > 128 {
                return Err(invalid("歌单标识无效"));
            }
            let mut keys = remember(txn, tracks, items);
            dedupe_keys(&mut keys);
            let name = clean_name(name);
            let entry = playlists.insert(txn, id.as_str(), MapPrelim::default());
            entry.insert(txn, "name", name);
            entry.insert(txn, "platform", platform.as_str());
            entry.insert(txn, "source_id", source_id.as_str());
            entry.insert(txn, "source_url", source_url.as_str());
            if !source_platform.is_empty() {
                entry.insert(txn, "source_platform", source_platform.as_str());
            }
            let cover = if cover.is_empty() {
                items
                    .first()
                    .map(|track| track.cover.as_str())
                    .unwrap_or("")
            } else {
                cover.as_str()
            };
            entry.insert(txn, "cover", cover);
            entry.insert(txn, "created_at", now);
            entry.insert(txn, "updated_at", now);
            entry.insert(
                txn,
                "order",
                ArrayPrelim::from(keys.into_iter().map(Any::from).collect::<Vec<_>>()),
            );
        }
        Op::Append { id, tracks: items } => {
            let Some((entry, order)) = playlist(txn, playlists, id) else {
                return Ok(());
            };
            let mut present: std::collections::HashSet<String> =
                strings(txn, &order).into_iter().collect();
            let keys: Vec<String> = remember(txn, tracks, items)
                .into_iter()
                .filter(|key| present.insert(key.clone()))
                .collect();
            if keys.is_empty() {
                return Ok(());
            }
            for key in keys {
                order.push_back(txn, key);
            }
            if string(txn, &entry, "cover").is_empty()
                && let Some(first) = items.first()
            {
                entry.insert(txn, "cover", first.cover.as_str());
            }
            entry.insert(txn, "updated_at", now);
        }
        Op::Rename { id, name } => {
            if let Some((entry, _)) = playlist(txn, playlists, id) {
                entry.insert(txn, "name", clean_name(name));
                entry.insert(txn, "updated_at", now);
            }
        }
        Op::Delete { id } => {
            playlists.remove(txn, id.as_str());
        }
        Op::Remove { id, key } => {
            if let Some((entry, order)) = playlist(txn, playlists, id) {
                remove_all(txn, &order, key);
                entry.insert(txn, "updated_at", now);
            }
        }
        Op::Move { id, key, after } => {
            let Some((entry, order)) = playlist(txn, playlists, id) else {
                return Ok(());
            };
            if !strings(txn, &order).contains(key) || after.as_deref() == Some(key.as_str()) {
                return Ok(());
            }
            remove_all(txn, &order, key);
            let position = match after {
                None => 0,
                Some(after) => strings(txn, &order)
                    .iter()
                    .position(|value| value == after)
                    .map(|index| index as u32 + 1)
                    .unwrap_or_else(|| order.len(txn)),
            };
            order.insert(txn, position, key.as_str());
            entry.insert(txn, "updated_at", now);
        }
        Op::Taste { kind, key } => {
            if key.is_empty() {
                return Ok(());
            }
            let list = match taste.get(txn, kind.name()) {
                Some(Out::YArray(list)) => list,
                _ => taste.insert(txn, kind.name(), ArrayPrelim::default()),
            };
            remove_all(txn, &list, key);
            list.insert(txn, 0, key.as_str());
            let length = list.len(txn);
            if length > TASTE_LIMIT {
                list.remove_range(txn, TASTE_LIMIT, length - TASTE_LIMIT);
            }
        }
    }
    Ok(())
}

/// Store track metadata (only when new or changed, like the web client) and
/// return the keys in input order.
fn remember(txn: &mut TransactionMut, tracks: &MapRef, items: &[TrackMeta]) -> Vec<String> {
    let mut keys = Vec::new();
    for item in items {
        let key = item.key();
        if key.is_empty() {
            continue;
        }
        let existing = tracks
            .get(txn, &key)
            .and_then(|value| track_meta(txn, &value));
        if existing
            .as_ref()
            .is_none_or(|meta| meta.title != item.title || meta.cover != item.cover)
        {
            let meta = HashMap::from([
                ("platform".to_string(), Any::from(item.platform.as_str())),
                ("source_id".to_string(), Any::from(item.source_id.as_str())),
                ("title".to_string(), Any::from(item.title.as_str())),
                ("artist".to_string(), Any::from(item.artist.as_str())),
                ("cover".to_string(), Any::from(item.cover.as_str())),
            ]);
            tracks.insert(txn, key.as_str(), Any::from(meta));
        }
        keys.push(key);
    }
    keys
}

fn dedupe_keys(keys: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    keys.retain(|key| seen.insert(key.clone()));
}

fn clean_name(name: &str) -> String {
    let name: String = name.trim().chars().take(200).collect();
    if name.is_empty() {
        UNTITLED.to_string()
    } else {
        name
    }
}

fn playlist<T: ReadTxn>(txn: &T, playlists: &MapRef, id: &str) -> Option<(MapRef, ArrayRef)> {
    let Some(Out::YMap(entry)) = playlists.get(txn, id) else {
        return None;
    };
    let Some(Out::YArray(order)) = entry.get(txn, "order") else {
        return None;
    };
    Some((entry, order))
}

fn remove_all(txn: &mut TransactionMut, list: &ArrayRef, key: &str) {
    let values = strings(txn, list);
    for index in (0..values.len()).rev() {
        if values[index] == key {
            list.remove(txn, index as u32);
        }
    }
}

fn strings<T: ReadTxn>(txn: &T, list: &ArrayRef) -> Vec<String> {
    list.iter(txn).map(|value| value.to_string(txn)).collect()
}

fn string<T: ReadTxn>(txn: &T, map: &MapRef, key: &str) -> String {
    match map.get(txn, key) {
        Some(Out::Any(Any::String(value))) => value.to_string(),
        Some(Out::YText(text)) => text.get_string(txn),
        _ => String::new(),
    }
}

fn number<T: ReadTxn>(txn: &T, map: &MapRef, key: &str) -> i64 {
    match map.get(txn, key) {
        Some(Out::Any(Any::Number(value))) => value
            .as_i64()
            .or_else(|| value.as_f64().map(|float| float as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

fn track_meta<T: ReadTxn>(_txn: &T, value: &Out) -> Option<TrackMeta> {
    let Out::Any(Any::Map(map)) = value else {
        return None;
    };
    let text = |key: &str| match map.get(key) {
        Some(Any::String(value)) => value.to_string(),
        _ => String::new(),
    };
    Some(TrackMeta {
        platform: text("platform"),
        source_id: text("source_id"),
        title: text("title"),
        artist: text("artist"),
        cover: text("cover"),
    })
}

/// The web client's `readLibrary()`.
pub fn snapshot(doc: &Doc) -> Snapshot {
    let playlists = doc.get_or_insert_map("playlists");
    let tracks = doc.get_or_insert_map("tracks");
    let taste = doc.get_or_insert_map("taste");
    let txn = doc.transact();
    let mut items = Vec::new();
    for (id, value) in playlists.iter(&txn) {
        let Out::YMap(entry) = value else { continue };
        let mut seen = std::collections::HashSet::new();
        let list: Vec<SnapshotTrack> = match entry.get(&txn, "order") {
            Some(Out::YArray(order)) => strings(&txn, &order),
            _ => Vec::new(),
        }
        .into_iter()
        .filter(|key| seen.insert(key.clone()))
        .filter_map(|key| {
            let meta = tracks
                .get(&txn, &key)
                .and_then(|value| track_meta(&txn, &value))?;
            Some(SnapshotTrack { key, meta })
        })
        .collect();
        let name = string(&txn, &entry, "name");
        let cover = string(&txn, &entry, "cover");
        items.push(SnapshotPlaylist {
            id: id.to_string(),
            name: if name.is_empty() {
                UNTITLED.to_string()
            } else {
                name
            },
            platform: string(&txn, &entry, "platform"),
            source_id: string(&txn, &entry, "source_id"),
            source_url: string(&txn, &entry, "source_url"),
            source_platform: string(&txn, &entry, "source_platform"),
            cover: if cover.is_empty() {
                list.first()
                    .map(|track| track.meta.cover.clone())
                    .unwrap_or_default()
            } else {
                cover
            },
            created_at: number(&txn, &entry, "created_at"),
            updated_at: number(&txn, &entry, "updated_at"),
            tracks: list,
        });
    }
    items.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let keys_of = |kind: &str| -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        match taste.get(&txn, kind) {
            Some(Out::YArray(list)) => strings(&txn, &list)
                .into_iter()
                .filter(|key| seen.insert(key.clone()))
                .collect(),
            _ => Vec::new(),
        }
    };
    Snapshot {
        playlists: items,
        liked: keys_of("liked"),
        less: keys_of("less"),
    }
}

/// The state of `doc` as one update, for tests and diffing.
pub fn full_state(doc: &Doc) -> Vec<u8> {
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

fn invalid(message: &str) -> crate::ApiError {
    crate::ApiError::Http {
        status: StatusCode::BAD_REQUEST,
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, title: &str) -> TrackMeta {
        TrackMeta {
            platform: "NetEase".into(),
            source_id: id.into(),
            title: title.into(),
            artist: "歌手".into(),
            cover: format!("https://img/{id}"),
        }
    }

    fn sync(from: &Doc, to: &Doc) {
        let sv = to.transact().state_vector();
        let diff = from.transact().encode_diff_v1(&sv);
        to.transact_mut()
            .apply_update(Update::decode_v1(&diff).unwrap())
            .unwrap();
    }

    fn keys(snapshot: &Snapshot, index: usize) -> Vec<String> {
        snapshot.playlists[index]
            .tracks
            .iter()
            .map(|track| track.key.clone())
            .collect()
    }

    #[test]
    fn ops_follow_the_web_library_semantics() {
        let doc = Doc::new();
        let update = apply_ops(
            &doc,
            &[
                Op::Create {
                    id: "p1".into(),
                    name: "  ".into(),
                    platform: String::new(),
                    source_id: String::new(),
                    source_url: String::new(),
                    source_platform: String::new(),
                    cover: String::new(),
                    tracks: vec![track("1", "一"), track("2", "二"), track("1", "一")],
                },
                Op::Append {
                    id: "p1".into(),
                    tracks: vec![track("2", "二"), track("3", "三")],
                },
                Op::Move {
                    id: "p1".into(),
                    key: "netease:3".into(),
                    after: None,
                },
                Op::Move {
                    id: "p1".into(),
                    key: "netease:1".into(),
                    after: Some("netease:2".into()),
                },
                Op::Taste {
                    kind: TasteKind::Liked,
                    key: "netease:2".into(),
                },
                Op::Taste {
                    kind: TasteKind::Liked,
                    key: "netease:1".into(),
                },
                Op::Taste {
                    kind: TasteKind::Liked,
                    key: "netease:2".into(),
                },
            ],
            1_700_000_000_000,
        )
        .unwrap();
        assert!(update.is_some());
        let snapshot = snapshot(&doc);
        assert_eq!(snapshot.playlists[0].name, "未命名歌单");
        assert_eq!(snapshot.playlists[0].cover, "https://img/1");
        assert_eq!(snapshot.playlists[0].created_at, 1_700_000_000_000);
        assert_eq!(keys(&snapshot, 0), ["netease:3", "netease:2", "netease:1"]);
        assert_eq!(snapshot.liked, ["netease:2", "netease:1"]);

        apply_ops(
            &doc,
            &[
                Op::Rename {
                    id: "p1".into(),
                    name: "通勤".into(),
                },
                Op::Remove {
                    id: "p1".into(),
                    key: "netease:2".into(),
                },
            ],
            1_700_000_000_500,
        )
        .unwrap();
        let snapshot = super::snapshot(&doc);
        assert_eq!(snapshot.playlists[0].name, "通勤");
        assert_eq!(snapshot.playlists[0].updated_at, 1_700_000_000_500);
        assert_eq!(keys(&snapshot, 0), ["netease:3", "netease:1"]);

        apply_ops(&doc, &[Op::Delete { id: "p1".into() }], 0).unwrap();
        assert!(super::snapshot(&doc).playlists.is_empty());
    }

    #[test]
    fn no_op_requests_produce_no_update() {
        let doc = Doc::new();
        assert_eq!(
            apply_ops(
                &doc,
                &[Op::Rename {
                    id: "missing".into(),
                    name: "x".into()
                }],
                0
            )
            .unwrap(),
            None
        );
        assert_eq!(apply_ops(&doc, &[], 0).unwrap(), None);
    }

    #[test]
    fn updates_from_a_scratch_replica_merge_into_other_replicas() {
        // The account document, already edited by a web client.
        let cloud = Doc::new();
        apply_ops(
            &cloud,
            &[Op::Create {
                id: "p1".into(),
                name: "网页".into(),
                platform: String::new(),
                source_id: String::new(),
                source_url: String::new(),
                source_platform: String::new(),
                cover: String::new(),
                tracks: vec![track("1", "一")],
            }],
            1,
        )
        .unwrap();
        let web = Doc::new();
        sync(&cloud, &web);

        // A native client appends through a scratch copy of the server state.
        let scratch = replica(&full_state(&cloud)).unwrap();
        let update = apply_ops(
            &scratch,
            &[Op::Append {
                id: "p1".into(),
                tracks: vec![track("2", "二")],
            }],
            2,
        )
        .unwrap()
        .unwrap();
        // Meanwhile the web client added another song.
        apply_ops(
            &web,
            &[Op::Append {
                id: "p1".into(),
                tracks: vec![track("3", "三")],
            }],
            3,
        )
        .unwrap();

        cloud
            .transact_mut()
            .apply_update(Update::decode_v1(&update).unwrap())
            .unwrap();
        sync(&web, &cloud);
        sync(&cloud, &web);
        let merged = snapshot(&cloud);
        assert_eq!(merged, snapshot(&web));
        let mut got = keys(&merged, 0);
        got.sort();
        assert_eq!(got, ["netease:1", "netease:2", "netease:3"]);
    }
}
