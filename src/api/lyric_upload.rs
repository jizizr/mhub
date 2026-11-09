//! Lyrics in the songs uploaded to Telegram, so clients that read them from
//! the file show them while playing (see `lyric_tags`).
//!
//! New uploads are tagged on their way out. Rows uploaded before that, or
//! whose lyrics could not be fetched in time (`lyrics_tag` 0), are tagged the
//! next time the bot looks them up, which waits for it when the bot is about
//! to send the song (`wait_for_lyrics`): the file is fetched back from
//! Telegram (no platform download, no quota), tagged, uploaded once more,
//! and the row is swapped to the new file only if it still holds the old
//! one. A re-tag slower than the wait finishes in the background while that
//! request gets the old file.

use super::*;
use crate::{
    cache::replace_music_cache_file,
    lyric_tags::{self, LYRICS_TAGGED},
};
use lazy_static::lazy_static;
use std::{sync::Mutex, time::Instant};
use tokio::sync::{Semaphore, watch};

/// Native lyrics already give up after 8 s; this bounds the online fallback.
const LYRICS_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a bot request waits for its song's re-tag; well inside the
/// gateway client's 60 s request timeout.
const RETAG_WAIT: Duration = Duration::from_secs(30);
/// A song whose re-tag failed is not tried again before this.
const RETAG_RETRY_AFTER: Duration = Duration::from_secs(6 * 60 * 60);
const RETAG_MEMORY_MAX: usize = 4096;

/// What a finished re-tag hands its waiters: the row to send now, or
/// `None` to keep the one they have.
type Outcome = Option<MusicCache>;

enum Retag {
    Running(watch::Receiver<Option<Outcome>>),
    Failed(Instant),
}

lazy_static! {
    /// One re-tag in flight per upload, joined by every request for it, and
    /// when the last one failed.
    static ref RETAGS: Mutex<HashMap<String, Retag>> = Mutex::new(HashMap::new());
}
/// Each re-tag holds a whole song in memory.
static RETAG_SLOTS: Semaphore = Semaphore::const_new(2);

pub(super) struct PreparedAudio {
    pub audio: Vec<u8>,
    /// What to store as the row's `lyrics_tag`.
    pub lyrics_tag: i8,
    /// Whether `audio` differs from the bytes passed in.
    pub embedded: bool,
}

impl PreparedAudio {
    fn unchanged(audio: Vec<u8>, lyrics_tag: i8) -> Self {
        Self {
            audio,
            lyrics_tag,
            embedded: false,
        }
    }
}

/// The text a song's file should carry, in Simplified Chinese.
enum Fetched {
    Text(String),
    /// The song has no lyrics: final.
    Nothing,
    /// The lookup failed or was too slow: try again later.
    Unavailable,
}

async fn fetch_text(cache: &MusicCache) -> Fetched {
    let Some(platform) = music::MusicPlatform::from_alias(&cache.platform) else {
        return Fetched::Nothing;
    };
    let lookup = music::lyrics_for_track(
        platform,
        &cache.track_id,
        &cache.song,
        &cache.singer,
        &cache.album,
        cache.duration.map(|duration| duration.max(0) as u32),
    );
    let lyrics = match tokio::time::timeout(LYRICS_FETCH_TIMEOUT, lookup).await {
        Ok(Ok(Some(lyrics))) => lyrics,
        Ok(Ok(None)) => return Fetched::Nothing,
        Ok(Err(error)) => {
            tracing::warn!(
                platform = %cache.platform,
                track_id = %cache.track_id,
                "lyrics for the Telegram upload unavailable: {}",
                redact_error_message(&error)
            );
            return Fetched::Unavailable;
        }
        Err(_) => {
            tracing::warn!(
                platform = %cache.platform,
                track_id = %cache.track_id,
                "lyrics for the Telegram upload timed out"
            );
            return Fetched::Unavailable;
        }
    };
    match lyric_tags::lyrics_text(&lyrics) {
        Some(text) => Fetched::Text(lyric_tags::to_simplified(&text)),
        None => Fetched::Nothing,
    }
}

/// `audio` with `text` written in. A file that cannot hold it is final:
/// the same bytes fail the same way next time.
async fn embed_text(cache: &MusicCache, audio: Vec<u8>, text: String) -> PreparedAudio {
    let audio = Arc::new(audio);
    let source = audio.clone();
    let tagged = tokio::task::spawn_blocking(move || lyric_tags::embed(&source, &text)).await;
    match tagged {
        Ok(Ok(tagged)) => PreparedAudio {
            audio: tagged,
            lyrics_tag: LYRICS_TAGGED,
            embedded: true,
        },
        result => {
            let error = match result {
                Ok(Err(error)) => error.to_string(),
                Err(error) => error.to_string(),
                Ok(Ok(_)) => unreachable!(),
            };
            tracing::warn!(
                platform = %cache.platform,
                track_id = %cache.track_id,
                "left lyrics out of the Telegram upload: {error}"
            );
            let audio = Arc::try_unwrap(audio).unwrap_or_else(|audio| (*audio).clone());
            PreparedAudio::unchanged(audio, LYRICS_TAGGED)
        }
    }
}

/// `audio` with the song's lyrics written in when it has any. A failed or
/// slow lyrics lookup leaves the file as it is and the row untagged (0), so
/// a later request tries again; a song without lyrics, or a file that
/// cannot hold them, is final.
pub(super) async fn prepare(cache: &MusicCache, audio: Vec<u8>) -> PreparedAudio {
    if lyric_tags::Container::sniff(&audio).is_none() {
        return PreparedAudio::unchanged(audio, LYRICS_TAGGED);
    }
    match fetch_text(cache).await {
        Fetched::Text(text) => embed_text(cache, audio, text).await,
        Fetched::Nothing => PreparedAudio::unchanged(audio, LYRICS_TAGGED),
        Fetched::Unavailable => PreparedAudio::unchanged(audio, 0),
    }
}

/// Whether an upload has been through lyrics tagging.
pub(super) fn is_tagged(cache: &MusicCache) -> bool {
    cache.lyrics_tag > 0
}

/// Tags a cached upload that has not been through lyrics tagging and returns
/// the updated row, joining a re-tag already running for the song. `None`
/// keeps `cache`: it is tagged already, its last re-tag failed recently, or
/// this one outlasted the wait (it carries on for the next request).
pub(super) async fn refresh_stale(cache: &MusicCache, wait: bool) -> Option<MusicCache> {
    if is_tagged(cache) || cache.file_id.trim().is_empty() {
        return None;
    }
    // Keyed by the upload: the qualities and track IDs sharing it are
    // re-tagged once, and all move to the new file together.
    let key = cache.file_id.clone();
    let mut finished = {
        let mut retags = RETAGS.lock().unwrap_or_else(|poison| poison.into_inner());
        match retags.get(&key) {
            Some(Retag::Running(finished)) => finished.clone(),
            Some(Retag::Failed(at)) if at.elapsed() < RETAG_RETRY_AFTER => return None,
            _ => {
                if retags.len() >= RETAG_MEMORY_MAX {
                    retags.retain(|_, retag| match retag {
                        Retag::Running(_) => true,
                        Retag::Failed(at) => at.elapsed() < RETAG_RETRY_AFTER,
                    });
                }
                let (done, finished) = watch::channel(None);
                retags.insert(key.clone(), Retag::Running(finished.clone()));
                tokio::spawn(run_retag(cache.clone(), key, done));
                finished
            }
        }
    };
    if !wait {
        return None;
    }
    let outcome = tokio::time::timeout(RETAG_WAIT, finished.wait_for(Option::is_some)).await;
    match outcome {
        Ok(Ok(outcome)) => outcome.clone().flatten(),
        _ => None,
    }
}

async fn run_retag(cache: MusicCache, key: String, done: watch::Sender<Option<Outcome>>) {
    let result = match RETAG_SLOTS.acquire().await {
        Ok(_slot) => retag(&cache).await,
        Err(_) => Err(ApiError::Custom("re-tag slots closed".to_string())),
    };
    let mut retags = RETAGS.lock().unwrap_or_else(|poison| poison.into_inner());
    let outcome = match result {
        Ok(updated) => {
            // The row no longer asks for it; forget the song.
            retags.remove(&key);
            updated
        }
        Err(error) => {
            tracing::warn!(
                platform = %cache.platform,
                track_id = %cache.track_id,
                "failed to add lyrics to a cached Telegram upload: {}",
                redact_error_message(&error)
            );
            retags.insert(key, Retag::Failed(Instant::now()));
            None
        }
    };
    let _ = done.send(Some(outcome));
}

/// The row as updated, or `None` when it changed meanwhile and kept the
/// newer file.
async fn retag(cache: &MusicCache) -> ApiResult<Outcome> {
    let mut updated = cache.clone();
    updated.lyrics_tag = LYRICS_TAGGED;
    let text = match fetch_text(cache).await {
        Fetched::Text(text) => text,
        // Nothing to write: the file stays, the row stops asking.
        Fetched::Nothing => {
            return Ok(replace_music_cache_file(&cache.file_id, &updated)
                .await?
                .then_some(updated));
        }
        Fetched::Unavailable => {
            return Err(ApiError::Custom("歌词暂时无法获取".to_string()));
        }
    };

    let audio =
        telegram_cache::read_file(&cache.file_id, Some(cache.music_size.max(0) as u64)).await?;
    let prepared = if lyric_tags::Container::sniff(&audio).is_none() {
        PreparedAudio::unchanged(audio, LYRICS_TAGGED)
    } else {
        embed_text(cache, audio, text).await
    };
    if prepared.embedded {
        // A thumbnail cannot be sent by file_id; upload the old one again.
        let thumbnail = match cache.thumb_file_id.trim() {
            "" => None,
            thumb => telegram_cache::read_file(thumb, None).await.ok(),
        };
        let chat_id = cache_upload_chat_id();
        let audio_len = prepared.audio.len();
        let uploaded = telegram_cache::upload_audio(
            chat_id,
            &cache_audio_filename(cache),
            &content_type_from_ext(&cache.file_ext),
            prepared.audio,
            thumbnail,
            &cache.song,
            &cache.singer,
            cache.duration.map(|duration| duration.max(0) as u32),
        )
        .await?;
        updated.file_id = uploaded.file_id;
        if let Some(thumb) = uploaded.thumb_file_id {
            updated.thumb_file_id = thumb;
        }
        updated.music_size = audio_len.min(i64::MAX as usize) as i64;
        updated.from_chat_id = chat_id;
    }
    if !replace_music_cache_file(&cache.file_id, &updated).await? {
        tracing::info!(
            platform = %cache.platform,
            track_id = %cache.track_id,
            "cached Telegram upload changed while adding lyrics; kept the newer one"
        );
        return Ok(None);
    }
    Ok(Some(updated))
}
