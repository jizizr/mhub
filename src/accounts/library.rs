//! One Yjs (yrs) document per account holds its playlists and taste lists.
//! Clients exchange y-sync messages with it; the CRDT guarantees that any
//! replica, including one edited offline after its session expired,
//! converges without duplicating or losing edits.

use super::store::AccountStore;
use crate::{ApiError, ApiResult};
use axum::http::StatusCode;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::{Mutex, broadcast};
use yrs::{
    Doc, ReadTxn, StateVector, Transact, Update,
    updates::{decoder::Decode, encoder::Encode},
};

/// A single client update larger than this is rejected outright.
pub const MAX_UPDATE_BYTES: usize = 2 * 1024 * 1024;
const COMPACT_EVERY_UPDATES: usize = 200;
const COMPACT_EVERY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub enum LibraryEvent {
    /// A library update; `origin` is the connection that sent it.
    Update { origin: u64, data: Arc<Vec<u8>> },
    /// A device's latest playback session (JSON), for hand-off.
    Playback {
        device_id: String,
        payload: Arc<String>,
    },
    /// A session was revoked or logged out; its live connection must close.
    Revoked { token_hash: Arc<String> },
}

struct LibraryState {
    doc: Doc,
    pending_updates: usize,
    pending_bytes: usize,
    last_update_id: i64,
    approx_size: usize,
}

pub struct Library {
    user_id: i64,
    state: Mutex<LibraryState>,
    events: broadcast::Sender<LibraryEvent>,
}

impl Library {
    pub fn subscribe(&self) -> broadcast::Receiver<LibraryEvent> {
        self.events.subscribe()
    }

    pub fn publish(&self, event: LibraryEvent) {
        let _ = self.events.send(event);
    }

    pub async fn state_vector(&self) -> Vec<u8> {
        self.state
            .lock()
            .await
            .doc
            .transact()
            .state_vector()
            .encode_v1()
    }

    /// Everything the holder of `state_vector` is missing.
    pub async fn diff(&self, state_vector: &[u8]) -> ApiResult<Vec<u8>> {
        let state_vector =
            StateVector::decode_v1(state_vector).map_err(|_| bad_request("同步状态无效"))?;
        Ok(self
            .state
            .lock()
            .await
            .doc
            .transact()
            .encode_diff_v1(&state_vector))
    }

    pub async fn full_state(&self) -> Vec<u8> {
        self.state
            .lock()
            .await
            .doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }
}

pub struct LibraryHub {
    store: Arc<dyn AccountStore>,
    limit_bytes: usize,
    libraries: StdMutex<HashMap<i64, Arc<Library>>>,
    loading: Mutex<()>,
}

impl LibraryHub {
    pub fn new(store: Arc<dyn AccountStore>, limit_bytes: usize) -> Self {
        Self {
            store,
            limit_bytes,
            libraries: StdMutex::new(HashMap::new()),
            loading: Mutex::new(()),
        }
    }

    /// Notify live connections without loading a document that is not open.
    pub fn loaded(&self, user_id: i64) -> Option<Arc<Library>> {
        self.libraries.lock().unwrap().get(&user_id).cloned()
    }

    pub async fn open(&self, user_id: i64) -> ApiResult<Arc<Library>> {
        if let Some(library) = self.libraries.lock().unwrap().get(&user_id) {
            return Ok(library.clone());
        }
        // Serialize loads so two first connections build one document.
        let _guard = self.loading.lock().await;
        if let Some(library) = self.libraries.lock().unwrap().get(&user_id) {
            return Ok(library.clone());
        }
        let stored = self.store.library(user_id).await?;
        let doc = Doc::new();
        let mut approx_size = 0;
        let mut last_update_id = 0;
        {
            let mut txn = doc.transact_mut();
            for (index, bytes) in stored
                .snapshot
                .iter()
                .chain(stored.updates.iter().map(|(_, data)| data))
                .enumerate()
            {
                approx_size += bytes.len();
                match Update::decode_v1(bytes) {
                    Ok(update) => {
                        if let Err(error) = txn.apply_update(update) {
                            tracing::warn!(
                                user_id,
                                index,
                                "skipping unappliable library update: {error}"
                            );
                        }
                    }
                    Err(error) => tracing::warn!(
                        user_id,
                        index,
                        "skipping undecodable library update: {error}"
                    ),
                }
            }
            if let Some((id, _)) = stored.updates.last() {
                last_update_id = *id;
            }
        }
        let (events, _) = broadcast::channel(256);
        let library = Arc::new(Library {
            user_id,
            state: Mutex::new(LibraryState {
                doc,
                pending_updates: stored.updates.len(),
                pending_bytes: stored.updates.iter().map(|(_, data)| data.len()).sum(),
                last_update_id,
                approx_size,
            }),
            events,
        });
        self.libraries
            .lock()
            .unwrap()
            .insert(user_id, library.clone());
        Ok(library)
    }

    /// Apply, persist and broadcast one client update.
    pub async fn apply(
        &self,
        library: &Arc<Library>,
        data: Vec<u8>,
        origin: u64,
        now: i64,
    ) -> ApiResult<()> {
        if data.len() > MAX_UPDATE_BYTES {
            return Err(too_large());
        }
        // `yrs::Update` is not Send: decode it only while holding the lock.
        let mut state = library.state.lock().await;
        if state.approx_size + data.len() > self.limit_bytes {
            return Err(too_large());
        }
        let update = Update::decode_v1(&data).map_err(|_| bad_request("同步数据无效"))?;
        state
            .doc
            .transact_mut()
            .apply_update(update)
            .map_err(|_| bad_request("同步数据无法合并"))?;
        let id = match self
            .store
            .append_library_update(library.user_id, &data, now)
            .await
        {
            Ok(id) => id,
            Err(error) => {
                // Memory now holds an unpersisted change; reload from storage.
                self.libraries.lock().unwrap().remove(&library.user_id);
                return Err(error);
            }
        };
        state.last_update_id = id;
        state.pending_updates += 1;
        state.pending_bytes += data.len();
        state.approx_size += data.len();
        library.publish(LibraryEvent::Update {
            origin,
            data: Arc::new(data),
        });
        if state.pending_updates >= COMPACT_EVERY_UPDATES
            || state.pending_bytes >= COMPACT_EVERY_BYTES
        {
            let snapshot = state
                .doc
                .transact()
                .encode_state_as_update_v1(&StateVector::default());
            match self
                .store
                .compact_library(library.user_id, &snapshot, state.last_update_id, now)
                .await
            {
                Ok(()) => {
                    state.pending_updates = 0;
                    state.pending_bytes = 0;
                    state.approx_size = snapshot.len();
                }
                Err(error) => tracing::warn!(
                    user_id = library.user_id,
                    "library compaction failed: {error}"
                ),
            }
        }
        Ok(())
    }

    /// Drop an idle document; the next connection reloads it.
    pub fn release(&self, library: &Arc<Library>) {
        let mut libraries = self.libraries.lock().unwrap();
        if library.events.receiver_count() == 0 && Arc::strong_count(library) <= 2 {
            libraries.remove(&library.user_id);
        }
    }
}

fn bad_request(message: &str) -> ApiError {
    ApiError::Http {
        status: StatusCode::BAD_REQUEST,
        message: message.to_string(),
    }
}

fn too_large() -> ApiError {
    ApiError::Http {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        message: "资料库已超过容量上限".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::store::MemoryStore;
    use yrs::{
        Array, ArrayPrelim, GetString, Map, MapPrelim, Out, Text, TextPrelim, types::ToJson,
    };

    fn replica() -> Doc {
        Doc::new()
    }

    fn sync(from: &Doc, to: &Doc) {
        let sv = to.transact().state_vector();
        let diff = from.transact().encode_diff_v1(&sv);
        to.transact_mut()
            .apply_update(Update::decode_v1(&diff).unwrap())
            .unwrap();
    }

    fn order(doc: &Doc, playlist: &str) -> Vec<String> {
        let playlists = doc.get_or_insert_map("playlists");
        let txn = doc.transact();
        let Some(Out::YMap(entry)) = playlists.get(&txn, playlist) else {
            return Vec::new();
        };
        let Some(Out::YArray(order)) = entry.get(&txn, "order") else {
            return Vec::new();
        };
        order
            .iter(&txn)
            .map(|value| value.to_string(&txn))
            .collect()
    }

    #[test]
    fn offline_edits_on_a_cloud_replica_merge_without_duplicates_or_losses() {
        let cloud = replica();
        {
            let playlists = cloud.get_or_insert_map("playlists");
            let mut txn = cloud.transact_mut();
            let entry = playlists.insert(&mut txn, "p1", MapPrelim::default());
            entry.insert(&mut txn, "name", TextPrelim::new("通勤"));
            entry.insert(&mut txn, "order", ArrayPrelim::from(["a", "b", "c"]));
        }
        // Device B was linked, then its session expired and it kept editing.
        let device = replica();
        sync(&cloud, &device);
        {
            let playlists = device.get_or_insert_map("playlists");
            let mut txn = device.transact_mut();
            let Some(Out::YMap(entry)) = playlists.get(&txn, "p1") else {
                panic!()
            };
            let Some(Out::YArray(order)) = entry.get(&txn, "order") else {
                panic!()
            };
            order.remove(&mut txn, 1); // delete "b" offline
            order.push_back(&mut txn, "d");
        }
        // Meanwhile another device added to the same playlist.
        {
            let playlists = cloud.get_or_insert_map("playlists");
            let mut txn = cloud.transact_mut();
            let Some(Out::YMap(entry)) = playlists.get(&txn, "p1") else {
                panic!()
            };
            let Some(Out::YArray(order)) = entry.get(&txn, "order") else {
                panic!()
            };
            order.insert(&mut txn, 0, "z");
        }
        // Re-login: exchange both directions, twice to prove idempotence.
        for _ in 0..2 {
            sync(&device, &cloud);
            sync(&cloud, &device);
        }
        assert_eq!(order(&cloud, "p1"), ["z", "a", "c", "d"]);
        assert_eq!(order(&device, "p1"), order(&cloud, "p1"));
        let name = {
            let playlists = cloud.get_or_insert_map("playlists");
            let txn = cloud.transact();
            let Some(Out::YMap(entry)) = playlists.get(&txn, "p1") else {
                panic!()
            };
            let Some(Out::YText(text)) = entry.get(&txn, "name") else {
                panic!()
            };
            text.get_string(&txn)
        };
        assert_eq!(name, "通勤");
        let _ = cloud.to_json(&cloud.transact());
    }

    #[tokio::test]
    async fn hub_persists_compacts_and_reloads_the_same_state() {
        let store: Arc<dyn AccountStore> = Arc::new(MemoryStore::default());
        let hub = LibraryHub::new(store.clone(), 16 * 1024 * 1024);
        let library = hub.open(7).await.unwrap();
        let client = replica();
        let list = client.get_or_insert_array("liked");
        for index in 0..(COMPACT_EVERY_UPDATES + 5) {
            let before = client.transact().state_vector();
            list.push_back(&mut client.transact_mut(), index as i64);
            let update = client.transact().encode_diff_v1(&before);
            hub.apply(&library, update, 1, 0).await.unwrap();
        }
        let stored = store.library(7).await.unwrap();
        assert!(stored.snapshot.is_some(), "compaction wrote a snapshot");
        assert!(
            stored.updates.len() < COMPACT_EVERY_UPDATES,
            "compacted updates were dropped"
        );

        let reloaded = LibraryHub::new(store, 16 * 1024 * 1024)
            .open(7)
            .await
            .unwrap();
        let fresh = replica();
        let state = reloaded.full_state().await;
        fresh
            .transact_mut()
            .apply_update(Update::decode_v1(&state).unwrap())
            .unwrap();
        let liked = fresh.get_or_insert_array("liked");
        assert_eq!(
            liked.len(&fresh.transact()),
            (COMPACT_EVERY_UPDATES + 5) as u32
        );
    }

    #[tokio::test]
    async fn oversized_and_invalid_updates_are_rejected() {
        let hub = LibraryHub::new(Arc::new(MemoryStore::default()), 1024);
        let library = hub.open(1).await.unwrap();
        assert!(hub.apply(&library, vec![0xff; 16], 1, 0).await.is_err());
        let client = replica();
        let text = client.get_or_insert_text("t");
        text.push(&mut client.transact_mut(), &"x".repeat(4096));
        let update = client
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        assert!(matches!(
            hub.apply(&library, update, 1, 0).await,
            Err(ApiError::Http {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                ..
            })
        ));
    }
}
