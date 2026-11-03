//! The contract between MHub and the services that borrow its music sources.
//!
//! MHub owns every platform account, implementation, download and the shared
//! Telegram cache. Other services, such as the Telegram bot, link this crate
//! and talk to MHub's loopback gateway instead of keeping their own copy of
//! the providers or their credentials. Both sides compile against the same
//! types, so a platform, quality or field added here reaches every consumer
//! at once, and [`PROTOCOL_VERSION`] rejects a deployment that drifted apart.

mod platform;
pub mod protocol;
mod quality;

#[cfg(feature = "client")]
mod client;

pub use platform::Platform;
pub use protocol::{
    CacheInvalidateRequest, CacheLookupRequest, CachedTrack, Capabilities, Collection,
    CollectionRequest, ErrorBody, ErrorCode, JobState, Lyrics, LyricsRequest, Media, MediaJob,
    MediaLookup, MediaPollQuery, MediaRequest, PROTOCOL_HEADER, PROTOCOL_VERSION,
    PlatformCapability, ResolveRequest, SearchItem, SearchRequest, Track,
};
pub use quality::Quality;

#[cfg(feature = "client")]
pub use client::{GatewayClient, GatewayError};
