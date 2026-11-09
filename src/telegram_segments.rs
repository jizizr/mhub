//! Reads a Telegram file by byte offset over MTProto.
//!
//! The Bot API has no partial download: a local server's `getFile` answers
//! only once the whole file is on its disk. Signing in to Telegram directly
//! as the same bot lets a response start with the first 512 KiB instead.
//! Bytes go straight from Telegram to the listener: nothing is written to
//! disk, so a song cached on Telegram is downloaded once per request rather
//! than once by us and again by the Bot API server. Everything here is
//! optional: without `[bot].api_id` / `api_hash`, or when anything fails
//! before the first chunk, callers keep using `getFile`.

use crate::settings::SETTINGS;
use axum::body::Bytes;
use base64::Engine;
use futures::{
    FutureExt,
    stream::{self, BoxStream, StreamExt},
};
use grammers_client::{
    Client, SenderPool,
    media::Downloadable,
    session::{Session, SessionData, storages::MemorySession, types::DcOption},
    tl,
};
use serde::{Deserialize, Serialize};
use std::{
    io,
    net::{SocketAddrV4, SocketAddrV6},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

/// `upload.getFile` serves at most this much per request, at offsets that are
/// multiples of it.
const CHUNK: u64 = 512 * 1024;
/// A first chunk slower than this is not worth waiting for: the caller falls
/// back to `getFile` instead.
const FIRST_CHUNK_TIMEOUT: Duration = Duration::from_secs(8);
/// Chunks requested at once. Sending stays in order; this only keeps the
/// next few on their way while one is being sent, which is what makes whole
/// file reads (a browser caching a song, the next-track prefetch) fast.
const PARALLEL: usize = 4;
const CHUNK_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK_RETRIES: u32 = 2;
/// How long a failed sign-in keeps the direct route switched off.
const SIGN_IN_RETRY: Duration = Duration::from_secs(5 * 60);

/// A document as `upload.getFile` addresses it, read out of a Bot API file id.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Document {
    dc_id: i32,
    id: i64,
    access_hash: i64,
    file_reference: Vec<u8>,
    size: u64,
}

impl Downloadable for Document {
    fn to_raw_input_location(&self) -> Option<tl::enums::InputFileLocation> {
        Some(
            tl::types::InputDocumentFileLocation {
                id: self.id,
                access_hash: self.access_hash,
                file_reference: self.file_reference.clone(),
                thumb_size: String::new(),
            }
            .into(),
        )
    }

    fn size(&self) -> Option<usize> {
        usize::try_from(self.size).ok()
    }

    fn dc_id(&self) -> Option<i32> {
        Some(self.dc_id)
    }
}

const FILE_REFERENCE_FLAG: u32 = 1 << 25;
const WEB_LOCATION_FLAG: u32 = 1 << 24;
/// Voice note, video, document, sticker, audio, animation, video note and
/// document-as-file: TDLib's file types stored as documents with an id and
/// an access hash.
const DOCUMENT_TYPES: [u32; 8] = [3, 4, 5, 8, 9, 10, 13, 17];

/// Decode the document a Bot API `file_id` points at. `None` for photos,
/// thumbnails, web files and anything this does not recognise.
fn decode_file_id(file_id: &str) -> Option<(i32, i64, i64, Vec<u8>)> {
    let packed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(file_id.trim().trim_end_matches('='))
        .ok()?;
    // Runs of zero bytes are stored as a zero followed by their length.
    let mut data = Vec::with_capacity(packed.len() * 2);
    let mut bytes = packed.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == 0 {
            let run = bytes.next()?;
            data.extend(std::iter::repeat_n(0u8, run as usize));
        } else {
            data.push(byte);
        }
    }
    // The last byte is the format version; version 4 keeps a sub-version
    // before it.
    let version = data.pop()?;
    if version != 4 {
        return None;
    }
    data.pop()?;

    let mut rest = data.as_slice();
    let mut take = |len: usize| -> Option<&[u8]> {
        if rest.len() < len {
            return None;
        }
        let (head, tail) = rest.split_at(len);
        rest = tail;
        Some(head)
    };
    let kind = u32::from_le_bytes(take(4)?.try_into().ok()?);
    let dc_id = i32::from_le_bytes(take(4)?.try_into().ok()?);
    if kind & WEB_LOCATION_FLAG != 0 || !DOCUMENT_TYPES.contains(&(kind & 0x00ff_ffff)) {
        return None;
    }
    let file_reference = if kind & FILE_REFERENCE_FLAG != 0 {
        // A TL string: a length byte (or 254 and three length bytes), the
        // bytes, then padding to a multiple of four.
        let first = take(1)?[0] as usize;
        let (len, header) = if first == 254 {
            let long = take(3)?;
            (
                long[0] as usize | (long[1] as usize) << 8 | (long[2] as usize) << 16,
                4,
            )
        } else {
            (first, 1)
        };
        let reference = take(len)?.to_vec();
        take((4 - (header + len) % 4) % 4)?;
        reference
    } else {
        Vec::new()
    };
    let id = i64::from_le_bytes(take(8)?.try_into().ok()?);
    let access_hash = i64::from_le_bytes(take(8)?.try_into().ok()?);
    if !rest.is_empty() || !(1..=5).contains(&dc_id) {
        return None;
    }
    Some((dc_id, id, access_hash, file_reference))
}

/// What a file id that `decode_file_id` refused looks like, without the
/// identifiers themselves: its version bytes, type, data centre and length.
fn file_id_shape(file_id: &str) -> String {
    let Ok(packed) = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(file_id.trim().trim_end_matches('='))
    else {
        return format!("not base64, {} chars", file_id.len());
    };
    let mut data = Vec::new();
    let mut bytes = packed.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == 0 {
            data.extend(std::iter::repeat_n(0u8, bytes.next().unwrap_or(0) as usize));
        } else {
            data.push(byte);
        }
    }
    let word = |at: usize| {
        data.get(at..at + 4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
    };
    format!(
        "len={} tail={:?} kind={:?} dc={:?}",
        data.len(),
        data.get(data.len().saturating_sub(2)..),
        word(0).map(|kind| format!("{kind:#x}")),
        word(4),
    )
}

/// The keys that keep the bot signed in between restarts. Signing in again
/// each time would run into Telegram's flood limits.
#[derive(Serialize, Deserialize)]
struct StoredSession {
    home_dc: i32,
    dcs: Vec<StoredDc>,
}

#[derive(Serialize, Deserialize)]
struct StoredDc {
    id: i32,
    ipv4: SocketAddrV4,
    ipv6: SocketAddrV6,
    /// Hex of the 256-byte authorization key.
    auth_key: String,
}

fn session_path() -> PathBuf {
    PathBuf::from(std::env::var("MUSIC_API_DATA_DIR").unwrap_or_else(|_| "data".to_string()))
        .join("telegram-mtproto-session.json")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<[u8; 256]> {
    if text.len() != 512 {
        return None;
    }
    let mut key = [0u8; 256];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(key)
}

fn load_session() -> MemorySession {
    let mut data = SessionData::default();
    let stored = std::fs::read(session_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<StoredSession>(&bytes).ok());
    if let Some(stored) = stored {
        data.home_dc = stored.home_dc;
        for dc in stored.dcs {
            let Some(auth_key) = unhex(&dc.auth_key) else {
                continue;
            };
            data.dc_options.insert(
                dc.id,
                DcOption {
                    id: dc.id,
                    ipv4: dc.ipv4,
                    ipv6: dc.ipv6,
                    auth_key: Some(auth_key),
                },
            );
        }
    }
    MemorySession::from(data)
}

fn stored_session(session: &MemorySession) -> Option<StoredSession> {
    let dcs = (1..=5)
        .filter_map(|id| session.dc_option(id).ok().flatten())
        .filter_map(|dc| {
            Some(StoredDc {
                id: dc.id,
                ipv4: dc.ipv4,
                ipv6: dc.ipv6,
                auth_key: hex(&dc.auth_key?),
            })
        })
        .collect();
    Some(StoredSession {
        home_dc: session.home_dc_id().ok()?,
        dcs,
    })
}

/// Write the session when it holds a key the file does not. The file is as
/// sensitive as the bot token: owner-only, in the private data directory.
fn save_session(connection: &Connection) {
    let Some(stored) = stored_session(&connection.session) else {
        return;
    };
    let keys = stored.dcs.len();
    if connection
        .saved_keys
        .swap(keys, std::sync::atomic::Ordering::Relaxed)
        == keys
    {
        return;
    }
    let Ok(json) = serde_json::to_vec(&stored) else {
        return;
    };
    let path = session_path();
    let temporary = path.with_extension("json.tmp");
    let written = (|| -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        options.open(&temporary)?.write_all(&json)?;
        std::fs::rename(&temporary, &path)
    })();
    if let Err(err) = written {
        tracing::warn!("failed to save the Telegram MTProto session: {err}");
    }
}

struct Connection {
    client: Client,
    session: Arc<MemorySession>,
    saved_keys: std::sync::atomic::AtomicUsize,
}

enum State {
    Idle,
    Ready(Arc<Connection>),
    Failed(Instant),
}

lazy_static::lazy_static! {
    static ref STATE: tokio::sync::Mutex<State> = tokio::sync::Mutex::new(State::Idle);
}

fn credentials() -> Option<(i32, &'static str, &'static str)> {
    let api_id = SETTINGS.bot.api_id.filter(|id| *id > 0)?;
    let api_hash = SETTINGS.bot.api_hash.as_deref().map(str::trim)?;
    let token = SETTINGS.bot.token.trim();
    (!api_hash.is_empty() && !token.is_empty()).then_some((api_id, api_hash, token))
}

/// Whether direct reads are configured at all.
pub(crate) fn enabled() -> bool {
    credentials().is_some()
}

/// Sign in in the background at startup, so the first cold song does not
/// wait for it.
pub(crate) fn warm_up() {
    if enabled() {
        tokio::spawn(async {
            connection().await;
        });
    }
}

async fn connection() -> Option<Arc<Connection>> {
    let (api_id, api_hash, token) = credentials()?;
    let mut state = STATE.lock().await;
    match &*state {
        State::Ready(connection) => return Some(connection.clone()),
        State::Failed(at) if at.elapsed() < SIGN_IN_RETRY => return None,
        _ => {}
    }
    let session = Arc::new(load_session());
    let SenderPool { runner, handle, .. } = SenderPool::new(session.clone(), api_id);
    let client = Client::new(handle);
    // Dropping the updates receiver is fine: the Bot API server keeps its
    // own session and remains the one that handles the bot's updates.
    tokio::spawn(runner.run());
    let signed_in = async {
        if client.is_authorized().await? {
            return Ok(());
        }
        client.bot_sign_in(token, api_hash).await.map(|_| ())
    };
    match tokio::time::timeout(Duration::from_secs(20), signed_in).await {
        Ok(Ok(())) => {
            let connection = Arc::new(Connection {
                client,
                session,
                saved_keys: std::sync::atomic::AtomicUsize::new(0),
            });
            save_session(&connection);
            tracing::info!("Telegram direct file reads are ready");
            *state = State::Ready(connection.clone());
            Some(connection)
        }
        outcome => {
            let reason = match outcome {
                Ok(Err(err)) => err.to_string(),
                _ => "timed out".to_string(),
            };
            tracing::warn!("Telegram direct file reads unavailable: {reason}");
            client.disconnect();
            *state = State::Failed(Instant::now());
            None
        }
    }
}

/// Fetch chunk `index` of `document`: exactly the bytes the recorded size
/// says it holds, retried `retries` times after a timeout or network error.
async fn fetch_chunk(
    connection: Arc<Connection>,
    document: Arc<Document>,
    index: u64,
    wait: Duration,
    retries: u32,
) -> io::Result<Bytes> {
    let expected = CHUNK.min(document.size - index * CHUNK) as usize;
    let mut attempt = 0;
    loop {
        // A fresh iterator per chunk: each moves its own request to the
        // file's data centre and exports the bot's authorization there.
        let mut download = connection
            .client
            .iter_download(document.as_ref())
            .skip_chunks(index as i32);
        let failure = match tokio::time::timeout(wait, download.next()).await {
            Ok(Ok(Some(bytes))) if bytes.len() == expected => return Ok(Bytes::from(bytes)),
            // A length other than the recorded size implies is a wrong
            // recorded size, which no retry changes.
            Ok(Ok(Some(bytes))) if bytes.len() > expected => {
                return Err(io::Error::other("Telegram 文件长于记录的长度"));
            }
            Ok(Ok(_)) => return Err(io::Error::other("Telegram 文件短于记录的长度")),
            Ok(Err(err)) => err.to_string(),
            Err(_) => "Telegram 分段读取超时".to_string(),
        };
        if attempt >= retries {
            return Err(io::Error::other(failure));
        }
        attempt += 1;
        tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
    }
}

/// The bytes `start..=end` of a Telegram document of `size` bytes, starting
/// as soon as Telegram returns the first chunk. Later chunks are requested
/// `PARALLEL` at a time ahead of the one being sent, and sent in order.
///
/// Returns `None` when the direct route cannot serve this file, having sent
/// nothing, so the caller can fall back to `getFile`. An error after that
/// ends the body with an error rather than a clean, short EOF.
pub(crate) async fn stream(
    file_id: &str,
    size: u64,
    start: u64,
    end: u64,
) -> Option<BoxStream<'static, io::Result<Bytes>>> {
    if size == 0 || start > end || end >= size {
        tracing::warn!(
            size,
            start,
            end,
            "Telegram direct read got an impossible range"
        );
        return None;
    }
    let Some((dc_id, id, access_hash, file_reference)) = decode_file_id(file_id) else {
        tracing::warn!(
            shape = %file_id_shape(file_id),
            "Telegram file id is not a readable document; using getFile"
        );
        return None;
    };
    let Some(connection) = connection().await else {
        tracing::warn!("Telegram direct reads not signed in; using getFile");
        return None;
    };
    let document = Arc::new(Document {
        dc_id,
        id,
        access_hash,
        file_reference,
        size,
    });
    let (first, last) = (start / CHUNK, end / CHUNK);
    let chunks = {
        let (connection, document) = (connection.clone(), document.clone());
        stream::iter(first..=last)
            .map(move |index| {
                // Only the first chunk decides whether to fall back, so it
                // gets one quick try; later ones may wait and retry.
                let (wait, retries) = if index == first {
                    (FIRST_CHUNK_TIMEOUT, 0)
                } else {
                    (CHUNK_TIMEOUT, CHUNK_RETRIES)
                };
                fetch_chunk(connection.clone(), document.clone(), index, wait, retries)
                    .map(move |bytes| (index, bytes))
            })
            .buffered(PARALLEL)
    };
    let mut chunks = Box::pin(chunks);
    let first_chunk = match chunks.next().await {
        Some((_, Ok(bytes))) => bytes,
        Some((_, Err(err))) => {
            tracing::warn!("Telegram direct read failed before the first chunk: {err}");
            return None;
        }
        None => return None,
    };
    // Reading from another data centre creates a key worth keeping.
    save_session(&connection);
    // Cut each chunk down to the part of `start..=end` it holds.
    let trim = move |index: u64, bytes: Bytes| {
        let offset = index * CHUNK;
        let from = start.saturating_sub(offset) as usize;
        let to = ((end + 1 - offset) as usize).min(bytes.len());
        bytes.slice(from.min(to)..to)
    };
    let head = stream::once(futures::future::ready(Ok(trim(first, first_chunk))));
    let rest = chunks.map(move |(index, bytes)| bytes.map(|bytes| trim(index, bytes)));
    // Stop after the first error: the body must not continue past a gap.
    let mut failed = false;
    Some(
        head.chain(rest)
            .take_while(move |item| {
                let keep = !failed;
                failed |= item.is_err();
                futures::future::ready(keep)
            })
            .boxed(),
    )
}

/// The whole document, read over MTProto. `None` when the direct route
/// cannot serve it (nothing was read); an error when it failed partway.
pub(crate) async fn read_all(file_id: &str, size: u64) -> Option<io::Result<Vec<u8>>> {
    let mut chunks = stream(file_id, size, 0, size.checked_sub(1)?).await?;
    let mut bytes = Vec::with_capacity(size as usize);
    while let Some(chunk) = chunks.next().await {
        match chunk {
            Ok(chunk) => bytes.extend_from_slice(&chunk),
            Err(err) => return Some(Err(err)),
        }
    }
    Some(Ok(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Bot API file id built the way TDLib packs one.
    fn encode(
        kind: u32,
        dc_id: i32,
        reference: Option<&[u8]>,
        id: i64,
        access_hash: i64,
    ) -> String {
        let mut data = Vec::new();
        let flags = if reference.is_some() {
            FILE_REFERENCE_FLAG
        } else {
            0
        };
        data.extend_from_slice(&(kind | flags).to_le_bytes());
        data.extend_from_slice(&dc_id.to_le_bytes());
        if let Some(reference) = reference {
            if reference.len() < 254 {
                data.push(reference.len() as u8);
            } else {
                data.push(254);
                data.extend_from_slice(&(reference.len() as u32).to_le_bytes()[..3]);
            }
            data.extend_from_slice(reference);
            while data.len() % 4 != 0 {
                data.push(0);
            }
        }
        data.extend_from_slice(&id.to_le_bytes());
        data.extend_from_slice(&access_hash.to_le_bytes());
        data.push(47);
        data.push(4);
        let mut packed = Vec::new();
        let mut zeros = 0u8;
        for byte in data {
            if byte == 0 && zeros < 250 {
                zeros += 1;
                continue;
            }
            if zeros > 0 {
                packed.extend_from_slice(&[0, zeros]);
                zeros = 0;
            }
            if byte == 0 {
                zeros = 1;
            } else {
                packed.push(byte);
            }
        }
        if zeros > 0 {
            packed.extend_from_slice(&[0, zeros]);
        }
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(packed)
    }

    #[test]
    fn decodes_audio_and_document_file_ids() {
        let reference = [
            1u8, 0, 0, 0, 9, 8, 7, 0, 0, 5, 4, 3, 2, 1, 0, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6,
        ];
        let id = encode(9, 5, Some(&reference), 6_244_000_000_000_123_456, -77);
        assert_eq!(
            decode_file_id(&id),
            Some((5, 6_244_000_000_000_123_456, -77, reference.to_vec()))
        );
        let id = encode(5, 2, None, 42, 0);
        assert_eq!(decode_file_id(&id), Some((2, 42, 0, Vec::new())));
        let long = vec![3u8; 300];
        let id = encode(9, 1, Some(&long), 1, 2);
        assert_eq!(decode_file_id(&id), Some((1, 1, 2, long)));
    }

    #[test]
    fn rejects_what_is_not_a_document() {
        // A photo, a web file, a bad data centre and plain garbage.
        assert_eq!(decode_file_id(&encode(2, 5, None, 1, 2)), None);
        assert_eq!(
            decode_file_id(&encode(9 | WEB_LOCATION_FLAG, 5, None, 1, 2)),
            None
        );
        assert_eq!(decode_file_id(&encode(9, 9, None, 1, 2)), None);
        assert_eq!(decode_file_id("not a file id"), None);
        assert_eq!(decode_file_id(""), None);
        let mut truncated = encode(9, 5, Some(&[1, 2, 3]), 1, 2);
        truncated.truncate(truncated.len() - 6);
        assert_eq!(decode_file_id(&truncated), None);
    }

    #[test]
    fn authorization_keys_survive_a_round_trip() {
        let key: Vec<u8> = (0..256).map(|index| index as u8).collect();
        assert_eq!(unhex(&hex(&key)).map(|key| key.to_vec()), Some(key));
        assert_eq!(unhex("abcd"), None);
    }
}
