//! Wire types of the gateway. Every endpoint takes and returns JSON; an error
//! is always an [`ErrorBody`] with a non-2xx status.

use crate::{Platform, Quality};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Bumped on any change an older peer would misread. Additive optional
/// fields keep the version; clients and servers refuse a different one.
pub const PROTOCOL_VERSION: u32 = 1;
/// Request header carrying the client's [`PROTOCOL_VERSION`].
pub const PROTOCOL_HEADER: &str = "x-music-gateway-protocol";

pub mod paths {
    pub const CAPABILITIES: &str = "/v1/capabilities";
    pub const SEARCH: &str = "/v1/search";
    pub const COLLECTION: &str = "/v1/collection";
    pub const LYRICS: &str = "/v1/lyrics";
    pub const RESOLVE: &str = "/v1/resolve";
    /// `POST` starts or joins a job; `GET {MEDIA}/{id}` polls it.
    pub const MEDIA: &str = "/v1/media";
    pub const MEDIA_LOOKUP: &str = "/v1/media/lookup";
    pub const CACHE_LOOKUP: &str = "/v1/cache/lookup";
    pub const CACHE_INVALIDATE: &str = "/v1/cache/invalidate";
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchItem {
    pub platform: Platform,
    pub id: String,
    pub song: String,
    pub singer: String,
    pub cover: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Collection {
    pub platform: Platform,
    pub id: String,
    pub title: String,
    pub items: Vec<SearchItem>,
}

/// Timed LRC text with contributor lines already moved into `credits`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lyrics {
    pub plain: String,
    pub translation: String,
    /// Romanization as timed LRC, matched to `plain` by time.
    pub roman: String,
    /// `role：names` entries such as `作词：林夕`.
    pub credits: Vec<String>,
}

/// Song metadata without anything needed to fetch the audio itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub platform: Platform,
    pub id: String,
    pub song: String,
    pub singer: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub cover: String,
    /// Public page of the song on its platform.
    #[serde(default)]
    pub link: String,
    /// Seconds.
    #[serde(default)]
    pub duration: Option<u32>,
    /// Bits per second.
    #[serde(default)]
    pub bitrate: Option<u32>,
    /// File extension of the delivered audio, e.g. `flac` or `m4a`.
    #[serde(default)]
    pub format: Option<String>,
}

impl Track {
    pub fn file_extension(&self) -> String {
        self.format
            .as_deref()
            .map(|format| format.trim().trim_start_matches('.'))
            .filter(|format| !format.is_empty())
            .unwrap_or("mp3")
            .to_ascii_lowercase()
    }

    pub fn file_name(&self) -> String {
        format!("{} - {}.{}", self.song, self.singer, self.file_extension())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchRequest {
    pub platform: Platform,
    pub keyword: String,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CollectionRequest {
    pub platform: Platform,
    /// Link or ID of a playlist or album.
    pub keyword: String,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LyricsRequest {
    pub platform: Platform,
    pub track_id: String,
}

/// A keyword, link or ID, optionally narrowed to one search result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolveRequest {
    pub platform: Platform,
    pub keyword: String,
    #[serde(default)]
    pub selected_id: Option<String>,
    pub quality: Quality,
}

/// Asks MHub to make a song available as a Telegram file. MHub reuses its own
/// cache and running downloads, so concurrent requests for the same song from
/// the web player and any client share one download and one upload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaRequest {
    pub platform: Platform,
    pub keyword: String,
    #[serde(default)]
    pub selected_id: Option<String>,
    pub quality: Quality,
    /// No longer has any effect: the cached upload is shared by every client
    /// and always carries the cover as its thumbnail. Kept so older clients
    /// still parse.
    #[serde(default = "default_true")]
    pub include_cover: bool,
    /// On a lookup, wait (briefly) for lyrics to be written into a cached
    /// file uploaded before MHub embedded them, so this request already gets
    /// the file with lyrics. Otherwise that happens in the background and
    /// only later requests get it.
    #[serde(default)]
    pub wait_for_lyrics: bool,
}

/// The answer to a [`MediaRequest`] sent to `MEDIA_LOOKUP`, which never starts
/// a download. Clients use it to tell a free cache hit from a download they
/// want to meter, then start the job with `selected_id` set to `track.id`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MediaLookup {
    /// The song a keyword resolved to; `None` when the request already named
    /// a `selected_id` and nothing is cached for it.
    #[serde(default)]
    pub track: Option<Track>,
    #[serde(default)]
    pub media: Option<Media>,
}

/// Long-poll parameters for `GET {MEDIA}/{id}`: the response waits up to
/// `wait_ms` while the job is still in the `since` phase.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MediaPollQuery {
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub wait_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MediaJob {
    /// Stable per platform/song/quality; poll it until `state` is final.
    pub id: String,
    /// The song, once MHub has resolved it.
    #[serde(default)]
    pub track: Option<Track>,
    pub state: JobState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum JobState {
    Queued,
    /// Bytes fetched from the platform so far, when MHub can tell.
    Downloading {
        #[serde(default)]
        written: Option<u64>,
        #[serde(default)]
        total: Option<u64>,
    },
    /// Downloaded; sending the file to the Telegram storage chat.
    Uploading,
    Ready {
        media: Box<Media>,
    },
    Failed {
        error: ErrorBody,
    },
}

impl JobState {
    /// The `since` value for the next poll.
    pub fn phase(&self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Downloading { .. } => "downloading",
            JobState::Uploading => "uploading",
            JobState::Ready { .. } => "ready",
            JobState::Failed { .. } => "failed",
        }
    }

    pub fn is_final(&self) -> bool {
        matches!(self, JobState::Ready { .. } | JobState::Failed { .. })
    }
}

/// A song stored in the shared Telegram cache, sendable by `file_id` with the
/// gateway's bot token.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub track: Track,
    pub quality: Quality,
    pub file_id: String,
    #[serde(default)]
    pub thumb_file_id: Option<String>,
    pub byte_size: u64,
    /// Time spent decrypting a protected source, when this request did it.
    #[serde(default)]
    pub decrypt_elapsed_ms: Option<u64>,
    /// Whether the file already existed before this request.
    pub cached: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheLookupRequest {
    pub file_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CachedTrack {
    pub track: Track,
    /// The tier the entry was requested at; providers may have delivered the
    /// nearest lower one, see `track.bitrate`.
    pub quality: String,
    pub byte_size: u64,
}

/// Drops a cache entry whose `file_id` Telegram rejected. Matching on the
/// `file_id` keeps a concurrent re-upload of the same song intact.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheInvalidateRequest {
    pub platform: Platform,
    pub track_id: String,
    pub quality: Quality,
    pub file_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub protocol: u32,
    /// Server build, for logs.
    pub server: String,
    pub qualities: Vec<Quality>,
    pub platforms: Vec<PlatformCapability>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlatformCapability {
    pub platform: Platform,
    pub label: String,
    pub lyrics: bool,
    pub collections: bool,
    /// `None` when the platform needs no account; otherwise whether MHub has
    /// one configured. A configured account may still have expired.
    #[serde(default)]
    pub account_configured: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    ProtocolMismatch,
    NotFound,
    RateLimited,
    /// MHub or a dependency (Telegram, the cache database) is unavailable.
    Unavailable,
    /// The music platform failed or refused the request.
    Upstream,
}

/// `message` is written for end users and never contains credentials or
/// signed URLs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default)]
    pub retry_after_ms: Option<u64>,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after_ms: None,
        }
    }
}

impl fmt::Display for ErrorBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ErrorBody {}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_states_are_tagged_by_phase() {
        let state = JobState::Downloading {
            written: Some(10),
            total: None,
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"phase": "downloading", "written": 10, "total": null})
        );
        assert_eq!(serde_json::from_value::<JobState>(json).unwrap(), state);
        assert_eq!(
            serde_json::from_str::<JobState>(r#"{"phase":"queued"}"#).unwrap(),
            JobState::Queued
        );
        assert!(!state.is_final());
        assert_eq!(state.phase(), "downloading");
    }

    #[test]
    fn media_request_defaults_to_embedding_the_cover() {
        let request: MediaRequest =
            serde_json::from_str(r#"{"platform":"netease","keyword":"晴天","quality":"lossless"}"#)
                .unwrap();
        assert!(request.include_cover);
        assert_eq!(request.quality, Quality::Lossless);
        assert_eq!(request.selected_id, None);
    }

    #[test]
    fn track_file_name_uses_the_delivered_format() {
        let mut track = Track {
            platform: Platform::Netease,
            id: "1".into(),
            song: "晴天".into(),
            singer: "周杰伦".into(),
            album: String::new(),
            cover: String::new(),
            link: String::new(),
            duration: None,
            bitrate: None,
            format: Some(".FLAC".into()),
        };
        assert_eq!(track.file_name(), "晴天 - 周杰伦.flac");
        track.format = None;
        assert_eq!(track.file_extension(), "mp3");
    }
}
