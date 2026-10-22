use super::provider::{
    DownloadProgress, MusicCollection, MusicLyrics, MusicPlatform, MusicProvider, MusicSearchItem,
    MusicTrack,
};
use super::streaming::{AppendOnlyFmp4Parser, Fmp4Chunk};
use super::{RecommendationHome, RecommendationPlaylist, RecommendationTag};
use crate::{BotError, settings::SETTINGS};
use aes::cipher::{KeyIvInit, StreamCipher};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose};
use lazy_static::lazy_static;
use oxideav_mp4::cenc::{SencBox, SubsampleEntry, TencBox, parse_senc, parse_tenc};
use protobuf::Message;
use regex::Regex;
use reqwest::Client;
use rsa::{RsaPrivateKey, pkcs1::DecodeRsaPrivateKey};
use serde::{Deserialize, de::DeserializeOwned};
use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    ops::Range,
    sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use temari::{rounds::decrypt_par, template::template_from_json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use url::Url;
use widevine::{
    Cdm, Device, KeyType, LicenseType, Pssh,
    device::{DeviceType, SecurityLevel},
};
use widevine_proto::license_protocol::{WidevinePsshData, widevine_pssh_data};

const APPLE_MUSIC_API: &str = "https://amp-api.music.apple.com";
const APPLE_MUSIC_ORIGIN: &str = "https://music.apple.com";
const APPLE_MUSIC_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/110.0.0.0 Safari/537.36";
const DEFAULT_ARTWORK_SIZE: usize = 1200;
const WEB_PLAYBACK_URL: &str = "https://play.itunes.apple.com/WebObjects/MZPlay.woa/wa/webPlayback";
const WEB_PLAYBACK_LICENSE_URL: &str =
    "https://play.itunes.apple.com/WebObjects/MZPlay.woa/wa/acquireWebPlaybackLicense";
const APPLE_WRAPPER_SCHEME: &str = "applemusic-wrapper://";
const APPLE_WRAPPER_LITE_SCHEME: &str = "applemusic-wrapper-lite://";
const APPLE_WIDEVINE_SCHEME: &str = "applemusic-widevine://";
const APPLE_WRAPPER_M3U8_PORT: u16 = 20020;
const APPLE_WRAPPER_DECRYPT_PORT: u16 = 10020;
const APPLE_PREFETCH_KEY_URI: &str = "skd://itunes.apple.com/P000000000/s1/e1";
const APPLE_WRAPPER_M3U8_TIMEOUT: Duration = Duration::from_secs(5);
const APPLE_WRAPPER_IO_TIMEOUT: Duration = Duration::from_secs(15);
const APPLE_WRAPPER_LITE_TIMEOUT: Duration = Duration::from_secs(10);
const APPLE_WRAPPER_LITE_MEDIA_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const APPLE_WRAPPER_LITE_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
type Aes128Ctr = ctr::Ctr128BE<aes::Aes128>;

lazy_static! {
    static ref CLIENT: Client = {
        let timeout = apple_timeout();
        Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| Client::new())
    };
    // Media transfers can legitimately outlive any whole-request deadline
    // (lossless tracks are tens of MB), so this client only bounds connect
    // and per-read stalls instead of the total request time.
    static ref MEDIA_CLIENT: Client = {
        let timeout = apple_timeout();
        Client::builder()
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .build()
            .unwrap_or_else(|_| Client::new())
    };
    static ref WRAPPER_LITE_MEDIA_CLIENT: Client = Client::builder()
        .connect_timeout(apple_timeout())
        .read_timeout(APPLE_WRAPPER_LITE_MEDIA_IDLE_TIMEOUT)
        .build()
        .unwrap_or_else(|_| Client::new());
    static ref DEVELOPER_TOKEN: RwLock<Option<String>> = RwLock::new(None);
    static ref DEVELOPER_TOKEN_FETCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
    static ref JS_ASSET_PATTERN: Regex = Regex::new(r#"/assets/index[^"'\s]*\.js"#).unwrap();
    static ref TOKEN_PATTERN: Regex =
        Regex::new(r"eyJ[A-Za-z0-9_-]{40,}\.[A-Za-z0-9_-]{40,}\.[A-Za-z0-9_-]{40,}").unwrap();
    static ref APPLE_URL_PATTERN: Regex = Regex::new(r"https?://[^\s]+").unwrap();
    static ref APPLE_SONG_PATH_RE: Regex = Regex::new(r"^/[a-z]{2}/song/[^/]+/(\d{6,})$").unwrap();
    // Recommendation cards deliberately omit Apple's descriptive URL slug,
    // which can contain account names. Accept both those canonical links and
    // the named share links used by manual imports.
    static ref APPLE_ALBUM_PATH_RE: Regex =
        Regex::new(r"^/[a-z]{2}/album/(?:[^/]+/)?([0-9]{1,20})/?$").unwrap();
    static ref APPLE_PLAYLIST_PATH_RE: Regex =
        Regex::new(r"^/[a-z]{2}/playlist/(?:[^/]+/)?(pl\.[a-zA-Z0-9.-]{1,117})/?$").unwrap();
    static ref ENHANCED_STREAM_INF_RE: Regex = Regex::new(r#"^#EXT-X-STREAM-INF:(.*)$"#).unwrap();
    static ref ENHANCED_AUDIO_RE: Regex = Regex::new(r#"AUDIO="([^"]*)""#).unwrap();
    static ref ENHANCED_CODEC_RE: Regex = Regex::new(r#"CODECS="([^"]*)""#).unwrap();
    static ref ENHANCED_BW_RE: Regex = Regex::new(r#"(^|,)BANDWIDTH=(\d+)"#).unwrap();
    static ref ENHANCED_AVG_BW_RE: Regex = Regex::new(r#"AVERAGE-BANDWIDTH=(\d+)"#).unwrap();
    static ref MEDIA_KEY_RE: Regex = Regex::new(r#"#EXT-X-KEY:[^\n]*URI="(skd://[^"]+)""#).unwrap();
    static ref MEDIA_MAP_RE: Regex = Regex::new(r#"URI="([^"]+)""#).unwrap();
    static ref APPLE_WRAPPER_ENDPOINTS: Vec<AppleWrapperEndpoint> = configured_wrapper_endpoints();
    static ref APPLE_WRAPPER_LITE_ENDPOINTS: Vec<AppleWrapperLiteEndpoint> =
        configured_wrapper_lite_endpoints();
    static ref APPLE_WRAPPER_ENDPOINT_DECRYPT_SEMAPHORES: Vec<tokio::sync::Semaphore> =
        APPLE_WRAPPER_ENDPOINTS
            .iter()
            .map(|_| tokio::sync::Semaphore::new(
                SETTINGS.music.applemusic.wrapper_track_concurrency.max(1)
            ))
            .collect();
    static ref APPLE_WRAPPER_NEXT_ENDPOINT: AtomicUsize = AtomicUsize::new(0);
    static ref APPLE_WRAPPER_LITE_NEXT_ENDPOINT: AtomicUsize = AtomicUsize::new(0);
    static ref APPLE_WRAPPER_LITE_TEMPLATES:
        RwLock<HashMap<(String, String), CachedWrapperLiteTemplate>> = RwLock::new(HashMap::new());
    static ref APPLE_WRAPPER_TRACK_DECRYPT_SEMAPHORE: tokio::sync::Semaphore =
        tokio::sync::Semaphore::new(
            SETTINGS
                .music
                .applemusic
                .wrapper_track_concurrency
                .max(1)
                .saturating_mul(APPLE_WRAPPER_ENDPOINTS.len().max(1))
        );
    static ref APPLE_WRAPPER_LITE_TRACK_SEMAPHORE: tokio::sync::Semaphore =
        tokio::sync::Semaphore::new(
            SETTINGS
                .music
                .applemusic
                .wrapper_track_concurrency
                .max(1)
                .saturating_mul(APPLE_WRAPPER_LITE_ENDPOINTS.len().max(1)),
        );
    static ref APPLE_WRAPPER_DECRYPT_SEMAPHORE: tokio::sync::Semaphore =
        tokio::sync::Semaphore::new(
            SETTINGS
                .music
                .applemusic
                .wrapper_connection_concurrency
                .max(1)
                .saturating_mul(APPLE_WRAPPER_ENDPOINTS.len().max(1))
        );
}

pub static APPLE_MUSIC_PROVIDER: AppleMusicProvider = AppleMusicProvider;

pub struct AppleMusicProvider;

#[derive(Clone)]
struct CachedWrapperLiteTemplate {
    fetched_at: Instant,
    template: Arc<temari::rounds::Template>,
}

#[path = "applemusic_recommendations.rs"]
mod recommendations;
pub use recommendations::{
    continuation_recommendations, personal_recommendation_home, personal_recommendations,
};

fn recommendation_playlist(
    recommendation_id: &str,
    content: &AppleMusicRecommendationContent,
) -> RecommendationPlaylist {
    let attributes = content.attributes.as_ref();
    RecommendationPlaylist {
        id: content.id.clone(),
        resource_type: content
            .resource_type
            .clone()
            .unwrap_or_else(|| "playlists".to_string()),
        name: shared_recommendation_name(content),
        curator: attributes
            .and_then(|attributes| {
                if content.resource_type.as_deref() == Some("albums") {
                    attributes.artist_name.clone()
                } else {
                    // Personal-mix curator names can contain an account
                    // holder's name; keep the shared feed account-neutral.
                    Some("Apple Music".to_string())
                }
            })
            .unwrap_or_default(),
        // Account recommendation text may contain a person's name or listening
        // history. Catalog tracks and neutral artwork are sufficient here.
        description: String::new(),
        cover: attributes
            .and_then(|attributes| attributes.artwork.as_ref())
            .map(|artwork| format_artwork_url(Some(artwork), 720))
            .unwrap_or_default(),
        url: format!(
            "https://music.apple.com/{}/{}/{}",
            apple_storefront(),
            if content.resource_type.as_deref() == Some("albums") {
                "album"
            } else {
                "playlist"
            },
            content.id,
        ),
        recommendation_id: recommendation_id.to_string(),
    }
}

fn shared_recommendation_name(content: &AppleMusicRecommendationContent) -> String {
    let name = content
        .attributes
        .as_ref()
        .and_then(|attributes| attributes.name.as_deref())
        .unwrap_or("")
        .trim();
    if content.id.starts_with("pl.pm-") {
        // Only Apple's standard personal-mix labels may be shared. A custom
        // title such as "a person's mix" must never leave the server.
        const MIX_NAMES: &[&str] = &[
            "音乐新发现",
            "音樂新發現",
            "新音乐精选",
            "新音樂精選",
            "新音乐合辑",
            "新音樂合輯",
            "能量充电",
            "能量充電",
            "放松歌单",
            "放鬆歌單",
            "放松精选",
            "放鬆精選",
            "最爱精选",
            "最愛精選",
            "喜爱精选",
            "喜愛精選",
            "朋友精选",
            "朋友精選",
            "重度循环",
            "重度循環",
            "New Music Mix",
            "Get Up! Mix",
            "Chill Mix",
            "Favourites Mix",
            "Favorites Mix",
            "Friends Mix",
            "Heavy Rotation Mix",
        ];
        if MIX_NAMES.contains(&name) {
            name.to_string()
        } else {
            "专属推荐歌单".to_string()
        }
    } else if name.is_empty() {
        if content.resource_type.as_deref() == Some("albums") {
            "推荐专辑"
        } else {
            "精选歌单"
        }
        .to_string()
    } else {
        name.to_string()
    }
}

#[derive(Clone, Debug)]
struct AppleWrapperEndpoint {
    index: Option<usize>,
    spec: String,
    decrypt_addr: String,
    m3u8_addr: String,
}

#[derive(Clone, Debug)]
struct AppleWrapperLiteEndpoint {
    index: usize,
    base_url: String,
}

#[derive(Debug, Deserialize)]
struct AppleWrapperLiteResponse<T> {
    code: i64,
    #[serde(default)]
    msg: String,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct AppleWrapperLiteM3u8Data {
    #[serde(rename = "m3u8")]
    m3u8: String,
}

#[derive(Debug, Deserialize, serde::Serialize)]
struct AppleWrapperLiteKeyData {
    #[serde(default, rename = "adamId")]
    adam_id: String,
    #[serde(default, rename = "keyUri")]
    key_uri: String,
    #[serde(default, rename = "contentKey")]
    content_key: String,
    #[serde(default)]
    ctx: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    rcx: String,
    #[serde(default)]
    rax: String,
    #[serde(default)]
    rdx: String,
    #[serde(default)]
    r9: String,
    #[serde(default)]
    rbp: String,
}

fn configured_wrapper_endpoints() -> Vec<AppleWrapperEndpoint> {
    let mut specs = SETTINGS
        .music
        .applemusic
        .wrapper_hosts
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if specs.is_empty() {
        let host = SETTINGS.music.applemusic.wrapper_host.trim();
        if !host.is_empty() {
            specs.push(host.to_string());
        }
    }
    specs
        .into_iter()
        .filter_map(|spec| parse_wrapper_endpoint(&spec))
        .enumerate()
        .map(|(index, mut endpoint)| {
            endpoint.index = Some(index);
            endpoint
        })
        .collect()
}

fn configured_wrapper_lite_endpoints() -> Vec<AppleWrapperLiteEndpoint> {
    let config = &SETTINGS.music.applemusic;
    if !config.wrapper_lite_enabled {
        return Vec::new();
    }
    let mut specs = config
        .wrapper_lite_urls
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(config.wrapper_lite_url.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .collect::<Vec<_>>();
    specs.dedup();
    specs
        .into_iter()
        .enumerate()
        .filter_map(|(index, spec)| {
            let base_url = if spec.starts_with("http://") || spec.starts_with("https://") {
                spec
            } else {
                format!("http://{spec}")
            };
            let parsed = Url::parse(&base_url).ok()?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return None;
            }
            Some(AppleWrapperLiteEndpoint { index, base_url })
        })
        .collect()
}

fn parse_wrapper_endpoint(spec: &str) -> Option<AppleWrapperEndpoint> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let (host, decrypt_port) = split_host_port(spec).unwrap_or((spec, APPLE_WRAPPER_DECRYPT_PORT));
    if host.trim().is_empty() {
        return None;
    }
    let m3u8_port = if decrypt_port == APPLE_WRAPPER_DECRYPT_PORT {
        APPLE_WRAPPER_M3U8_PORT
    } else {
        decrypt_port.saturating_add(APPLE_WRAPPER_M3U8_PORT - APPLE_WRAPPER_DECRYPT_PORT)
    };
    Some(AppleWrapperEndpoint {
        index: None,
        spec: if decrypt_port == APPLE_WRAPPER_DECRYPT_PORT {
            host.to_string()
        } else {
            format!("{host}:{decrypt_port}")
        },
        decrypt_addr: format!("{host}:{decrypt_port}"),
        m3u8_addr: format!("{host}:{m3u8_port}"),
    })
}

fn split_host_port(value: &str) -> Option<(&str, u16)> {
    let (host, port) = value.rsplit_once(':')?;
    // A host containing ':' must be a bracketed IPv6 literal ("[::1]:10024");
    // bare IPv6 ("::1") falls through to the default-port path in the caller.
    if host.contains(':') && !host.ends_with(']') {
        return None;
    }
    port.parse::<u16>().ok().map(|port| (host, port))
}

fn select_wrapper_endpoints(spec: &str) -> Result<Vec<AppleWrapperEndpoint>, BotError> {
    if spec == "pool" {
        let endpoints = &*APPLE_WRAPPER_ENDPOINTS;
        if endpoints.is_empty() {
            return Err(BotError::Custom(
                "Apple Music wrapper pool 未配置".to_string(),
            ));
        }
        let start = APPLE_WRAPPER_NEXT_ENDPOINT.fetch_add(1, Ordering::Relaxed) % endpoints.len();
        return Ok((0..endpoints.len())
            .map(|offset| endpoints[(start + offset) % endpoints.len()].clone())
            .collect());
    }
    if let Some(endpoint) = APPLE_WRAPPER_ENDPOINTS
        .iter()
        .find(|endpoint| endpoint.spec == spec)
    {
        return Ok(vec![endpoint.clone()]);
    }
    parse_wrapper_endpoint(spec)
        .map(|endpoint| vec![endpoint])
        .ok_or_else(|| BotError::Custom(format!("Apple Music wrapper endpoint 无效：{spec}")))
}

#[async_trait]
impl MusicProvider for AppleMusicProvider {
    async fn search(&self, keyword: &str, limit: usize) -> Result<Vec<MusicSearchItem>, BotError> {
        ensure_apple_enabled()?;
        let limit = limit.clamp(1, 25);
        let url = apple_search_url(keyword, limit);
        let response: AppleMusicResponse = apple_get_json(&url, true).await?;
        Ok(response
            .results
            .and_then(|results| results.songs)
            .map(|songs| songs.data)
            .unwrap_or_default()
            .into_iter()
            .map(song_to_item)
            .filter(|item| !item.id.is_empty())
            .collect())
    }

    async fn collection(
        &self,
        keyword: &str,
        limit: usize,
    ) -> Result<Option<MusicCollection>, BotError> {
        ensure_apple_enabled()?;
        let Some(collection) = parse_apple_collection_id(keyword) else {
            return Ok(None);
        };
        get_apple_collection(collection, limit).await.map(Some)
    }

    async fn lyrics(&self, track_id: &str) -> Result<Option<MusicLyrics>, BotError> {
        ensure_apple_enabled()?;
        let id = parse_apple_track_id(track_id).unwrap_or_else(|| track_id.trim().to_string());
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            return Ok(None);
        }
        let ttml = get_apple_lyrics_ttml(&id).await?;
        let plain = parse_ttml_to_lrc(&ttml);
        if plain.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(MusicLyrics {
            plain,
            translation: String::new(),
        }))
    }

    async fn resolve(
        &self,
        keyword: &str,
        selected_id: Option<&str>,
    ) -> Result<MusicTrack, BotError> {
        resolve_apple_track(keyword, selected_id, AppleMusicQuality::High).await
    }
}

pub(super) async fn resolve_with_quality(
    keyword: &str,
    selected_id: Option<&str>,
    quality: &str,
) -> Result<MusicTrack, BotError> {
    resolve_apple_track(keyword, selected_id, quality_from_str(quality)).await
}

async fn resolve_apple_track(
    keyword: &str,
    selected_id: Option<&str>,
    quality: AppleMusicQuality,
) -> Result<MusicTrack, BotError> {
    ensure_apple_enabled()?;
    let id = match selected_id {
        Some(id) => id.to_string(),
        None => parse_apple_track_id(keyword)
            .unwrap_or_default()
            .trim()
            .to_string(),
    };
    let id = if id.is_empty() {
        APPLE_MUSIC_PROVIDER
            .search(keyword, 1)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| BotError::Custom("没有找到可下载的音乐".to_string()))?
            .id
    } else {
        id
    };

    let song = get_apple_song(&id).await?;
    let attrs = song.attributes;
    let expected_duration = attrs
        .duration_in_millis
        .filter(|duration| *duration > 0)
        .map(|duration| ((duration + 500) / 1000) as u32);
    let download_url = get_apple_download_url_with_quality(&id, quality, expected_duration).await?;
    Ok(MusicTrack {
        id: song.id,
        platform: MusicPlatform::AppleMusic,
        song: attrs.name,
        singer: attrs.artist_name,
        album: attrs.album_name,
        cover: format_artwork_url(attrs.artwork.as_ref(), DEFAULT_ARTWORK_SIZE),
        link: attrs
            .url
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| apple_track_url(&id)),
        url: download_url,
        headers: apple_download_headers(),
        max_chunk_size: None,
        duration: expected_duration,
        bitrate: None,
        format: Some("m4a".to_string()),
    })
}

#[derive(Deserialize)]
struct AppleMusicResponse {
    results: Option<AppleMusicSearchResults>,
    #[serde(default)]
    data: Vec<AppleMusicResource>,
}

#[derive(Deserialize)]
struct AppleMusicSearchResults {
    songs: Option<AppleMusicResourceList>,
}

#[derive(Deserialize)]
struct AppleMusicResourceList {
    #[serde(default)]
    data: Vec<AppleMusicResource>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Deserialize)]
struct AppleMusicRecommendationsResponse {
    #[serde(default)]
    data: Vec<AppleMusicRecommendationResource>,
}

#[derive(Deserialize)]
struct AppleMusicRecommendationResource {
    #[serde(default)]
    id: String,
    #[serde(default)]
    attributes: Option<AppleMusicRecommendationAttributes>,
    relationships: Option<AppleMusicRecommendationRelationships>,
}

#[derive(Deserialize)]
struct AppleMusicRecommendationAttributes {
    title: Option<AppleMusicDisplayText>,
    reason: Option<AppleMusicDisplayText>,
}

#[derive(Deserialize)]
struct AppleMusicDisplayText {
    #[serde(rename = "stringForDisplay")]
    string_for_display: String,
}

#[derive(Deserialize)]
struct AppleMusicRecommendationRelationships {
    contents: Option<AppleMusicRecommendationResourceList>,
}

#[derive(Deserialize)]
struct AppleMusicRecommendationResourceList {
    #[serde(default)]
    data: Vec<AppleMusicRecommendationContent>,
}

#[derive(Deserialize)]
struct AppleMusicRecommendationContent {
    #[serde(default)]
    id: String,
    #[serde(rename = "type")]
    resource_type: Option<String>,
    attributes: Option<AppleMusicPlaylistAttributes>,
}

#[derive(Deserialize)]
struct AppleMusicPlaylistAttributes {
    name: Option<String>,
    #[serde(rename = "curatorName")]
    curator_name: Option<String>,
    #[serde(rename = "artistName")]
    artist_name: Option<String>,
    description: Option<AppleMusicDescription>,
    artwork: Option<AppleMusicArtwork>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct AppleMusicDescription {
    standard: Option<String>,
}

#[derive(Deserialize)]
struct AppleMusicResource {
    id: String,
    attributes: AppleMusicAttributes,
}

#[derive(Deserialize)]
struct AppleMusicCollectionResponse {
    #[serde(default)]
    data: Vec<AppleMusicCollectionResource>,
}

#[derive(Deserialize)]
struct AppleMusicLyricsResponse {
    #[serde(default)]
    data: Vec<AppleMusicLyricsResource>,
}

#[derive(Deserialize)]
struct AppleMusicLyricsResource {
    attributes: AppleMusicLyricsAttributes,
}

#[derive(Deserialize)]
struct AppleMusicLyricsAttributes {
    #[serde(default)]
    ttml: String,
}

#[derive(Deserialize)]
struct AppleMusicCollectionResource {
    id: String,
    attributes: AppleMusicCollectionAttributes,
    relationships: Option<AppleMusicRelationships>,
}

#[derive(Deserialize)]
struct AppleMusicCollectionAttributes {
    name: String,
    #[serde(default, rename = "artistName")]
    artist_name: String,
}

#[derive(Deserialize)]
struct AppleMusicRelationships {
    tracks: Option<AppleMusicResourceList>,
}

#[derive(Deserialize)]
struct AppleMusicAttributes {
    name: String,
    #[serde(default, rename = "artistName")]
    artist_name: String,
    #[serde(default, rename = "albumName")]
    album_name: String,
    #[serde(default, rename = "durationInMillis")]
    duration_in_millis: Option<u64>,
    artwork: Option<AppleMusicArtwork>,
    url: Option<String>,
    #[serde(default, rename = "extendedAssetUrls")]
    extended_asset_urls: Option<AppleMusicExtendedAssetUrls>,
}

#[derive(Deserialize)]
struct AppleMusicArtwork {
    url: String,
}

#[derive(Deserialize)]
struct AppleMusicExtendedAssetUrls {
    #[serde(default, rename = "enhancedHls")]
    enhanced_hls: String,
}

#[derive(Deserialize)]
struct WebPlaybackResponse {
    #[serde(default, rename = "songList")]
    song_list: Vec<WebPlaybackSong>,
}

#[derive(Deserialize)]
struct WebPlaybackSong {
    #[serde(default)]
    assets: Vec<WebPlaybackAsset>,
}

#[derive(Clone, Deserialize)]
struct WebPlaybackAsset {
    #[serde(rename = "URL")]
    url: String,
    flavor: Option<String>,
    #[serde(default)]
    metadata: WebPlaybackMetadata,
}

#[derive(Clone, Default, Deserialize)]
struct WebPlaybackMetadata {
    #[serde(default, rename = "bitRate")]
    bit_rate: i64,
    #[serde(default)]
    duration: i64,
}

async fn get_apple_song(id: &str) -> Result<AppleMusicResource, BotError> {
    let url = apple_song_url(id);
    let response: AppleMusicResponse = apple_get_json(&url, true).await?;
    response
        .data
        .into_iter()
        .next()
        .ok_or_else(|| BotError::Custom("没有找到 Apple Music 歌曲详情".to_string()))
}

async fn get_apple_collection(
    collection: AppleCollectionId,
    limit: usize,
) -> Result<MusicCollection, BotError> {
    let url = apple_collection_url(&collection);
    let response: AppleMusicCollectionResponse = apple_get_json(&url, true).await?;
    let resource = response
        .data
        .into_iter()
        .next()
        .ok_or_else(|| BotError::Custom("没有找到 Apple Music 歌单或专辑".to_string()))?;
    let title = match collection.kind {
        AppleCollectionKind::Album => {
            let artist = resource.attributes.artist_name.trim();
            if artist.is_empty() {
                resource.attributes.name
            } else {
                format!("{} - {}", resource.attributes.name, artist)
            }
        }
        AppleCollectionKind::Playlist => resource.attributes.name,
    };
    let page = resource
        .relationships
        .and_then(|relationships| relationships.tracks)
        .ok_or_else(|| BotError::Custom("Apple Music 未返回歌单歌曲列表".to_string()))?;
    let items = collect_apple_collection_tracks(page, limit).await?;
    Ok(MusicCollection {
        platform: MusicPlatform::AppleMusic,
        id: resource.id,
        title,
        items,
    })
}

async fn collect_apple_collection_tracks(
    mut page: AppleMusicResourceList,
    limit: usize,
) -> Result<Vec<MusicSearchItem>, BotError> {
    let max_items = limit.max(1);
    let mut items = Vec::new();
    let mut visited = std::collections::HashSet::new();
    loop {
        let remaining = max_items - items.len();
        for song in page.data.into_iter().take(remaining) {
            if song.id.trim().is_empty() {
                return Err(BotError::Custom(
                    "Apple Music 歌单包含无效歌曲 ID，已停止导入以避免漏歌".to_string(),
                ));
            }
            items.push(song_to_item(song));
        }
        if items.len() >= max_items {
            break;
        }
        let Some(next) = page.next.filter(|next| !next.trim().is_empty()) else {
            break;
        };
        let next_url = apple_collection_next_url(&next)?;
        if !visited.insert(next_url.clone()) {
            return Err(BotError::Custom(
                "Apple Music 歌单分页重复，已停止导入以避免漏歌".to_string(),
            ));
        }
        page = apple_get_json(&next_url, true).await?;
    }
    Ok(items)
}

fn apple_collection_next_url(next: &str) -> Result<String, BotError> {
    let base = Url::parse(APPLE_MUSIC_API)?;
    let mut url = base.join(next)?;
    // Pagination carries the user's authorization headers, so only follow
    // the Apple API's own /v1/ links.
    if url.origin() != base.origin() || !url.path().starts_with("/v1/") {
        return Err(BotError::Custom("Apple Music 歌单分页地址无效".to_string()));
    }
    if !url.query_pairs().any(|(key, _)| key == "l") {
        url.query_pairs_mut().append_pair("l", &apple_language());
    }
    Ok(url.to_string())
}

async fn get_apple_lyrics_ttml(id: &str) -> Result<String, BotError> {
    let media_user_token =
        normalize_media_user_token(SETTINGS.music.applemusic.media_user_token.trim());
    if media_user_token.is_empty() {
        return Err(BotError::Custom(
            "Apple Music 歌词需要配置 music.applemusic.media_user_token".to_string(),
        ));
    }
    let url = apple_lyrics_url(id);
    let response: AppleMusicLyricsResponse =
        apple_get_json_with_media_user_token(&url, &media_user_token, true).await?;
    response
        .data
        .into_iter()
        .next()
        .map(|resource| resource.attributes.ttml)
        .filter(|ttml| !ttml.trim().is_empty())
        .ok_or_else(|| BotError::Custom("Apple Music 没有返回歌词".to_string()))
}

async fn get_apple_download_url_with_quality(
    id: &str,
    quality: AppleMusicQuality,
    expected_duration: Option<u32>,
) -> Result<String, BotError> {
    validate_apple_download_environment()?;
    if !APPLE_WRAPPER_LITE_ENDPOINTS.is_empty() {
        return Ok(apple_wrapper_lite_internal_url(
            id,
            quality,
            expected_duration,
        ));
    }
    let wrapper_host = apple_wrapper_host_opt();
    if let Some(wrapper_host) = wrapper_host {
        return Ok(apple_wrapper_internal_url(
            &wrapper_host,
            id,
            quality,
            expected_duration,
        ));
    }
    let media_user_token =
        normalize_media_user_token(SETTINGS.music.applemusic.media_user_token.trim());
    if media_user_token.is_empty() {
        return Err(BotError::Custom(
            "Apple Music 下载需要配置 music.applemusic.media_user_token、wrapper_host 或 wrapper_lite_url"
                .to_string(),
        ));
    }
    Ok(apple_widevine_internal_url(id, quality, expected_duration))
}

async fn get_catalog_enhanced_hls_master_url(track_id: &str) -> Result<String, BotError> {
    let url = get_apple_song(track_id)
        .await?
        .attributes
        .extended_asset_urls
        .map(|assets| assets.enhanced_hls)
        .unwrap_or_default();
    if url.trim().is_empty() {
        return Err(BotError::Custom(
            "Apple Music catalog 没有返回 enhancedHls".to_string(),
        ));
    }
    Ok(url)
}

async fn get_apple_webplayback_assets(
    id: &str,
    media_user_token: String,
) -> Result<Vec<WebPlaybackAsset>, BotError> {
    let developer_token = ensure_developer_token().await?;
    let response = CLIENT
        .post(WEB_PLAYBACK_URL)
        .header("Authorization", format!("Bearer {developer_token}"))
        .header("Content-Type", "application/json")
        .header("Origin", APPLE_MUSIC_ORIGIN)
        .header("User-Agent", APPLE_MUSIC_UA)
        .header("media-user-token", media_user_token)
        .json(&serde_json::json!({ "salableAdamId": id }))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(BotError::Custom(format!(
            "Apple Music WebPlayback 请求失败：HTTP {}",
            response.status()
        )));
    }
    let response: WebPlaybackResponse = response.json().await?;
    Ok(response
        .song_list
        .into_iter()
        .flat_map(|song| song.assets)
        .collect())
}

fn apple_wrapper_internal_url(
    host: &str,
    id: &str,
    quality: AppleMusicQuality,
    expected_duration: Option<u32>,
) -> String {
    append_internal_duration_query(
        format!(
            "{APPLE_WRAPPER_SCHEME}{host}/{id}?quality={}",
            quality.as_str()
        ),
        expected_duration,
    )
}

fn apple_wrapper_lite_internal_url(
    id: &str,
    quality: AppleMusicQuality,
    expected_duration: Option<u32>,
) -> String {
    append_internal_duration_query(
        format!(
            "{APPLE_WRAPPER_LITE_SCHEME}{id}?quality={}",
            quality.as_str()
        ),
        expected_duration,
    )
}

fn apple_widevine_internal_url(
    id: &str,
    quality: AppleMusicQuality,
    expected_duration: Option<u32>,
) -> String {
    append_internal_duration_query(
        format!("{APPLE_WIDEVINE_SCHEME}{id}?quality={}", quality.as_str()),
        expected_duration,
    )
}

fn append_internal_duration_query(mut url: String, expected_duration: Option<u32>) -> String {
    if let Some(duration) = expected_duration.filter(|duration| *duration > 0) {
        url.push_str("&duration=");
        url.push_str(&duration.to_string());
    }
    url
}

pub(super) async fn download_internal_url_with_progress_stats<F>(
    url: &str,
    progress: &mut F,
) -> Result<(Vec<u8>, Option<Duration>), BotError>
where
    F: FnMut(DownloadProgress) + Send,
{
    if let Some(payload) = url.strip_prefix(APPLE_WRAPPER_LITE_SCHEME) {
        let (track_id, query) = payload.split_once('?').unwrap_or((payload, ""));
        if track_id.trim().is_empty() {
            return Err(BotError::Custom(
                "Apple Music wrapper-lite URL 缺少 track id".to_string(),
            ));
        }
        let quality = query
            .split('&')
            .filter_map(|part| part.split_once('='))
            .find_map(|(key, value)| (key == "quality").then_some(value))
            .map(quality_from_str)
            .unwrap_or(AppleMusicQuality::High);
        let expected_duration = internal_query_u32(query, "duration");
        let lite_result = download_via_wrapper_lite_with_progress_stats(
            track_id,
            quality,
            expected_duration,
            progress,
        )
        .await;
        match lite_result {
            Ok(result) => return Ok(result),
            Err(lite_error) => {
                // Keep the existing raw TCP wrapper as an automatic rollback
                // path. A bad lite template, an old lite container, or a
                // transient HTTP error must never make a previously playable
                // track fail outright.
                if let Some(host) = apple_wrapper_host_opt() {
                    eprintln!(
                        "Apple Music wrapper-lite failed for {track_id}, falling back to legacy wrapper: {lite_error}"
                    );
                    return download_via_wrapper_with_progress_stats(
                        &host,
                        track_id,
                        quality,
                        expected_duration,
                        progress,
                    )
                    .await;
                }
                let media_user_token =
                    normalize_media_user_token(SETTINGS.music.applemusic.media_user_token.trim());
                if !media_user_token.is_empty()
                    && matches!(
                        quality,
                        AppleMusicQuality::Standard | AppleMusicQuality::High
                    )
                {
                    eprintln!(
                        "Apple Music wrapper-lite failed for {track_id}, falling back to Widevine: {lite_error}"
                    );
                    return download_via_widevine_with_progress(
                        track_id,
                        expected_duration,
                        progress,
                    )
                    .await
                    .map(|data| (data, None));
                }
                return Err(lite_error);
            }
        }
    }
    if let Some(payload) = url.strip_prefix(APPLE_WIDEVINE_SCHEME) {
        let (track_id, query) = payload.split_once('?').unwrap_or((payload, ""));
        if track_id.trim().is_empty() {
            return Err(BotError::Custom(
                "Apple Music Widevine URL 缺少 track id".to_string(),
            ));
        }
        let expected_duration = internal_query_u32(query, "duration");
        // applemusic-widevine:// URLs are only minted when no wrapper endpoint
        // is configured (get_apple_download_url_with_quality), and the endpoint
        // list is immutable for the process lifetime, so there is no wrapper
        // fallback to attempt here.
        return download_via_widevine_with_progress(track_id, expected_duration, progress)
            .await
            .map(|data| (data, None));
    }

    let payload = url
        .strip_prefix(APPLE_WRAPPER_SCHEME)
        .ok_or_else(|| BotError::Custom("Unknown Apple Music internal URL".to_string()))?;
    let (host, rest) = payload
        .split_once('/')
        .ok_or_else(|| BotError::Custom("Apple Music wrapper URL 缺少 track id".to_string()))?;
    let (track_id, query) = rest.split_once('?').unwrap_or((rest, ""));
    if host.trim().is_empty() || track_id.trim().is_empty() {
        return Err(BotError::Custom("Apple Music wrapper URL 无效".to_string()));
    }
    let quality = query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find_map(|(key, value)| (key == "quality").then_some(value))
        .map(quality_from_str)
        .unwrap_or(AppleMusicQuality::High);
    let expected_duration = internal_query_u32(query, "duration");
    download_via_wrapper_with_progress_stats(host, track_id, quality, expected_duration, progress)
        .await
}

fn internal_query_u32(query: &str, key: &str) -> Option<u32> {
    query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find_map(|(name, value)| (name == key).then_some(value))
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
}

#[cfg(test)]
async fn download_via_wrapper(
    host: &str,
    track_id: &str,
    quality: AppleMusicQuality,
) -> Result<Vec<u8>, BotError> {
    let mut progress = |_| {};
    download_via_wrapper_with_progress(host, track_id, quality, &mut progress).await
}

async fn download_via_widevine_with_progress<F>(
    track_id: &str,
    expected_duration: Option<u32>,
    progress: &mut F,
) -> Result<Vec<u8>, BotError>
where
    F: FnMut(DownloadProgress) + Send,
{
    let media_user_token =
        normalize_media_user_token(SETTINGS.music.applemusic.media_user_token.trim());
    if media_user_token.is_empty() {
        return Err(BotError::Custom(
            "Apple Music Widevine 下载需要配置 media_user_token".to_string(),
        ));
    }
    let assets = get_apple_webplayback_assets(track_id, media_user_token.clone()).await?;
    let asset = select_widevine_asset(&assets)
        .ok_or_else(|| BotError::Custom("Apple Music 没有返回 Widevine AAC 资源".to_string()))?;
    validate_webplayback_asset_duration(asset, expected_duration)?;
    let hls = download_text(&asset.url).await?;
    let widevine_media = parse_widevine_hls(&asset.url, &hls)?;
    let encrypted = download_bytes_with_progress(&widevine_media.mp4_url, progress).await?;
    let key = acquire_widevine_content_key(
        track_id,
        &widevine_media.kid,
        &widevine_media.uri_prefix,
        &widevine_media.kid_b64,
        &media_user_token,
    )
    .await?;
    // AES-CTR over the whole buffer plus the remux is CPU-bound; keep it off
    // the async worker threads.
    tokio::task::spawn_blocking(move || decrypt_cenc_fmp4(encrypted, &key))
        .await
        .map_err(|e| BotError::Custom(format!("Apple Music 解密任务失败：{e}")))?
}

#[cfg(test)]
async fn download_via_wrapper_with_progress<F>(
    host: &str,
    track_id: &str,
    quality: AppleMusicQuality,
    progress: &mut F,
) -> Result<Vec<u8>, BotError>
where
    F: FnMut(DownloadProgress) + Send,
{
    download_via_wrapper_with_progress_stats(host, track_id, quality, None, progress)
        .await
        .map(|(audio, _)| audio)
}

async fn download_via_wrapper_with_progress_stats<F>(
    host: &str,
    track_id: &str,
    quality: AppleMusicQuality,
    expected_duration: Option<u32>,
    progress: &mut F,
) -> Result<(Vec<u8>, Option<Duration>), BotError>
where
    F: FnMut(DownloadProgress) + Send,
{
    let endpoints = select_wrapper_endpoints(host)?;
    let endpoint_count = endpoints.len();
    // The catalog enhancedHls URL depends only on the track id, so fetch it at
    // most once across endpoint attempts (lazily: HiRes may never need it).
    let mut catalog_master_url: Option<Result<String, String>> = None;
    let mut last_error = None;
    for (index, endpoint) in endpoints.into_iter().enumerate() {
        match download_via_wrapper_endpoint(
            track_id,
            quality,
            expected_duration,
            progress,
            endpoint,
            &mut catalog_master_url,
        )
        .await
        {
            Ok(result) => return Ok(result),
            Err(err) if index + 1 < endpoint_count => last_error = Some(err),
            Err(err) => return Err(err),
        }
    }
    Err(last_error
        .unwrap_or_else(|| BotError::Custom("Apple Music wrapper endpoint 未配置".to_string())))
}

/// Download and decrypt through the HTTP wrapper-lite API. The legacy raw
/// wrapper remains the fallback in `download_internal_url_with_progress_stats`;
/// this path only opts in when wrapper-lite is explicitly configured. It keeps
/// the existing full-buffer/remux contract for now, while removing the
/// per-sample TCP round trips from the decrypt stage.
async fn download_via_wrapper_lite_with_progress_stats<F>(
    track_id: &str,
    quality: AppleMusicQuality,
    expected_duration: Option<u32>,
    progress: &mut F,
) -> Result<(Vec<u8>, Option<Duration>), BotError>
where
    F: FnMut(DownloadProgress) + Send,
{
    if APPLE_WRAPPER_LITE_ENDPOINTS.is_empty() {
        return Err(BotError::Custom(
            "Apple Music wrapper-lite endpoint 未配置".to_string(),
        ));
    }
    let _track_permit = APPLE_WRAPPER_LITE_TRACK_SEMAPHORE
        .acquire()
        .await
        .map_err(|_| BotError::Custom("Apple Music wrapper-lite track semaphore closed".into()))?;
    let start = APPLE_WRAPPER_LITE_NEXT_ENDPOINT.fetch_add(1, Ordering::Relaxed)
        % APPLE_WRAPPER_LITE_ENDPOINTS.len();
    let mut last_error = None;
    for offset in 0..APPLE_WRAPPER_LITE_ENDPOINTS.len() {
        let endpoint =
            &APPLE_WRAPPER_LITE_ENDPOINTS[(start + offset) % APPLE_WRAPPER_LITE_ENDPOINTS.len()];
        match download_via_wrapper_lite_endpoint(
            endpoint,
            track_id,
            quality,
            expected_duration,
            progress,
        )
        .await
        {
            Ok(result) => return Ok(result),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        BotError::Custom("Apple Music wrapper-lite 所有 endpoint 均失败".to_string())
    }))
}

async fn download_via_wrapper_lite_endpoint<F>(
    endpoint: &AppleWrapperLiteEndpoint,
    track_id: &str,
    quality: AppleMusicQuality,
    expected_duration: Option<u32>,
    progress: &mut F,
) -> Result<(Vec<u8>, Option<Duration>), BotError>
where
    F: FnMut(DownloadProgress) + Send,
{
    let master_url = wrapper_lite_m3u8_url(endpoint, track_id).await?;
    if !is_m3u8_url(&master_url) {
        return Err(BotError::Custom(format!(
            "Apple Music wrapper-lite 返回了无效 m3u8：{master_url}"
        )));
    }
    let master = download_text(&master_url).await?;
    let variants = parse_enhanced_hls_master(&master)?;
    let variant = select_variant_for_quality(&variants, quality)
        .ok_or_else(|| BotError::Custom("Apple Music enhancedHls 没有匹配的音质".to_string()))?;
    let media_url = absolute_hls_url(&master_url, &variant.uri);
    let media = parse_enhanced_hls_media(&media_url, &download_text(&media_url).await?)?;
    validate_hls_media_duration(media.duration_seconds, expected_duration)?;
    let mut encrypted = download_enhanced_hls_media_direct_or_range(&media, progress).await?;
    let tenc = parse_tenc_from_init(&encrypted)?;
    validate_wrapper_lite_cenc_layout(&encrypted, &tenc)?;

    let commands = collect_wrapper_decrypt_commands(&encrypted, track_id, &media.seg_keys, &tenc)?;
    let groups = group_wrapper_commands_by_fragment(commands)?;
    if groups.is_empty() {
        return Err(BotError::Custom(
            "Apple Music wrapper-lite 没有可解密的 MP4 fragment".to_string(),
        ));
    }

    let started = Instant::now();
    let batch_size = SETTINGS
        .music
        .applemusic
        .wrapper_lite_batch_size
        .clamp(1, 4096);
    for group in groups {
        let template = wrapper_lite_template(endpoint, &group.adam, &group.uri).await?;
        for jobs in group.jobs.chunks(batch_size) {
            if jobs.is_empty() {
                continue;
            }
            let payloads = jobs
                .iter()
                .map(|job| job.payload.clone())
                .collect::<Vec<_>>();
            let template = Arc::clone(&template);
            let plaintexts = tokio::task::spawn_blocking(move || {
                let samples = payloads.iter().map(Vec::as_slice).collect::<Vec<_>>();
                decrypt_par(template.as_ref(), &samples)
            })
            .await
            .map_err(|error| {
                BotError::Custom(format!("Apple Music wrapper-lite 解密任务失败：{error}"))
            })?;
            if plaintexts.len() != jobs.len() {
                return Err(BotError::Custom(
                    "Apple Music wrapper-lite 解密结果数量异常".to_string(),
                ));
            }
            for (job, plaintext) in jobs.iter().zip(plaintexts) {
                if plaintext.len() != job.payload.len()
                    || job
                        .ranges
                        .iter()
                        .map(|range| range.end - range.start)
                        .sum::<usize>()
                        != plaintext.len()
                {
                    return Err(BotError::Custom(
                        "Apple Music wrapper-lite 解密结果长度异常".to_string(),
                    ));
                }
                let mut source_offset = 0usize;
                for range in &job.ranges {
                    if range.end > encrypted.len()
                        || source_offset + (range.end - range.start) > plaintext.len()
                    {
                        return Err(BotError::Custom(
                            "Apple Music wrapper-lite 解密范围超出文件".to_string(),
                        ));
                    }
                    let length = range.end - range.start;
                    encrypted[range.clone()]
                        .copy_from_slice(&plaintext[source_offset..source_offset + length]);
                    source_offset += length;
                }
            }
        }
    }
    let decrypt_elapsed = started.elapsed();
    let audio =
        tokio::task::spawn_blocking(move || remux_decrypted_fmp4_to_progressive(&encrypted))
            .await
            .map_err(|error| {
                BotError::Custom(format!("Apple Music wrapper-lite remux 任务失败：{error}"))
            })??;
    Ok((audio, Some(decrypt_elapsed)))
}

const ALAC_STREAM_MAGIC: &[u8; 8] = b"SPLALAC1";
const ALAC_STREAM_VERSION: u32 = 2;

fn alac_stream_path(streaming_path: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.alacstream", streaming_path.display()))
}

/// Returns whether this track can be started through the incremental
/// wrapper-lite path. Native `<audio>` playback remains limited to AAC;
/// capable clients may explicitly request `wasm_alac`, which receives the
/// self-describing SPLALAC1 packet stream without changing the source quality.
pub(super) fn can_stream_track(track: &MusicTrack) -> bool {
    can_stream_track_with_mode(&track.url, None)
}

/// Return whether a wrapper-lite URL can be incrementally exposed.  ALAC is
/// only enabled for the explicit `wasm_alac` transport; native browser audio
/// must continue to use AAC or the completed-file fallback.
pub(super) fn can_stream_track_with_mode(url: &str, streaming_mode: Option<&str>) -> bool {
    if !url.starts_with(APPLE_WRAPPER_LITE_SCHEME) {
        return false;
    }
    let query = url
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default();
    let quality = query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find_map(|(key, value)| (key == "quality").then_some(quality_from_str(value)))
        .unwrap_or(AppleMusicQuality::High);
    match streaming_mode
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
    {
        Some("wasm_alac") => matches!(
            quality,
            AppleMusicQuality::Lossless | AppleMusicQuality::HiRes
        ),
        _ => matches!(
            quality,
            AppleMusicQuality::Standard | AppleMusicQuality::High
        ),
    }
}

/// Download wrapper-lite HLS incrementally and expose a browser-playable
/// fragmented MP4 while the remaining segments are still being decrypted.
/// The returned bytes are the normal progressive file used for the final
/// local/Telegram cache, so the existing completed-download contract remains
/// unchanged.
pub(super) async fn stream_wrapper_lite_to_path<F>(
    url: &str,
    streaming_path: &std::path::Path,
    on_ready: &mut F,
) -> Result<(Vec<u8>, Option<Duration>), BotError>
where
    F: FnMut() + Send,
{
    stream_wrapper_lite_to_path_with_mode(url, streaming_path, None, on_ready).await
}

pub(super) async fn stream_wrapper_lite_to_path_with_mode<F>(
    url: &str,
    streaming_path: &std::path::Path,
    streaming_mode: Option<&str>,
    on_ready: &mut F,
) -> Result<(Vec<u8>, Option<Duration>), BotError>
where
    F: FnMut() + Send,
{
    let queued_at = Instant::now();
    let _track_permit = APPLE_WRAPPER_LITE_TRACK_SEMAPHORE
        .acquire()
        .await
        .map_err(|_| BotError::Custom("Apple Music wrapper-lite track semaphore closed".into()))?;
    let queue_wait = queued_at.elapsed();
    let Some(payload) = url.strip_prefix(APPLE_WRAPPER_LITE_SCHEME) else {
        return Err(BotError::Custom(
            "Apple Music 增量播放需要 wrapper-lite URL".to_string(),
        ));
    };
    let (track_id, query) = payload.split_once('?').unwrap_or((payload, ""));
    if track_id.trim().is_empty() {
        return Err(BotError::Custom(
            "Apple Music wrapper-lite URL 缺少 track id".to_string(),
        ));
    }
    let quality = query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find_map(|(key, value)| (key == "quality").then_some(quality_from_str(value)))
        .unwrap_or(AppleMusicQuality::High);
    let wasm_alac = streaming_mode.map(str::trim) == Some("wasm_alac");
    if wasm_alac {
        if !matches!(
            quality,
            AppleMusicQuality::Lossless | AppleMusicQuality::HiRes
        ) {
            return Err(BotError::Custom(
                "WASM ALAC 增量播放需要 Lossless/Hi-Res 音质".to_string(),
            ));
        }
    } else if !matches!(
        quality,
        AppleMusicQuality::Standard | AppleMusicQuality::High
    ) {
        return Err(BotError::Custom(
            "Apple Music 原生增量播放暂仅支持 AAC Standard/High".to_string(),
        ));
    }
    let expected_duration = internal_query_u32(query, "duration");
    tracing::info!(
        track_id,
        queue_ms = queue_wait.as_millis() as u64,
        "Apple Music stream slot acquired"
    );
    let start = APPLE_WRAPPER_LITE_NEXT_ENDPOINT.fetch_add(1, Ordering::Relaxed)
        % APPLE_WRAPPER_LITE_ENDPOINTS.len().max(1);
    let mut last_error = None;
    let mut ready_seen = false;
    for offset in 0..APPLE_WRAPPER_LITE_ENDPOINTS.len() {
        let endpoint =
            &APPLE_WRAPPER_LITE_ENDPOINTS[(start + offset) % APPLE_WRAPPER_LITE_ENDPOINTS.len()];
        let mut signal_ready = || {
            ready_seen = true;
            tracing::info!(
                track_id,
                ready_ms = queued_at.elapsed().as_millis() as u64,
                "Apple Music stream ready including queue"
            );
            on_ready();
        };
        match stream_wrapper_lite_endpoint(
            endpoint,
            track_id,
            quality,
            streaming_mode,
            expected_duration,
            streaming_path,
            &mut signal_ready,
        )
        .await
        {
            Ok(result) => return Ok(result),
            Err(error) => {
                if ready_seen {
                    return Err(error);
                }
                let _ = tokio::fs::remove_file(streaming_path).await;
                let _ = tokio::fs::remove_file(streaming_assembled_path(streaming_path)).await;
                let _ = tokio::fs::remove_file(alac_stream_path(streaming_path)).await;
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        BotError::Custom("Apple Music wrapper-lite endpoint 未配置".to_string())
    }))
}

async fn stream_wrapper_lite_endpoint<F>(
    endpoint: &AppleWrapperLiteEndpoint,
    track_id: &str,
    quality: AppleMusicQuality,
    streaming_mode: Option<&str>,
    expected_duration: Option<u32>,
    streaming_path: &std::path::Path,
    on_ready: &mut F,
) -> Result<(Vec<u8>, Option<Duration>), BotError>
where
    F: FnMut() + Send,
{
    let preparation_started = Instant::now();
    if let Some(parent) = streaming_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let assembled_path = streaming_assembled_path(streaming_path);
    let wasm_alac = streaming_mode.map(str::trim) == Some("wasm_alac");
    let master_url = wrapper_lite_m3u8_url(endpoint, track_id).await?;
    let wrapper_ms = preparation_started.elapsed().as_millis() as u64;
    let stage_started = Instant::now();
    let master = download_text(&master_url).await?;
    let master_ms = stage_started.elapsed().as_millis() as u64;
    let variants = parse_enhanced_hls_master(&master)?;
    let variant = select_variant_for_quality(&variants, quality).ok_or_else(|| {
        if wasm_alac {
            BotError::Custom("Apple Music enhancedHls 没有匹配的 ALAC 音质".to_string())
        } else {
            BotError::Custom("Apple Music enhancedHls 没有匹配的 AAC 音质".to_string())
        }
    })?;
    if (wasm_alac && !variant.is_alac()) || (!wasm_alac && !variant.is_aac()) {
        return Err(BotError::Custom(if wasm_alac {
            "Apple Music 当前音质没有 ALAC fragmented MP4 变体".to_string()
        } else {
            "Apple Music 当前音质没有 AAC fragmented MP4 变体".to_string()
        }));
    }
    let media_url = absolute_hls_url(&master_url, &variant.uri);
    let stage_started = Instant::now();
    let media = parse_enhanced_hls_media(&media_url, &download_text(&media_url).await?)?;
    let media_ms = stage_started.elapsed().as_millis() as u64;
    validate_hls_media_duration(media.duration_seconds, expected_duration)?;
    if media.parts.len() < 2 {
        return Err(BotError::Custom(
            "Apple Music media playlist 缺少可增量播放的 init/segment".to_string(),
        ));
    }

    let stage_started = Instant::now();
    let init =
        download_enhanced_hls_part_with_retry(&media.parts[0], &WRAPPER_LITE_MEDIA_CLIENT).await?;
    let init_ms = stage_started.elapsed().as_millis() as u64;
    let tenc = parse_tenc_from_init(&init)?;
    validate_wrapper_lite_cenc_layout(&init, &tenc)?;
    let sanitized_init = build_streaming_init(&init)?;
    let alac_params = if wasm_alac {
        Some(
            find_alac_params(&sanitized_init)?
                .ok_or_else(|| BotError::Custom("ALAC init 缺少可用的解码参数".to_string()))?,
        )
    } else {
        None
    };
    let alac_path = alac_stream_path(streaming_path);
    let mut stream_file = tokio::fs::File::create(streaming_path).await?;
    let mut assembled_file = tokio::fs::File::create(&assembled_path).await?;
    let mut alac_file = if wasm_alac {
        let mut file = tokio::fs::File::create(&alac_path).await?;
        let header = build_alac_stream_header(&sanitized_init)?;
        file.write_all(&header).await?;
        file.flush().await?;
        Some(file)
    } else {
        None
    };
    stream_file.write_all(&sanitized_init).await?;
    stream_file.flush().await?;

    // Keep the original encrypted init and append plaintext fragments.  The
    // normal progressive remuxer can consume this at EOF without another
    // network pass, while the separate streaming file is safe for browsers.
    assembled_file.write_all(&init).await?;
    assembled_file.flush().await?;
    let started = Instant::now();
    let mut ready = false;
    let mut parser = AppendOnlyFmp4Parser::new();
    parser
        .push(&init)
        .map_err(|error| BotError::Custom(format!("Apple Music init 解析失败：{error}")))?;
    let mut fragment_index = 0usize;
    for part in media.parts.iter().skip(1) {
        let stage_started = Instant::now();
        let bytes = download_enhanced_hls_part_with_retry(part, &WRAPPER_LITE_MEDIA_CLIENT).await?;
        let download_ms = stage_started.elapsed().as_millis() as u64;
        let chunks = parser
            .push(&bytes)
            .map_err(|error| BotError::Custom(format!("Apple Music fragment 解析失败：{error}")))?;
        for chunk in chunks {
            let Fmp4Chunk::Fragment(mut fragment) = chunk else {
                continue;
            };
            let key_uri = media
                .seg_keys
                .get(fragment_index)
                .filter(|key| !key.trim().is_empty())
                .ok_or_else(|| BotError::Custom("Apple Music segment key 数量不足".to_string()))?;
            fragment_index += 1;
            validate_wrapper_lite_cenc_layout(&fragment, &tenc)?;
            let stage_started = Instant::now();
            decrypt_wrapper_lite_fragment(endpoint, track_id, key_uri, &mut fragment, &tenc)
                .await?;
            let decrypt_ms = stage_started.elapsed().as_millis() as u64;
            let stage_started = Instant::now();
            if let Some(file) = alac_file.as_mut() {
                append_alac_stream_fragment(
                    file,
                    &fragment,
                    &tenc,
                    alac_params.as_ref().expect("ALAC params validated above"),
                )
                .await?;
            }
            let browser_fragment = sanitize_streaming_fragment(&fragment)?;
            stream_file.write_all(&browser_fragment).await?;
            stream_file.flush().await?;
            assembled_file.write_all(&fragment).await?;
            assembled_file.flush().await?;
            if !ready {
                ready = true;
                // Public catalog ID and durations only: no account details,
                // signed media URLs or decryption material in diagnostics.
                tracing::info!(
                    track_id,
                    wrapper_ms,
                    master_ms,
                    media_ms,
                    init_ms,
                    download_ms,
                    decrypt_ms,
                    packet_write_ms = stage_started.elapsed().as_millis() as u64,
                    first_part_bytes = bytes.len(),
                    ready_ms = preparation_started.elapsed().as_millis() as u64,
                    sample_rate = variant.sample_rate,
                    "Apple Music first fragment timing"
                );
                on_ready();
            }
        }
    }
    parser
        .finish()
        .map_err(|error| BotError::Custom(format!("Apple Music fragment 截断：{error}")))?;
    if let Some(mut file) = alac_file {
        file.flush().await?;
        drop(file);
    }
    stream_file.flush().await?;
    assembled_file.flush().await?;
    drop(stream_file);
    drop(assembled_file);
    if !ready {
        return Err(BotError::Custom(
            "Apple Music wrapper-lite 没有可播放的 media fragment".to_string(),
        ));
    }
    let decrypt_elapsed = started.elapsed();
    let assembled = tokio::fs::read(&assembled_path).await?;
    let _ = tokio::fs::remove_file(&assembled_path).await;
    let audio =
        tokio::task::spawn_blocking(move || remux_decrypted_fmp4_to_progressive(&assembled))
            .await
            .map_err(|error| {
                BotError::Custom(format!("Apple Music wrapper-lite remux 任务失败：{error}"))
            })??;
    Ok((audio, Some(decrypt_elapsed)))
}

fn build_alac_stream_header(data: &[u8]) -> Result<Vec<u8>, BotError> {
    let params = find_alac_params(data)?.ok_or_else(|| {
        BotError::Custom("ALAC init 缺少 alac 配置，无法建立 WASM 流".to_string())
    })?;
    let sample_rate = find_box_recursive(data, 0, data.len(), b"mdhd")
        .and_then(|mdhd| parse_mdhd_timescale(&data[mdhd.payload_start()..mdhd.end()]))
        .unwrap_or(0);
    if sample_rate <= 0 {
        return Err(BotError::Custom("ALAC init 缺少有效采样率".to_string()));
    }
    let cookie = find_alac_cookie(data)?;
    let mut out = Vec::with_capacity(52);
    out.extend_from_slice(ALAC_STREAM_MAGIC);
    out.extend_from_slice(&ALAC_STREAM_VERSION.to_be_bytes());
    out.extend_from_slice(&(sample_rate as u32).to_be_bytes());
    out.push(params.channels.max(1));
    out.push(params.sample_size);
    // Header v2 appends the complete 24-byte ALAC magic cookie. The decoder
    // cannot initialize from rate/bit-depth alone because the Rice/LPC
    // parameters live in this configuration atom.
    out.extend_from_slice(&(cookie.len() as u16).to_be_bytes());
    // Records use mdhd timescale units. ALAC tracks conventionally use the
    // sample rate as their timescale; retaining it in the header keeps a
    // decoder independent of MP4 demuxing.
    out.extend_from_slice(&(params.max_samples_per_frame).to_be_bytes());
    out.extend_from_slice(&cookie);
    Ok(out)
}

fn find_alac_cookie(data: &[u8]) -> Result<Vec<u8>, BotError> {
    let stsd = find_box_recursive(data, 0, data.len(), b"stsd")
        .ok_or_else(|| BotError::Custom("ALAC init 缺少 stsd".to_string()))?;
    let entry = read_mp4_box(data, stsd.payload_start() + 8)?
        .ok_or_else(|| BotError::Custom("ALAC init 缺少 sample entry".to_string()))?;
    if &entry.typ != b"alac" {
        return Err(BotError::Custom(
            "ALAC init sample entry 类型异常".to_string(),
        ));
    }
    let config = find_child_box(data, entry.payload_start() + 28, entry.end(), b"alac")
        .ok_or_else(|| BotError::Custom("ALAC init 缺少配置 atom".to_string()))?;
    let body = &data[config.payload_start()..config.end()];
    // The atom payload starts with version/flags (4 bytes), followed by the
    // 24-byte ALAC magic cookie consumed by `StreamInfo::from_cookie`.
    if body.len() < 28 {
        return Err(BotError::Custom("ALAC 配置 atom 长度不足".to_string()));
    }
    Ok(body[4..28].to_vec())
}

async fn append_alac_stream_fragment(
    file: &mut tokio::fs::File,
    fragment: &[u8],
    tenc: &TencBox,
    params: &AlacParams,
) -> Result<(), BotError> {
    let samples = collect_fragment_samples(fragment, tenc)?;
    if samples.is_empty() {
        return Err(BotError::Custom(
            "ALAC fragment 没有可输出的 sample".to_string(),
        ));
    }
    for sample in samples {
        let packet = fragment
            .get(sample.range.clone())
            .ok_or_else(|| BotError::Custom("ALAC sample 范围超出 fragment".to_string()))?;
        let mut packet = packet.to_vec();
        patch_alac_packet_for_stream(&mut packet, params);
        let packet_len = u32::try_from(packet.len())
            .map_err(|_| BotError::Custom("ALAC sample 过大".to_string()))?;
        file.write_all(&packet_len.to_be_bytes()).await?;
        file.write_all(&sample.duration.to_be_bytes()).await?;
        file.write_all(&packet).await?;
    }
    file.flush().await?;
    Ok(())
}

/// Some Apple ALAC fragments omit the explicit end element and leave garbage
/// in the padding bits. The progressive remuxer already repairs this case;
/// apply the same repair before exposing a packet to the standalone WASM
/// decoder so a packet cannot poison the incremental playback pipeline.
fn patch_alac_packet_for_stream(packet: &mut [u8], params: &AlacParams) {
    let body_end = alac_find_body_end_bit(packet, params);
    if body_end < 0 || body_end as usize >= packet.len() * 8 {
        return;
    }
    let body_end = body_end as usize;
    let mut reader = AlacBitReader::new(packet);
    if !reader.skip(body_end) || reader.left() < 3 || reader.show(3) == Some(7) {
        return;
    }
    let _ = alac_patch_in_place(packet, 0, packet.len(), body_end);
}

/// Re-create the browser packet transport from our completed progressive ALAC
/// file. A sidecar may have been evicted independently; returning the MP4 bytes
/// at the packet endpoint would otherwise break an already-resolved player.
pub(super) fn alac_transport_from_progressive(data: &[u8]) -> Result<Vec<u8>, BotError> {
    let params =
        find_alac_params(data)?.ok_or_else(|| BotError::Custom("完整音频不是 ALAC".to_string()))?;
    let locations = find_progressive_packet_locations(data)?;
    let stts = find_box_recursive(data, 0, data.len(), b"stts")
        .ok_or_else(|| BotError::Custom("ALAC 完整音频缺少时间索引".to_string()))?;
    let timing = &data[stts.payload_start()..stts.end()];
    if timing.len() < 8 {
        return Err(BotError::Custom("ALAC 时间索引被截断".to_string()));
    }
    let entries = u32::from_be_bytes(timing[4..8].try_into().unwrap()) as usize;
    if entries > (timing.len() - 8) / 8 {
        return Err(BotError::Custom("ALAC 时间索引被截断".to_string()));
    }
    let mut durations = Vec::with_capacity(locations.len());
    for entry in timing[8..8 + entries * 8].chunks_exact(8) {
        let count = u32::from_be_bytes(entry[..4].try_into().unwrap()) as usize;
        let duration = u32::from_be_bytes(entry[4..].try_into().unwrap());
        if duration == 0 || count > locations.len().saturating_sub(durations.len()) {
            return Err(BotError::Custom(
                "ALAC 时间索引与 packet 数量不一致".to_string(),
            ));
        }
        durations.extend(std::iter::repeat_n(duration, count));
    }
    if locations.is_empty() || durations.len() != locations.len() {
        return Err(BotError::Custom(
            "ALAC 时间索引与 packet 数量不一致".to_string(),
        ));
    }
    let mut out = build_alac_stream_header(data)?;
    for (location, duration) in locations.into_iter().zip(durations) {
        let end = location
            .offset
            .checked_add(location.size)
            .ok_or_else(|| BotError::Custom("ALAC packet 范围无效".to_string()))?;
        let mut packet = data
            .get(location.offset..end)
            .filter(|packet| !packet.is_empty())
            .ok_or_else(|| BotError::Custom("ALAC packet 被截断".to_string()))?
            .to_vec();
        patch_alac_packet_for_stream(&mut packet, &params);
        out.extend_from_slice(&(packet.len() as u32).to_be_bytes());
        out.extend_from_slice(&duration.to_be_bytes());
        out.extend_from_slice(&packet);
    }
    Ok(out)
}

fn streaming_assembled_path(streaming_path: &std::path::Path) -> std::path::PathBuf {
    let name = streaming_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio.m4a.streaming");
    streaming_path.with_file_name(format!("{name}.assembled"))
}

async fn decrypt_wrapper_lite_fragment(
    endpoint: &AppleWrapperLiteEndpoint,
    track_id: &str,
    key_uri: &str,
    fragment: &mut Vec<u8>,
    tenc: &TencBox,
) -> Result<(), BotError> {
    let commands =
        collect_wrapper_decrypt_commands(fragment, track_id, &[key_uri.to_string()], tenc)?;
    let groups = group_wrapper_commands_by_fragment(commands)?;
    if groups.len() != 1 {
        return Err(BotError::Custom(
            "Apple Music wrapper-lite fragment 边界异常".to_string(),
        ));
    }
    let group = groups.into_iter().next().unwrap();
    let template = wrapper_lite_template(endpoint, &group.adam, &group.uri).await?;
    let batch_size = SETTINGS
        .music
        .applemusic
        .wrapper_lite_batch_size
        .clamp(1, 4096);
    for jobs in group.jobs.chunks(batch_size) {
        let payloads = jobs
            .iter()
            .map(|job| job.payload.clone())
            .collect::<Vec<_>>();
        let template = Arc::clone(&template);
        let plaintexts = tokio::task::spawn_blocking(move || {
            let samples = payloads.iter().map(Vec::as_slice).collect::<Vec<_>>();
            decrypt_par(template.as_ref(), &samples)
        })
        .await
        .map_err(|error| {
            BotError::Custom(format!("Apple Music wrapper-lite 解密任务失败：{error}"))
        })?;
        if plaintexts.len() != jobs.len() {
            return Err(BotError::Custom(
                "Apple Music wrapper-lite 解密结果数量异常".to_string(),
            ));
        }
        for (job, plaintext) in jobs.iter().zip(plaintexts) {
            if plaintext.len() != job.payload.len()
                || job
                    .ranges
                    .iter()
                    .map(|range| range.end - range.start)
                    .sum::<usize>()
                    != plaintext.len()
            {
                return Err(BotError::Custom(
                    "Apple Music wrapper-lite 解密结果长度异常".to_string(),
                ));
            }
            let mut source_offset = 0usize;
            for range in &job.ranges {
                let length = range.end - range.start;
                let end = source_offset + length;
                if range.end > fragment.len() || end > plaintext.len() {
                    return Err(BotError::Custom(
                        "Apple Music wrapper-lite 解密范围超出 fragment".to_string(),
                    ));
                }
                fragment[range.clone()].copy_from_slice(&plaintext[source_offset..end]);
                source_offset = end;
            }
        }
    }
    Ok(())
}

fn build_streaming_init(data: &[u8]) -> Result<Vec<u8>, BotError> {
    let ftyp = find_child_box(data, 0, data.len(), b"ftyp")
        .ok_or_else(|| BotError::Custom("MP4 缺少 ftyp".to_string()))?;
    let moov = find_child_box(data, 0, data.len(), b"moov")
        .ok_or_else(|| BotError::Custom("MP4 缺少 moov".to_string()))?;
    let mut out = Vec::with_capacity(ftyp.end() + moov.end().saturating_sub(moov.start));
    out.extend_from_slice(&data[ftyp.start..ftyp.end()]);
    out.extend_from_slice(&rewrite_box_for_streaming_init(data, moov)?);
    Ok(out)
}

/// Remove CENC auxiliary boxes from a decrypted fragment before handing it to
/// the browser.  The encrypted init has already been sanitized, but browsers
/// can still interpret `senc`/`saiz`/`saio` in `traf` as an encrypted track.
/// Rebuilding the moof moves the following mdat, so every signed trun
/// `data_offset` is adjusted by the same size delta.
fn sanitize_streaming_fragment(data: &[u8]) -> Result<Vec<u8>, BotError> {
    let mut pos = 0usize;
    let mut moof = None;
    while let Some(box_info) = read_mp4_box(data, pos)? {
        if &box_info.typ == b"moof" {
            moof = Some(box_info);
            break;
        }
        pos = box_info.end();
    }
    let moof = moof.ok_or_else(|| BotError::Custom("MP4 fragment 缺少 moof".to_string()))?;
    let mdat = read_mp4_box(data, moof.end())?
        .filter(|box_info| &box_info.typ == b"mdat")
        .ok_or_else(|| BotError::Custom("MP4 fragment 缺少 mdat".to_string()))?;
    let skeleton = rewrite_fragment_box_for_streaming(data, moof, false, false, 0)?;
    let delta = skeleton.len() as i64 - (moof.end() - moof.start) as i64;
    let rewritten = rewrite_fragment_box_for_streaming(data, moof, false, false, delta)?;
    let mut output = Vec::with_capacity(
        moof.start + rewritten.len() + mdat.end() - mdat.start + data.len() - mdat.end(),
    );
    output.extend_from_slice(&data[..moof.start]);
    output.extend_from_slice(&rewritten);
    output.extend_from_slice(&data[mdat.start..mdat.end()]);
    output.extend_from_slice(&data[mdat.end()..]);
    Ok(output)
}

fn rewrite_fragment_box_for_streaming(
    data: &[u8],
    box_info: Mp4Box,
    in_traf: bool,
    traf_explicit_base: bool,
    delta: i64,
) -> Result<Vec<u8>, BotError> {
    if in_traf
        && matches!(
            &box_info.typ,
            b"senc" | b"saiz" | b"saio" | b"sgpd" | b"sbgp" | b"pssh" | b"uuid"
        )
    {
        return Ok(Vec::new());
    }
    if &box_info.typ == b"tfhd" && in_traf && traf_explicit_base && delta != 0 {
        let mut payload = data[box_info.payload_start()..box_info.end()].to_vec();
        if payload.len() >= 16 {
            let value = u64::from_be_bytes(payload[8..16].try_into().unwrap());
            let adjusted = (value as i128) + delta as i128;
            let adjusted = u64::try_from(adjusted)
                .map_err(|_| BotError::Custom("MP4 tfhd base_data_offset 溢出".to_string()))?;
            payload[8..16].copy_from_slice(&adjusted.to_be_bytes());
        }
        return Ok(build_box(&box_info.typ, payload));
    }
    if &box_info.typ == b"trun" && delta != 0 && !traf_explicit_base {
        let mut payload = data[box_info.payload_start()..box_info.end()].to_vec();
        if payload.len() >= 12 {
            let flags = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
            if flags & 0x000001 != 0 {
                let value = i32::from_be_bytes(payload[8..12].try_into().unwrap()) as i64;
                let adjusted = value
                    .checked_add(delta)
                    .ok_or_else(|| BotError::Custom("MP4 trun data_offset 溢出".to_string()))?;
                let adjusted = i32::try_from(adjusted)
                    .map_err(|_| BotError::Custom("MP4 trun data_offset 超出范围".to_string()))?;
                payload[8..12].copy_from_slice(&adjusted.to_be_bytes());
            }
        }
        return Ok(build_box(&box_info.typ, payload));
    }
    let is_container = matches!(&box_info.typ, b"moof" | b"traf");
    if !is_container {
        return Ok(data[box_info.start..box_info.end()].to_vec());
    }
    let mut payload = Vec::new();
    let child_traf_explicit_base = if &box_info.typ == b"traf" {
        traf_has_explicit_base(data, box_info)?
    } else {
        traf_explicit_base
    };
    let mut pos = box_info.payload_start();
    while let Some(child) = read_mp4_box(data, pos)? {
        if child.end() > box_info.end() {
            break;
        }
        let child_in_traf = in_traf || &box_info.typ == b"traf";
        payload.extend_from_slice(&rewrite_fragment_box_for_streaming(
            data,
            child,
            child_in_traf,
            child_traf_explicit_base,
            delta,
        )?);
        pos = child.end();
    }
    Ok(build_box(&box_info.typ, payload))
}

fn traf_has_explicit_base(data: &[u8], traf: Mp4Box) -> Result<bool, BotError> {
    let mut pos = traf.payload_start();
    while let Some(child) = read_mp4_box(data, pos)? {
        if child.end() > traf.end() {
            break;
        }
        if &child.typ == b"tfhd" {
            let payload = &data[child.payload_start()..child.end()];
            if payload.len() < 4 {
                return Err(BotError::Custom("MP4 tfhd truncated".to_string()));
            }
            let flags = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
            return Ok(flags & 0x000001 != 0);
        }
        pos = child.end();
    }
    Ok(false)
}

fn rewrite_box_for_streaming_init(data: &[u8], box_info: Mp4Box) -> Result<Vec<u8>, BotError> {
    if &box_info.typ == b"stsd" {
        return sanitize_stsd(data, box_info);
    }
    let is_container = matches!(
        &box_info.typ,
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"dinf" | b"mvex"
    );
    if !is_container {
        return Ok(data[box_info.start..box_info.end()].to_vec());
    }
    let mut payload = Vec::new();
    let mut pos = box_info.payload_start();
    while let Some(child) = read_mp4_box(data, pos)? {
        if child.end() > box_info.end() {
            break;
        }
        payload.extend_from_slice(&rewrite_box_for_streaming_init(data, child)?);
        pos = child.end();
    }
    Ok(build_box(&box_info.typ, payload))
}

async fn wrapper_lite_m3u8_url(
    endpoint: &AppleWrapperLiteEndpoint,
    track_id: &str,
) -> Result<String, BotError> {
    let data: AppleWrapperLiteM3u8Data =
        wrapper_lite_get_json(endpoint, "/m3u8", &[("adamId", track_id)]).await?;
    let url = data.m3u8.trim().to_string();
    if url.is_empty() || url == "\0" {
        return Err(BotError::Custom(
            "Apple Music wrapper-lite 没有返回 m3u8".to_string(),
        ));
    }
    Ok(url)
}

async fn wrapper_lite_template(
    endpoint: &AppleWrapperLiteEndpoint,
    adam_id: &str,
    key_uri: &str,
) -> Result<Arc<temari::rounds::Template>, BotError> {
    let cache_key = (endpoint.base_url.clone(), format!("{adam_id}\\0{key_uri}"));
    if let Ok(cache) = APPLE_WRAPPER_LITE_TEMPLATES.read()
        && let Some(cached) = cache.get(&cache_key)
        && cached.fetched_at.elapsed() < APPLE_WRAPPER_LITE_CACHE_TTL
    {
        return Ok(Arc::clone(&cached.template));
    }
    let data: AppleWrapperLiteKeyData =
        wrapper_lite_get_json(endpoint, "/key", &[("adamId", adam_id), ("uri", key_uri)]).await?;
    if data.ctx.trim().is_empty() || data.state.trim().is_empty() {
        return Err(BotError::Custom(
            "Apple Music wrapper-lite key 缺少 Temari ctx/state".to_string(),
        ));
    }
    let template_json = serde_json::json!({
        "ctx": data.ctx,
        "state": data.state,
        "rcx": data.rcx,
        "rax": data.rax,
        "rdx": data.rdx,
        "r9": data.r9,
        "rbp": data.rbp,
    })
    .to_string();
    let template = template_from_json(&template_json).map_err(|error| {
        BotError::Custom(format!("Apple Music wrapper-lite 模板解析失败：{error}"))
    })?;
    let template = Arc::new(template);
    if let Ok(mut cache) = APPLE_WRAPPER_LITE_TEMPLATES.write() {
        cache.insert(
            cache_key,
            CachedWrapperLiteTemplate {
                fetched_at: Instant::now(),
                template: Arc::clone(&template),
            },
        );
    }
    Ok(template)
}

async fn wrapper_lite_get_json<T: DeserializeOwned>(
    endpoint: &AppleWrapperLiteEndpoint,
    path: &str,
    query: &[(&str, &str)],
) -> Result<T, BotError> {
    let url = format!(
        "{}/{}",
        endpoint.base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let response = tokio::time::timeout(
        APPLE_WRAPPER_LITE_TIMEOUT,
        CLIENT
            .get(&url)
            .query(query)
            .header("User-Agent", APPLE_MUSIC_UA)
            .send(),
    )
    .await
    .map_err(|_| BotError::Custom(format!("Apple Music wrapper-lite 请求超时：{path}")))??;
    if !response.status().is_success() {
        return Err(BotError::Custom(format!(
            "Apple Music wrapper-lite {path} 请求失败：HTTP {}",
            response.status()
        )));
    }
    let envelope: AppleWrapperLiteResponse<T> =
        tokio::time::timeout(APPLE_WRAPPER_LITE_TIMEOUT, response.json())
            .await
            .map_err(|_| {
                BotError::Custom(format!("Apple Music wrapper-lite 响应超时：{path}"))
            })??;
    if envelope.code != 0 {
        return Err(BotError::Custom(format!(
            "Apple Music wrapper-lite {path} 失败：{}",
            if envelope.msg.trim().is_empty() {
                "未知错误"
            } else {
                envelope.msg.trim()
            }
        )));
    }
    envelope
        .data
        .ok_or_else(|| BotError::Custom(format!("Apple Music wrapper-lite {path} 缺少 data")))
}

fn validate_wrapper_lite_cenc_layout(data: &[u8], tenc: &TencBox) -> Result<(), BotError> {
    if tenc.default_crypt_byte_block != 0 || tenc.default_skip_byte_block != 0 {
        return Err(BotError::Custom(
            "Apple Music wrapper-lite 暂不支持 pattern encryption，回退 raw wrapper".to_string(),
        ));
    }
    let mut pos = 0usize;
    while let Some(moof) = read_mp4_box(data, pos)? {
        if &moof.typ == b"moof" {
            let mut child = moof.payload_start();
            while let Some(traf) = read_mp4_box(data, child)? {
                if traf.end() > moof.end() {
                    break;
                }
                if &traf.typ == b"traf" {
                    let parsed = parse_traf(data, traf, tenc)?;
                    if let Some(senc) = &parsed.senc {
                        let samples = parsed.runs.iter().flat_map(|run| &run.samples);
                        if senc.samples.len() != samples.clone().count() {
                            return Err(BotError::Custom(
                                "Apple Music senc 与 trun sample 数量不一致".to_string(),
                            ));
                        }
                        for (sample, encryption) in samples.zip(&senc.samples) {
                            validate_wrapper_lite_subsamples(sample.size, &encryption.subsamples)?;
                        }
                    }
                }
                child = traf.end();
            }
        }
        pos = moof.end();
    }
    Ok(())
}

fn validate_wrapper_lite_subsamples(
    size: u32,
    subsamples: &[SubsampleEntry],
) -> Result<(), BotError> {
    if subsamples.is_empty() {
        return Ok(());
    }
    // Apple audio commonly declares a single protected span in `senc`, even
    // when it covers the whole sample. The existing scatter/gather collector
    // already preserves its clear prefix and non-block-aligned clear tail.
    // Temari resets its cipher state for each job. Keep multiple encrypted
    // spans disabled until the scheme-specific CBC chaining is explicit.
    if subsamples
        .iter()
        .filter(|entry| entry.bytes_of_protected_data > 0)
        .count()
        > 1
    {
        return Err(BotError::Custom(
            "Apple Music wrapper-lite 暂不支持同一 sample 多段加密，回退 raw wrapper".to_string(),
        ));
    }
    let mut covered = 0u64;
    for entry in subsamples {
        covered += u64::from(entry.bytes_of_clear_data) + u64::from(entry.bytes_of_protected_data);
        if covered > u64::from(size) {
            return Err(BotError::Custom(
                "Apple Music senc 范围超出 sample".to_string(),
            ));
        }
    }
    if covered != u64::from(size) {
        return Err(BotError::Custom(
            "Apple Music senc 未完整描述 sample".to_string(),
        ));
    }
    Ok(())
}

async fn download_via_wrapper_endpoint<F>(
    track_id: &str,
    quality: AppleMusicQuality,
    expected_duration: Option<u32>,
    progress: &mut F,
    endpoint: AppleWrapperEndpoint,
    catalog_master_url: &mut Option<Result<String, String>>,
) -> Result<(Vec<u8>, Option<Duration>), BotError>
where
    F: FnMut(DownloadProgress) + Send,
{
    let master_url =
        enhanced_hls_master_url(&endpoint, track_id, quality, catalog_master_url).await?;
    if !is_m3u8_url(&master_url) {
        return Err(BotError::Custom(format!(
            "Apple Music wrapper 返回了无效 m3u8：{master_url}"
        )));
    }

    let master = download_text(&master_url).await?;
    let variants = parse_enhanced_hls_master(&master)?;
    let variant = select_variant_for_quality(&variants, quality)
        .ok_or_else(|| BotError::Custom("Apple Music enhancedHls 没有匹配的音质".to_string()))?;
    let media_url = absolute_hls_url(&master_url, &variant.uri);
    let media = parse_enhanced_hls_media(&media_url, &download_text(&media_url).await?)?;
    validate_hls_media_duration(media.duration_seconds, expected_duration)?;
    let mut encrypted = download_enhanced_hls_media_direct_or_range(&media, progress).await?;
    let mut last_error = None;
    for attempt in 0..2 {
        // Only keep a pristine copy while a retry can still need it; the final
        // attempt consumes the buffer.
        let payload = if attempt == 0 {
            encrypted.clone()
        } else {
            std::mem::take(&mut encrypted)
        };
        match decrypt_fmp4_with_wrapper(&endpoint, track_id, payload, &media.seg_keys).await {
            Ok(data) => return Ok((data.audio, Some(data.decrypt_elapsed))),
            Err(e) if attempt == 0 && is_retryable_wrapper_error(&e) => {
                last_error = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_error.unwrap_or_else(|| BotError::Custom("Apple Music wrapper 解密失败".to_string())))
}

async fn enhanced_hls_master_url(
    endpoint: &AppleWrapperEndpoint,
    track_id: &str,
    quality: AppleMusicQuality,
    catalog_master_url: &mut Option<Result<String, String>>,
) -> Result<String, BotError> {
    if quality == AppleMusicQuality::HiRes {
        match wrapper_get_m3u8_url(endpoint, track_id).await {
            Ok(url) if is_m3u8_url(&url) => return Ok(url),
            Ok(url) => {
                eprintln!(
                    "Apple Music wrapper device m3u8 invalid for {track_id}: {url}; falling back to catalog enhancedHls"
                );
            }
            Err(err) => {
                eprintln!(
                    "Apple Music wrapper device m3u8 unavailable for {track_id}: {err}; falling back to catalog enhancedHls"
                );
            }
        }
    }

    let catalog_result = match catalog_master_url.as_ref() {
        Some(result) => result.clone(),
        None => catalog_master_url
            .insert(
                get_catalog_enhanced_hls_master_url(track_id)
                    .await
                    .map_err(|err| err.to_string()),
            )
            .clone(),
    };
    // A catalog URL that is not an .m3u8 is treated like a catalog failure so
    // the wrapper device m3u8 fallback still runs and the error names the
    // actual source instead of blaming the wrapper.
    let catalog_err = match catalog_result {
        Ok(url) if is_m3u8_url(&url) => return Ok(url),
        Ok(url) => format!("Apple Music catalog enhancedHls 无效：{url}"),
        Err(err) => err,
    };
    if quality == AppleMusicQuality::HiRes {
        return Err(BotError::Custom(catalog_err));
    }
    match wrapper_get_m3u8_url(endpoint, track_id).await {
        Ok(url) if is_m3u8_url(&url) => Ok(url),
        Ok(url) => Err(BotError::Custom(format!(
            "Apple Music catalog enhancedHls 获取失败：{catalog_err}；wrapper device m3u8 无效：{url}"
        ))),
        Err(wrapper_err) => Err(BotError::Custom(format!(
            "Apple Music catalog enhancedHls 获取失败：{catalog_err}；wrapper device m3u8 也失败：{wrapper_err}"
        ))),
    }
}

fn select_widevine_asset(assets: &[WebPlaybackAsset]) -> Option<&WebPlaybackAsset> {
    assets
        .iter()
        .find(|asset| asset.flavor.as_deref() == Some("28:ctrp256"))
        .or_else(|| assets.iter().max_by_key(|asset| asset.metadata.bit_rate))
}

fn validate_webplayback_asset_duration(
    asset: &WebPlaybackAsset,
    expected_duration: Option<u32>,
) -> Result<(), BotError> {
    let Some(expected_duration) = expected_duration.filter(|duration| *duration > 60) else {
        return Ok(());
    };
    let actual_millis = asset.metadata.duration;
    if actual_millis <= 0 {
        return Ok(());
    }
    let actual_duration = ((actual_millis as u64 + 500) / 1000) as u32;
    if actual_duration.saturating_add(5) < expected_duration {
        return Err(apple_preview_error(expected_duration, actual_duration));
    }
    Ok(())
}

fn apple_preview_error(expected_duration: u32, actual_duration: u32) -> BotError {
    BotError::Custom(format!(
        "Apple Music 只返回试听片段（{actual_duration}s/{expected_duration}s），请检查 Apple Music Cookie 或 wrapper 登录状态"
    ))
}

#[cfg(test)]
fn is_apple_preview_error(err: &BotError) -> bool {
    matches!(err, BotError::Custom(message) if message.contains("只返回试听片段"))
}

fn validate_hls_media_duration(
    duration_seconds: f64,
    expected_duration: Option<u32>,
) -> Result<(), BotError> {
    let Some(expected_duration) = expected_duration.filter(|duration| *duration > 60) else {
        return Ok(());
    };
    if duration_seconds <= 0.0 {
        return Ok(());
    }
    let actual_duration = duration_seconds.round() as u32;
    if actual_duration.saturating_add(5) < expected_duration {
        return Err(apple_preview_error(expected_duration, actual_duration));
    }
    Ok(())
}

struct AppleWrapperDecryptOutput {
    audio: Vec<u8>,
    decrypt_elapsed: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WidevineHlsMedia {
    mp4_url: String,
    kid_b64: String,
    kid: [u8; 16],
    uri_prefix: String,
}

fn parse_widevine_hls(m3u8_url: &str, content: &str) -> Result<WidevineHlsMedia, BotError> {
    let mut mp4_url = String::new();
    let mut kid_b64 = String::new();
    let mut uri_prefix = String::new();
    for line in content.lines().map(str::trim) {
        if line.contains("#EXT-X-KEY")
            && let Some(uri) = extract_quoted_attr(line, "URI")
            && let Some((prefix, kid)) = uri.split_once(',')
        {
            uri_prefix = prefix.to_string();
            kid_b64 = kid.to_string();
        }
        if line.contains("#EXT-X-MAP")
            && let Some(uri) = extract_quoted_attr(line, "URI")
        {
            mp4_url = absolute_hls_url(m3u8_url, &uri);
        }
        if !line.starts_with('#')
            && !line.is_empty()
            && (line.ends_with(".mp4") || line.ends_with(".m4a") || line.ends_with(".m4s"))
            && mp4_url.is_empty()
        {
            mp4_url = absolute_hls_url(m3u8_url, line);
        }
    }
    if mp4_url.is_empty() {
        return Err(BotError::Custom(
            "Apple Music Widevine playlist 缺少 MP4 URL".to_string(),
        ));
    }
    let kid_vec = general_purpose::STANDARD
        .decode(kid_b64.as_bytes())
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine KID 解码失败：{e}")))?;
    let kid: [u8; 16] = kid_vec
        .try_into()
        .map_err(|_| BotError::Custom("Apple Music Widevine KID 长度错误".to_string()))?;
    Ok(WidevineHlsMedia {
        mp4_url,
        kid_b64,
        kid,
        uri_prefix,
    })
}

fn extract_quoted_attr(line: &str, attr: &str) -> Option<String> {
    let marker = format!(r#"{attr}=""#);
    let start = line.find(&marker)? + marker.len();
    let end = line[start..].find('"')?;
    Some(line[start..start + end].to_string())
}

async fn acquire_widevine_content_key(
    track_id: &str,
    kid: &[u8; 16],
    uri_prefix: &str,
    kid_b64: &str,
    media_user_token: &str,
) -> Result<Vec<u8>, BotError> {
    let device = load_widevine_device()?;
    let pssh = build_widevine_pssh(kid)?;
    let request = Cdm::new(device)
        .open()
        .get_license_request(pssh, LicenseType::AUTOMATIC)
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine challenge 生成失败：{e}")))?;
    let challenge = request
        .challenge()
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine challenge 签名失败：{e}")))?;
    let developer_token = ensure_developer_token().await?;
    let response = CLIENT
        .post(WEB_PLAYBACK_LICENSE_URL)
        .header("Authorization", format!("Bearer {developer_token}"))
        .header("Content-Type", "application/json")
        .header("Origin", APPLE_MUSIC_ORIGIN)
        .header("User-Agent", APPLE_MUSIC_UA)
        .header("media-user-token", media_user_token)
        .json(&serde_json::json!({
            "challenge": general_purpose::STANDARD.encode(challenge),
            "key-system": "com.widevine.alpha",
            "uri": format!("{uri_prefix},{kid_b64}"),
            "adamId": track_id,
            "isLibrary": false,
            "user-initiated": true,
        }))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(BotError::Custom(format!(
            "Apple Music Widevine license 请求失败：HTTP {}",
            response.status()
        )));
    }
    let response: WidevineLicenseResponse = response.json().await?;
    if response.status != 0 {
        return Err(BotError::Custom(format!(
            "Apple Music Widevine license 状态异常：{}",
            response.status
        )));
    }
    let license = general_purpose::STANDARD
        .decode(response.license.as_bytes())
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine license 解码失败：{e}")))?;
    let keys = request
        .get_keys(&license)
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine license 解析失败：{e}")))?;
    let key = keys
        .content_key(kid)
        .or_else(|_| keys.first_of_type(KeyType::CONTENT))
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine content key 缺失：{e}")))?;
    Ok(key.key.clone())
}

#[derive(Deserialize)]
struct WidevineLicenseResponse {
    license: String,
    status: i32,
}

fn build_widevine_pssh(kid: &[u8; 16]) -> Result<Pssh, BotError> {
    let mut data = WidevinePsshData::new();
    data.key_ids.push(kid.to_vec());
    data.set_algorithm(widevine_pssh_data::Algorithm::AESCTR);
    let bytes = data
        .write_to_bytes()
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine PSSH 序列化失败：{e}")))?;
    Pssh::from_bytes(&bytes)
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine PSSH 解析失败：{e}")))
}

fn load_widevine_device() -> Result<Device, BotError> {
    let client_id_path = SETTINGS.music.applemusic.wv_client_id.trim();
    let private_key_path = SETTINGS.music.applemusic.wv_private_key.trim();
    if !client_id_path.is_empty() && !private_key_path.is_empty() {
        let client_id = fs::read(client_id_path).map_err(|e| {
            BotError::Custom(format!("Apple Music Widevine client_id 读取失败：{e}"))
        })?;
        let private_key = fs::read_to_string(private_key_path).map_err(|e| {
            BotError::Custom(format!("Apple Music Widevine private_key 读取失败：{e}"))
        })?;
        let private_key = RsaPrivateKey::from_pkcs1_pem(&private_key).map_err(|e| {
            BotError::Custom(format!("Apple Music Widevine private_key 解析失败：{e}"))
        })?;
        return Device::new(
            DeviceType::ANDROID,
            SecurityLevel::L3,
            private_key,
            &client_id,
        )
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine device 初始化失败：{e}")));
    }

    let device_path = SETTINGS.music.applemusic.wv_device.trim();
    if device_path.is_empty() {
        return Err(BotError::Custom(
            "Apple Music Widevine 未配置设备；请提供 wv_device 或 wv_client_id / wv_private_key"
                .into(),
        ));
    }
    let bytes = fs::read(device_path)
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine device 读取失败：{e}")))?;
    Device::read_wvd(Cursor::new(bytes))
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine device 初始化失败：{e}")))
}

fn decrypt_cenc_fmp4(mut data: Vec<u8>, key: &[u8]) -> Result<Vec<u8>, BotError> {
    if key.len() != 16 {
        return Err(BotError::Custom(
            "Apple Music Widevine content key 长度错误".to_string(),
        ));
    }
    let tenc = parse_tenc_from_init(&data)?;
    if tenc.default_crypt_byte_block != 0 || tenc.default_skip_byte_block != 0 {
        return Err(BotError::Custom(
            "Apple Music Widevine native 解密暂不支持 pattern encryption".to_string(),
        ));
    }

    let mut pos = 0usize;
    while let Some(box_info) = read_mp4_box(&data, pos)? {
        if &box_info.typ != b"moof" {
            pos = box_info.end();
            continue;
        }
        let moof = box_info;
        let mdat = read_mp4_box(&data, moof.end())?
            .filter(|next| &next.typ == b"mdat")
            .ok_or_else(|| BotError::Custom("MP4 moof 后缺少 mdat".to_string()))?;
        let mut traf_pos = moof.payload_start();
        while let Some(traf) = read_mp4_box(&data, traf_pos)? {
            if traf.end() > moof.end() {
                break;
            }
            if &traf.typ == b"traf" {
                let parsed = parse_traf(&data, traf, &tenc)?;
                let mut next_sample_start = mdat.payload_start();
                for run in &parsed.runs {
                    let start = trun_run_start(moof.start, run.data_offset, next_sample_start)?;
                    next_sample_start = decrypt_cenc_samples(
                        &mut data,
                        start,
                        mdat.end(),
                        &run.samples,
                        parsed.senc.as_ref(),
                        run.senc_sample_offset,
                        &tenc,
                        key,
                    )?;
                }
            }
            traf_pos = traf.end();
        }
        pos = mdat.end();
    }

    remux_decrypted_fmp4_to_progressive(&data)
}

#[allow(clippy::too_many_arguments)]
fn decrypt_cenc_samples(
    data: &mut [u8],
    sample_start: usize,
    sample_end: usize,
    samples: &[TrunSampleInfo],
    senc: Option<&SencBox>,
    senc_sample_offset: usize,
    tenc: &TencBox,
    key: &[u8],
) -> Result<usize, BotError> {
    let mut pos = sample_start;
    for (index, sample) in samples.iter().enumerate() {
        let end = checked_sample_end(pos, sample.size, sample_end.min(data.len()))?;
        let senc_sample = senc.and_then(|senc| senc.samples.get(senc_sample_offset + index));
        let iv = cenc_sample_iv(senc_sample, tenc)?;
        let subsamples = senc_sample
            .map(|sample| sample.subsamples.as_slice())
            .unwrap_or(&[]);
        decrypt_cenc_sample(&mut data[pos..end], subsamples, key, &iv)?;
        pos = end;
    }
    Ok(pos)
}

fn cenc_sample_iv(
    sample: Option<&oxideav_mp4::cenc::SencSample>,
    tenc: &TencBox,
) -> Result<[u8; 16], BotError> {
    let iv = sample
        .map(|sample| sample.initialization_vector.as_slice())
        .filter(|iv| !iv.is_empty())
        .or(tenc.default_constant_iv.as_deref())
        .ok_or_else(|| BotError::Custom("Apple Music Widevine sample IV 缺失".to_string()))?;
    let mut out = [0u8; 16];
    match iv.len() {
        8 => out[..8].copy_from_slice(iv),
        16 => out.copy_from_slice(iv),
        len => {
            return Err(BotError::Custom(format!(
                "Apple Music Widevine sample IV 长度错误：{len}"
            )));
        }
    }
    Ok(out)
}

fn decrypt_cenc_sample(
    sample: &mut [u8],
    subsamples: &[SubsampleEntry],
    key: &[u8],
    iv: &[u8; 16],
) -> Result<(), BotError> {
    let mut cipher = Aes128Ctr::new_from_slices(key, iv)
        .map_err(|e| BotError::Custom(format!("Apple Music Widevine AES-CTR 初始化失败：{e}")))?;
    if subsamples.is_empty() {
        cipher.apply_keystream(sample);
        return Ok(());
    }

    let mut pos = 0usize;
    for subsample in subsamples {
        pos += subsample.bytes_of_clear_data as usize;
        let end = pos + subsample.bytes_of_protected_data as usize;
        if end > sample.len() {
            return Err(BotError::Custom(
                "MP4 subsample protected range 超出 sample".to_string(),
            ));
        }
        cipher.apply_keystream(&mut sample[pos..end]);
        pos = end;
    }
    Ok(())
}

fn is_retryable_wrapper_error(err: &BotError) -> bool {
    match err {
        BotError::Custom(message) => {
            message.contains("wrapper") || message.contains("decrypt") || message.contains("CKC")
        }
        BotError::IOError(_) => true,
        _ => false,
    }
}

async fn wrapper_get_m3u8_url(
    endpoint: &AppleWrapperEndpoint,
    track_id: &str,
) -> Result<String, BotError> {
    let mut stream = tokio::time::timeout(
        APPLE_WRAPPER_M3U8_TIMEOUT,
        tokio::net::TcpStream::connect(&endpoint.m3u8_addr),
    )
    .await
    .map_err(|_| {
        BotError::Custom(format!(
            "Apple Music wrapper m3u8 连接超时：{}",
            endpoint.m3u8_addr
        ))
    })??;
    stream.set_nodelay(true)?;
    let id = track_id.as_bytes();
    if id.len() > u8::MAX as usize {
        return Err(BotError::Custom("Apple Music track id 过长".to_string()));
    }
    wrapper_write_all(&mut stream, &[id.len() as u8], "m3u8 track id length").await?;
    wrapper_write_all(&mut stream, id, "m3u8 track id").await?;
    let mut reader = tokio::io::BufReader::new(stream);
    let mut url = String::new();
    tokio::time::timeout(APPLE_WRAPPER_M3U8_TIMEOUT, reader.read_line(&mut url))
        .await
        .map_err(|_| BotError::Custom("Apple Music wrapper m3u8 读取超时".to_string()))??;
    let url = url.trim().to_string();
    if url.is_empty() || url == "\0" {
        return Err(BotError::Custom(
            "Apple Music wrapper 没有返回 m3u8".to_string(),
        ));
    }
    Ok(url)
}

async fn download_text(url: &str) -> Result<String, BotError> {
    let response = CLIENT
        .get(url)
        .header("User-Agent", APPLE_MUSIC_UA)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(BotError::Custom(format!(
            "Apple Music 下载 playlist 失败：HTTP {}",
            response.status()
        )));
    }
    Ok(response.text().await?)
}

async fn download_bytes_with_progress<F>(url: &str, progress: &mut F) -> Result<Vec<u8>, BotError>
where
    F: FnMut(DownloadProgress) + Send,
{
    let response = MEDIA_CLIENT
        .get(url)
        .header("User-Agent", APPLE_MUSIC_UA)
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(BotError::Custom(
            "Apple Music 下载媒体失败：未请求分段却收到 206 Partial Content".to_string(),
        ));
    }
    if !response.status().is_success() {
        return Err(BotError::Custom(format!(
            "Apple Music 下载媒体失败：HTTP {}",
            response.status()
        )));
    }
    let total = response.content_length();
    let mut response = response;
    let mut buf = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        buf.extend_from_slice(&chunk);
        progress(DownloadProgress {
            written: buf.len() as u64,
            total,
        });
    }
    Ok(buf)
}

async fn download_enhanced_hls_media_direct_or_range<F>(
    media: &EnhancedHlsMedia,
    progress: &mut F,
) -> Result<Vec<u8>, BotError>
where
    F: FnMut(DownloadProgress) + Send,
{
    match download_bytes_with_progress(&media.mp4_url, progress).await {
        Ok(bytes) => match validate_enhanced_hls_direct_download(&bytes, media) {
            Ok(()) => Ok(bytes),
            Err(err) if !media.parts.is_empty() => {
                match download_enhanced_hls_media_with_progress(media, progress).await {
                    Ok(bytes) => Ok(bytes),
                    Err(fallback_err) => Err(BotError::Custom(format!(
                        "Apple Music encrypted MP4 直下不完整：{err}；range fallback 也失败：{fallback_err}"
                    ))),
                }
            }
            Err(err) => Err(err),
        },
        Err(err) if !media.parts.is_empty() => {
            match download_enhanced_hls_media_with_progress(media, progress).await {
                Ok(bytes) => Ok(bytes),
                Err(fallback_err) => Err(BotError::Custom(format!(
                    "Apple Music encrypted MP4 直下失败：{err}；range fallback 也失败：{fallback_err}"
                ))),
            }
        }
        Err(err) => Err(err),
    }
}

fn validate_enhanced_hls_direct_download(
    bytes: &[u8],
    media: &EnhancedHlsMedia,
) -> Result<(), BotError> {
    let Some(expected_size) = enhanced_hls_media_expected_size(media) else {
        return Ok(());
    };
    if (bytes.len() as u64) < expected_size {
        return Err(BotError::Custom(format!(
            "Apple Music encrypted MP4 长度异常：{} < {}",
            bytes.len(),
            expected_size
        )));
    }
    Ok(())
}

fn enhanced_hls_media_expected_size(media: &EnhancedHlsMedia) -> Option<u64> {
    media
        .parts
        .iter()
        .filter_map(|part| part.range.map(HlsByteRange::end))
        .max()
}

async fn download_enhanced_hls_media_with_progress<F>(
    media: &EnhancedHlsMedia,
    progress: &mut F,
) -> Result<Vec<u8>, BotError>
where
    F: FnMut(DownloadProgress) + Send,
{
    if media.parts.is_empty() {
        return download_bytes_with_progress(&media.mp4_url, progress).await;
    }
    let total = media
        .parts
        .iter()
        .map(|part| part.range.map(|range| range.length))
        .try_fold(0u64, |sum, length| length.map(|length| sum + length));
    let mut buf = Vec::new();
    for part in &media.parts {
        let bytes = download_enhanced_hls_part(part).await?;
        buf.extend_from_slice(&bytes);
        progress(DownloadProgress {
            written: buf.len() as u64,
            total,
        });
    }
    Ok(buf)
}

async fn download_enhanced_hls_part(part: &EnhancedHlsPart) -> Result<Vec<u8>, BotError> {
    download_enhanced_hls_part_with_client(part, &MEDIA_CLIENT).await
}

async fn download_enhanced_hls_part_with_retry(
    part: &EnhancedHlsPart,
    client: &Client,
) -> Result<Vec<u8>, BotError> {
    let mut last_error = None;
    for attempt in 0..3u32 {
        match download_enhanced_hls_part_with_client(part, client).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                last_error = Some(error);
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(200 * (1u64 << attempt))).await;
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| BotError::Custom("Apple Music 媒体分片下载失败".into())))
}

async fn download_enhanced_hls_part_with_client(
    part: &EnhancedHlsPart,
    client: &Client,
) -> Result<Vec<u8>, BotError> {
    let mut request = client.get(&part.url).header("User-Agent", APPLE_MUSIC_UA);
    if let Some(range) = part.range {
        request = request.header(
            reqwest::header::RANGE,
            format!(
                "bytes={}-{}",
                range.offset,
                range.offset.saturating_add(range.length).saturating_sub(1)
            ),
        );
    }
    let response = request.send().await?;
    if part.range.is_none() && response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(BotError::Custom(
            "Apple Music 下载媒体分片失败：未请求分段却收到 206 Partial Content".to_string(),
        ));
    }
    if !response.status().is_success() {
        return Err(BotError::Custom(format!(
            "Apple Music 下载媒体分片失败：HTTP {}",
            response.status()
        )));
    }
    let bytes = response.bytes().await?.to_vec();
    if let Some(range) = part.range
        && bytes.len() as u64 != range.length
    {
        return Err(BotError::Custom(format!(
            "Apple Music 媒体分片长度异常：{} != {}",
            bytes.len(),
            range.length
        )));
    }
    Ok(bytes)
}

async fn apple_get_json<T: serde::de::DeserializeOwned>(
    url: &str,
    retry: bool,
) -> Result<T, BotError> {
    let token = ensure_developer_token().await?;
    let response = CLIENT
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Origin", APPLE_MUSIC_ORIGIN)
        .header("User-Agent", APPLE_MUSIC_UA)
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED && retry {
        clear_developer_token();
        return Box::pin(apple_get_json(url, false)).await;
    }
    if !response.status().is_success() {
        return Err(BotError::Custom(format!(
            "Apple Music API 请求失败：HTTP {}",
            response.status()
        )));
    }
    Ok(response.json().await?)
}

async fn apple_get_json_with_media_user_token<T: serde::de::DeserializeOwned>(
    url: &str,
    media_user_token: &str,
    retry: bool,
) -> Result<T, BotError> {
    let token = ensure_developer_token().await?;
    let response = CLIENT
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Origin", APPLE_MUSIC_ORIGIN)
        .header("User-Agent", APPLE_MUSIC_UA)
        .header("media-user-token", media_user_token)
        .header("Cookie", format!("media-user-token={media_user_token}"))
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED && retry {
        clear_developer_token();
        return Box::pin(apple_get_json_with_media_user_token(
            url,
            media_user_token,
            false,
        ))
        .await;
    }
    if !response.status().is_success() {
        return Err(BotError::Custom(format!(
            "Apple Music API 请求失败：HTTP {}",
            response.status()
        )));
    }
    Ok(response.json().await?)
}

async fn ensure_developer_token() -> Result<String, BotError> {
    if let Some(token) = DEVELOPER_TOKEN.read().unwrap().clone() {
        return Ok(token);
    }
    // Single-flight the refresh: concurrent cold-cache callers would otherwise
    // each download music.apple.com's HTML plus the full JS bundle.
    let _guard = DEVELOPER_TOKEN_FETCH_LOCK.lock().await;
    if let Some(token) = DEVELOPER_TOKEN.read().unwrap().clone() {
        return Ok(token);
    }
    let token = fetch_developer_token().await?;
    *DEVELOPER_TOKEN.write().unwrap() = Some(token.clone());
    Ok(token)
}

async fn fetch_developer_token() -> Result<String, BotError> {
    let html = CLIENT
        .get(APPLE_MUSIC_ORIGIN)
        .header("User-Agent", APPLE_MUSIC_UA)
        .send()
        .await?
        .text()
        .await?;
    let js_path = JS_ASSET_PATTERN
        .find(&html)
        .ok_or_else(|| BotError::Custom("Apple Music 首页没有找到 JS bundle".to_string()))?
        .as_str();
    let js_url = format!("{APPLE_MUSIC_ORIGIN}{js_path}");
    let js = CLIENT
        .get(js_url)
        .header("User-Agent", APPLE_MUSIC_UA)
        .send()
        .await?
        .text()
        .await?;
    TOKEN_PATTERN
        .find(&js)
        .map(|value| value.as_str().to_string())
        .ok_or_else(|| BotError::Custom("Apple Music JS 中没有找到 developer token".to_string()))
}

fn clear_developer_token() {
    *DEVELOPER_TOKEN.write().unwrap() = None;
}

fn apple_search_url(keyword: &str, limit: usize) -> String {
    let mut url = Url::parse(&format!(
        "{APPLE_MUSIC_API}/v1/catalog/{}/search",
        apple_storefront()
    ))
    .unwrap();
    url.query_pairs_mut()
        .append_pair("term", keyword)
        .append_pair("types", "songs")
        .append_pair("limit", &limit.to_string())
        .append_pair("l", &apple_language());
    url.to_string()
}

fn apple_song_url(id: &str) -> String {
    let mut url = Url::parse(&format!(
        "{APPLE_MUSIC_API}/v1/catalog/{}/songs/{id}",
        apple_storefront()
    ))
    .unwrap();
    url.query_pairs_mut()
        .append_pair("include", "albums,artists")
        .append_pair("extend", "extendedAssetUrls")
        .append_pair("l", &apple_language());
    url.to_string()
}

fn apple_collection_url(collection: &AppleCollectionId) -> String {
    let resource = match collection.kind {
        AppleCollectionKind::Album => "albums",
        AppleCollectionKind::Playlist => "playlists",
    };
    let mut url = Url::parse(&format!(
        "{APPLE_MUSIC_API}/v1/catalog/{}/{resource}/{}",
        apple_storefront(),
        collection.id
    ))
    .unwrap();
    url.query_pairs_mut()
        .append_pair("include", "tracks")
        .append_pair("l", &apple_language());
    url.to_string()
}

fn apple_lyrics_url(id: &str) -> String {
    let mut url = Url::parse(&format!(
        "{APPLE_MUSIC_API}/v1/catalog/{}/songs/{id}/lyrics",
        apple_storefront()
    ))
    .unwrap();
    url.query_pairs_mut().append_pair("l", &apple_language());
    url.to_string()
}

fn song_to_item(song: AppleMusicResource) -> MusicSearchItem {
    let AppleMusicResource { id, attributes } = song;
    let cover = format_artwork_url(attributes.artwork.as_ref(), DEFAULT_ARTWORK_SIZE);
    MusicSearchItem {
        platform: MusicPlatform::AppleMusic,
        id,
        song: attributes.name,
        singer: if attributes.artist_name.trim().is_empty() {
            "未知歌手".to_string()
        } else {
            attributes.artist_name
        },
        cover,
    }
}

fn parse_apple_track_id(text: &str) -> Option<String> {
    let text = text.trim();
    if text.chars().all(|c| c.is_ascii_digit()) && text.len() >= 6 {
        return Some(text.to_string());
    }
    if let Some((prefix, value)) = text.split_once(':')
        && MusicPlatform::from_alias(prefix).is_some()
        && value.len() >= 6
        && value.chars().all(|c| c.is_ascii_digit())
    {
        return Some(value.trim().to_string());
    }
    let url_text = APPLE_URL_PATTERN
        .find(text)
        .map(|value| {
            value
                .as_str()
                .trim_end_matches(&['.', ',', '!', '?', ')', ']', '}'][..])
        })
        .unwrap_or(text);
    let url = Url::parse(url_text).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if host != "music.apple.com" && !host.ends_with(".music.apple.com") {
        return None;
    }
    for key in ["i", "songId"] {
        if let Some(id) = url
            .query_pairs()
            .find(|(query_key, _)| query_key == key)
            .map(|(_, value)| value.to_string())
            .filter(|value| value.len() >= 6 && value.chars().all(|c| c.is_ascii_digit()))
        {
            return Some(id);
        }
    }
    APPLE_SONG_PATH_RE
        .captures(url.path())
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppleCollectionKind {
    Album,
    Playlist,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AppleCollectionId {
    kind: AppleCollectionKind,
    id: String,
}

fn parse_apple_collection_id(text: &str) -> Option<AppleCollectionId> {
    let text = text.trim();
    if let Some(album_id) = text.strip_prefix("album:")
        && is_apple_numeric_id(album_id)
    {
        return Some(AppleCollectionId {
            kind: AppleCollectionKind::Album,
            id: album_id.trim().to_string(),
        });
    }
    if text.starts_with("pl.") {
        return Some(AppleCollectionId {
            kind: AppleCollectionKind::Playlist,
            id: text.to_string(),
        });
    }
    let url_text = APPLE_URL_PATTERN
        .find(text)
        .map(|value| {
            value
                .as_str()
                .trim_end_matches(&['.', ',', '!', '?', ')', ']', '}'][..])
        })
        .unwrap_or(text);
    let url = Url::parse(url_text).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if host != "music.apple.com" && !host.ends_with(".music.apple.com") {
        return None;
    }
    if url
        .query_pairs()
        .any(|(key, value)| (key == "i" || key == "songId") && is_apple_numeric_id(&value))
    {
        return None;
    }
    if let Some(id) = APPLE_ALBUM_PATH_RE
        .captures(url.path())
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str())
        .filter(|id| is_apple_numeric_id(id))
    {
        return Some(AppleCollectionId {
            kind: AppleCollectionKind::Album,
            id: id.to_string(),
        });
    }
    APPLE_PLAYLIST_PATH_RE
        .captures(url.path())
        .and_then(|captures| captures.get(1))
        .map(|value| AppleCollectionId {
            kind: AppleCollectionKind::Playlist,
            id: value.as_str().to_string(),
        })
}

fn is_apple_numeric_id(value: &str) -> bool {
    let value = value.trim();
    (1..=20).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.bytes().any(|byte| byte != b'0')
}

fn parse_ttml_to_lrc(ttml: &str) -> String {
    let Ok(document) = roxmltree::Document::parse(ttml) else {
        return ttml.trim().to_string();
    };
    let mut lines = Vec::new();
    for paragraph in document.descendants().filter(|node| node.has_tag_name("p")) {
        let mut body = String::new();
        append_ttml_primary_text(paragraph, &mut body);
        let text = body.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        if let Some(begin) = ttml_begin(paragraph) {
            lines.push(format!("[{}]{}", format_lrc_timestamp(begin), text));
        } else {
            lines.push(text.to_string());
        }
    }
    if lines.is_empty() && !ttml.trim().is_empty() {
        return ttml.trim().to_string();
    }
    lines.join("\n")
}

fn append_ttml_primary_text(node: roxmltree::Node<'_, '_>, output: &mut String) {
    if node
        .attribute("role")
        .is_some_and(|role| role.split_whitespace().any(is_ttml_secondary_role))
    {
        return;
    }
    if node.has_tag_name("br") {
        output.push(' ');
        return;
    }
    for child in node.children() {
        if child.is_text() {
            if let Some(text) = child.text() {
                output.push_str(text);
            }
        } else if child.is_element() {
            append_ttml_primary_text(child, output);
        }
    }
}

fn is_ttml_secondary_role(role: &str) -> bool {
    matches!(role, "x-translation" | "x-roman" | "x-bg")
}

fn ttml_begin<'a>(node: roxmltree::Node<'a, 'a>) -> Option<&'a str> {
    node.attribute("begin").or_else(|| {
        node.descendants()
            .skip(1)
            .find_map(|child| child.attribute("begin"))
    })
}

fn format_lrc_timestamp(value: &str) -> String {
    let millis = parse_ttml_time_to_millis(value);
    let minutes = millis / 60_000;
    let seconds = (millis % 60_000) / 1000;
    let centiseconds = (millis % 1000) / 10;
    format!("{minutes:02}:{seconds:02}.{centiseconds:02}")
}

fn parse_ttml_time_to_millis(value: &str) -> u64 {
    let value = value.trim();
    if value.is_empty() {
        return 0;
    }
    for (suffix, multiplier) in [
        ("ms", 1.0),
        ("h", 3_600_000.0),
        ("m", 60_000.0),
        ("s", 1_000.0),
    ] {
        if let Some(number) = value.strip_suffix(suffix)
            && let Ok(number) = number.trim().parse::<f64>()
            && number.is_finite()
            && number >= 0.0
        {
            return (number * multiplier).round() as u64;
        }
    }
    let parts = value.split(':').collect::<Vec<_>>();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [seconds] => (0, 0, *seconds),
        [minutes, seconds] => (0, parse_u64_or_zero(minutes), *seconds),
        [hours, minutes, seconds] => (
            parse_u64_or_zero(hours),
            parse_u64_or_zero(minutes),
            *seconds,
        ),
        _ => return 0,
    };
    let seconds = seconds.parse::<f64>().unwrap_or(0.0);
    let total = (hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds) * 1000.0;
    total.max(0.0).round() as u64
}

fn parse_u64_or_zero(value: &str) -> u64 {
    value.parse().unwrap_or(0)
}

fn format_artwork_url(artwork: Option<&AppleMusicArtwork>, size: usize) -> String {
    artwork
        .map(|artwork| {
            artwork
                .url
                .replace("{w}", &size.to_string())
                .replace("{h}", &size.to_string())
        })
        .unwrap_or_default()
}

fn apple_track_url(id: &str) -> String {
    format!("https://music.apple.com/song/{id}")
}

fn apple_storefront() -> String {
    SETTINGS
        .music
        .applemusic
        .storefront
        .trim()
        .to_string()
        .if_empty("us")
}

fn apple_language() -> String {
    SETTINGS
        .music
        .applemusic
        .language
        .trim()
        .to_string()
        .if_empty("en-US")
}

fn apple_timeout() -> Duration {
    Duration::from_secs(SETTINGS.music.applemusic.timeout.max(1))
}

fn apple_wrapper_io_timeout() -> Duration {
    APPLE_WRAPPER_IO_TIMEOUT.max(apple_timeout())
}

fn ensure_apple_enabled() -> Result<(), BotError> {
    if SETTINGS.music.applemusic.enabled {
        Ok(())
    } else {
        Err(BotError::Custom("Apple Music provider 未启用".to_string()))
    }
}

fn validate_apple_download_environment() -> Result<(), BotError> {
    let config = &SETTINGS.music.applemusic;
    let has_widevine_override =
        !config.wv_client_id.trim().is_empty() || !config.wv_private_key.trim().is_empty();
    if has_widevine_override
        && (config.wv_client_id.trim().is_empty() || config.wv_private_key.trim().is_empty())
    {
        return Err(BotError::Custom(
            "Apple Music Widevine 覆盖配置需要同时填写 music.applemusic.wv_client_id 和 music.applemusic.wv_private_key"
                .to_string(),
        ));
    }
    Ok(())
}

fn apple_download_headers() -> HashMap<String, String> {
    [("User-Agent".to_string(), APPLE_MUSIC_UA.to_string())]
        .into_iter()
        .collect()
}

fn normalize_media_user_token(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    raw.split(';')
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key.trim() == "media-user-token").then(|| value.trim().to_string())
        })
        .unwrap_or_else(|| raw.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppleMusicQuality {
    Standard,
    High,
    Lossless,
    HiRes,
}

impl AppleMusicQuality {
    fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::High => "high",
            Self::Lossless => "lossless",
            Self::HiRes => "hires",
        }
    }
}

fn quality_from_str(value: &str) -> AppleMusicQuality {
    match value.trim().to_ascii_lowercase().as_str() {
        "standard" | "std" | "128" => AppleMusicQuality::Standard,
        "lossless" | "alac" | "无损" => AppleMusicQuality::Lossless,
        "hires" | "hi-res" | "hi_res" | "高解析" => AppleMusicQuality::HiRes,
        _ => AppleMusicQuality::High,
    }
}

fn apple_wrapper_host_opt() -> Option<String> {
    if APPLE_WRAPPER_ENDPOINTS.len() > 1 {
        return Some("pool".to_string());
    }
    APPLE_WRAPPER_ENDPOINTS
        .first()
        .map(|endpoint| endpoint.spec.clone())
}

#[derive(Clone, Debug)]
struct EnhancedHlsVariant {
    codecs: String,
    audio: String,
    avg_bw: i64,
    uri: String,
    sample_rate: i64,
    bit_depth: i64,
}

impl EnhancedHlsVariant {
    fn is_alac(&self) -> bool {
        self.codecs == "alac"
    }

    fn is_aac(&self) -> bool {
        self.codecs.starts_with("mp4a.40")
    }
}

#[derive(Clone, Debug)]
struct EnhancedHlsMedia {
    mp4_url: String,
    parts: Vec<EnhancedHlsPart>,
    seg_keys: Vec<String>,
    duration_seconds: f64,
}

#[derive(Clone, Debug)]
struct EnhancedHlsPart {
    url: String,
    range: Option<HlsByteRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HlsByteRange {
    offset: u64,
    length: u64,
}

fn parse_enhanced_hls_master(content: &str) -> Result<Vec<EnhancedHlsVariant>, BotError> {
    let lines = content.lines().map(str::trim).collect::<Vec<_>>();
    let mut variants = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(captures) = ENHANCED_STREAM_INF_RE.captures(lines[i]) else {
            i += 1;
            continue;
        };
        let attrs = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
        let mut variant = EnhancedHlsVariant {
            codecs: ENHANCED_CODEC_RE
                .captures(attrs)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
            audio: ENHANCED_AUDIO_RE
                .captures(attrs)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
            avg_bw: ENHANCED_AVG_BW_RE
                .captures(attrs)
                .or_else(|| ENHANCED_BW_RE.captures(attrs))
                .and_then(|c| c.get(if c.len() > 2 { 2 } else { 1 }))
                .and_then(|m| m.as_str().parse::<i64>().ok())
                .unwrap_or_default(),
            uri: String::new(),
            sample_rate: 0,
            bit_depth: 0,
        };
        let (sample_rate, bit_depth) = parse_alac_group_details(&variant.audio);
        variant.sample_rate = sample_rate;
        variant.bit_depth = bit_depth;
        i += 1;
        while i < lines.len() {
            let line = lines[i];
            i += 1;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            variant.uri = line.to_string();
            break;
        }
        if !variant.uri.is_empty() {
            variants.push(variant);
        }
    }
    if variants.is_empty() {
        return Err(BotError::Custom(
            "Apple Music enhancedHls 没有 stream variant".to_string(),
        ));
    }
    variants.sort_by_key(|variant| std::cmp::Reverse(variant.avg_bw));
    Ok(variants)
}

fn parse_alac_group_details(group: &str) -> (i64, i64) {
    if !group.contains("alac") {
        return (0, 0);
    }
    let parts = group.split('-').collect::<Vec<_>>();
    if parts.len() < 2 {
        return (0, 0);
    }
    let bit_depth = parts
        .last()
        .and_then(|part| part.parse::<i64>().ok())
        .unwrap_or_default();
    let sample_rate = parts
        .get(parts.len().saturating_sub(2))
        .and_then(|part| part.parse::<i64>().ok())
        .unwrap_or_default();
    (sample_rate, bit_depth)
}

fn select_variant_for_quality(
    variants: &[EnhancedHlsVariant],
    quality: AppleMusicQuality,
) -> Option<EnhancedHlsVariant> {
    // Apple distinguishes Lossless (up to 24-bit/48 kHz) from Hi-Res
    // Lossless (>48 kHz). Bit depth alone does not make 24-bit/44.1 kHz Hi-Res.
    // https://itunespartner.apple.com/music/support/5215-digital-packaging-music
    let selected = match quality {
        AppleMusicQuality::HiRes => variants
            .iter()
            .filter(|v| v.is_alac() && v.sample_rate > 48_000)
            .max_by_key(|v| (v.sample_rate, v.bit_depth, v.avg_bw))
            .cloned()
            .or_else(|| select_variant_for_quality(variants, AppleMusicQuality::Lossless)),
        AppleMusicQuality::Lossless => variants
            .iter()
            .filter(|v| v.is_alac() && (1..=48_000).contains(&v.sample_rate))
            .max_by_key(|v| (v.sample_rate, v.bit_depth, v.avg_bw))
            .cloned()
            .or_else(|| {
                // Keep an ALAC source when no standard Lossless variant is
                // offered. Prefer the nearest higher rate to avoid an
                // unnecessary 192 kHz download on a Lossless connection.
                variants
                    .iter()
                    .filter(|v| v.is_alac())
                    .min_by_key(|v| {
                        (
                            v.sample_rate <= 0,
                            v.sample_rate,
                            std::cmp::Reverse(v.bit_depth),
                            std::cmp::Reverse(v.avg_bw),
                        )
                    })
                    .cloned()
            }),
        AppleMusicQuality::High => best_aac_variant(variants, 256_000),
        AppleMusicQuality::Standard => best_aac_variant(variants, 128_000),
    };
    if let Some(variant) = &selected {
        let fallback = match quality {
            AppleMusicQuality::Lossless if variant.sample_rate > 48_000 => Some("lossless"),
            AppleMusicQuality::HiRes if variant.sample_rate <= 48_000 => Some("hires"),
            _ => None,
        };
        if let Some(requested_quality) = fallback {
            tracing::info!(
                requested_quality,
                actual_sample_rate = variant.sample_rate,
                actual_bit_depth = variant.bit_depth,
                "Apple Music ALAC quality fallback; source metadata is preserved"
            );
        }
    }
    selected
}

fn best_aac_variant(variants: &[EnhancedHlsVariant], target: i64) -> Option<EnhancedHlsVariant> {
    variants
        .iter()
        .filter(|v| v.is_aac())
        .min_by_key(|v| (v.avg_bw - target).abs())
        .cloned()
}

fn parse_enhanced_hls_media(media_url: &str, content: &str) -> Result<EnhancedHlsMedia, BotError> {
    let mut current_key = String::new();
    let mut mp4_name = String::new();
    let mut parts = Vec::new();
    let mut seg_keys = Vec::new();
    let mut duration_seconds = 0.0;
    let mut pending_range = None;
    let mut previous_range_end = None;
    for line in content.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line
            .strip_prefix("#EXTINF:")
            .and_then(|rest| rest.split(',').next())
            .and_then(|value| value.parse::<f64>().ok())
        {
            duration_seconds += value;
            continue;
        }
        if line.starts_with("#EXT-X-KEY") {
            if let Some(key) = MEDIA_KEY_RE
                .captures(line)
                .and_then(|captures| captures.get(1))
                .map(|m| m.as_str().to_string())
            {
                current_key = key;
            }
            continue;
        }
        if line.starts_with("#EXT-X-MAP") {
            if let Some(name) = MEDIA_MAP_RE
                .captures(line)
                .and_then(|captures| captures.get(1))
                .map(|m| m.as_str().to_string())
            {
                mp4_name = name.clone();
                let range = hls_attr_value(line, "BYTERANGE")
                    .and_then(|value| parse_hls_byte_range(value, previous_range_end));
                previous_range_end = range.map(HlsByteRange::end).or(previous_range_end);
                parts.push(EnhancedHlsPart {
                    url: absolute_hls_url(media_url, &name),
                    range,
                });
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_range = parse_hls_byte_range(value, previous_range_end);
            if let Some(range) = pending_range {
                previous_range_end = Some(range.end());
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if mp4_name.is_empty() {
            mp4_name = line.to_string();
        }
        let range = pending_range.take();
        previous_range_end = range.map(HlsByteRange::end).or(previous_range_end);
        parts.push(EnhancedHlsPart {
            url: absolute_hls_url(media_url, line),
            range,
        });
        seg_keys.push(current_key.clone());
    }
    if mp4_name.is_empty() {
        return Err(BotError::Custom(
            "Apple Music media playlist 没有 mp4 segment".to_string(),
        ));
    }
    if seg_keys.is_empty() {
        return Err(BotError::Custom(
            "Apple Music media playlist 没有 segment key".to_string(),
        ));
    }
    Ok(EnhancedHlsMedia {
        mp4_url: absolute_hls_url(media_url, &mp4_name),
        parts,
        seg_keys,
        duration_seconds,
    })
}

impl HlsByteRange {
    fn end(self) -> u64 {
        self.offset.saturating_add(self.length)
    }
}

fn hls_attr_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(&format!("{key}="))? + key.len() + 1;
    let value = &line[start..];
    if let Some(value) = value.strip_prefix('"') {
        return value.split('"').next();
    }
    value.split(',').next()
}

fn parse_hls_byte_range(value: &str, previous_end: Option<u64>) -> Option<HlsByteRange> {
    let value = value.trim().trim_matches('"');
    let (length, offset) = value
        .split_once('@')
        .map(|(length, offset)| (length, offset.parse().ok()))
        .unwrap_or((value, previous_end));
    let length = length.parse().ok()?;
    let offset = offset?;
    Some(HlsByteRange { offset, length })
}

fn absolute_hls_url(base_url: &str, value: &str) -> String {
    if value.starts_with("http://") || value.starts_with("https://") {
        return value.to_string();
    }
    base_url
        .rsplit_once('/')
        .map(|(base, _)| format!("{base}/{value}"))
        .unwrap_or_else(|| value.to_string())
}

fn is_m3u8_url(value: &str) -> bool {
    let without_fragment = value.split_once('#').map(|(path, _)| path).unwrap_or(value);
    let path = without_fragment
        .split_once('?')
        .map(|(path, _)| path)
        .unwrap_or(without_fragment);
    path.ends_with(".m3u8")
}

#[derive(Clone, Copy, Debug)]
struct Mp4Box {
    start: usize,
    header_size: usize,
    size: usize,
    typ: [u8; 4],
}

impl Mp4Box {
    fn payload_start(self) -> usize {
        self.start + self.header_size
    }

    fn end(self) -> usize {
        self.start + self.size
    }
}

fn read_mp4_box(data: &[u8], start: usize) -> Result<Option<Mp4Box>, BotError> {
    if start >= data.len() {
        return Ok(None);
    }
    if start + 8 > data.len() {
        return Err(BotError::Custom("MP4 box header truncated".to_string()));
    }
    let mut size = u32::from_be_bytes([
        data[start],
        data[start + 1],
        data[start + 2],
        data[start + 3],
    ]) as usize;
    let mut header_size = 8;
    let typ = [
        data[start + 4],
        data[start + 5],
        data[start + 6],
        data[start + 7],
    ];
    if size == 1 {
        if start + 16 > data.len() {
            return Err(BotError::Custom(
                "MP4 largesize header truncated".to_string(),
            ));
        }
        size = u64::from_be_bytes([
            data[start + 8],
            data[start + 9],
            data[start + 10],
            data[start + 11],
            data[start + 12],
            data[start + 13],
            data[start + 14],
            data[start + 15],
        ]) as usize;
        header_size = 16;
    } else if size == 0 {
        size = data.len() - start;
    }
    if size < header_size || start + size > data.len() {
        return Err(BotError::Custom(format!(
            "MP4 box {} size invalid",
            String::from_utf8_lossy(&typ)
        )));
    }
    Ok(Some(Mp4Box {
        start,
        header_size,
        size,
        typ,
    }))
}

fn find_child_box(data: &[u8], start: usize, end: usize, typ: &[u8; 4]) -> Option<Mp4Box> {
    let mut pos = start;
    while pos + 8 <= end {
        let Ok(Some(box_info)) = read_mp4_box(data, pos) else {
            return None;
        };
        if &box_info.typ == typ {
            return Some(box_info);
        }
        pos = box_info.end();
    }
    None
}

fn find_box_recursive(data: &[u8], start: usize, end: usize, typ: &[u8; 4]) -> Option<Mp4Box> {
    let mut pos = start;
    while pos + 8 <= end {
        let Ok(Some(box_info)) = read_mp4_box(data, pos) else {
            return None;
        };
        if &box_info.typ == typ {
            return Some(box_info);
        }
        if let Some(child_start) = container_child_start(box_info)
            && let Some(found) = find_box_recursive(data, child_start, box_info.end(), typ)
        {
            return Some(found);
        }
        pos = box_info.end();
    }
    None
}

fn container_child_start(box_info: Mp4Box) -> Option<usize> {
    let payload_start = box_info.payload_start();
    match &box_info.typ {
        b"stsd" => Some(payload_start + 8),
        b"enca" | b"mp4a" | b"alac" => Some(payload_start + 28),
        b"encv" => Some(payload_start + 78),
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"sinf" | b"schi" | b"moof" | b"traf" => {
            Some(payload_start)
        }
        _ => None,
    }
}

fn parse_tenc_from_init(data: &[u8]) -> Result<TencBox, BotError> {
    let tenc_box = find_box_recursive(data, 0, data.len(), b"tenc")
        .ok_or_else(|| BotError::Custom("Apple Music fMP4 没有 tenc box".to_string()))?;
    parse_tenc(&data[tenc_box.payload_start()..tenc_box.end()])
        .map_err(|e| BotError::Custom(format!("Apple Music tenc 解析失败：{e}")))
}

#[derive(Clone, Copy, Debug, Default)]
struct FragmentDefaults {
    default_sample_size: u32,
    default_sample_duration: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct TrunSampleInfo {
    size: u32,
    duration: u32,
}

#[derive(Clone, Debug, Default)]
struct ParsedTraf {
    runs: Vec<ParsedTrun>,
    senc: Option<SencBox>,
}

#[derive(Clone, Debug, Default)]
struct ParsedTrun {
    data_offset: Option<i32>,
    samples: Vec<TrunSampleInfo>,
    senc_sample_offset: usize,
}

fn parse_tfhd_defaults(body: &[u8]) -> Result<FragmentDefaults, BotError> {
    if body.len() < 8 {
        return Err(BotError::Custom("MP4 tfhd truncated".to_string()));
    }
    let flags = u32::from_be_bytes([0, body[1], body[2], body[3]]);
    let mut off = 8usize;
    if flags & 0x000001 != 0 {
        off += 8;
    }
    if flags & 0x000002 != 0 {
        off += 4;
    }
    let default_sample_duration = if flags & 0x000008 != 0 {
        if off + 4 > body.len() {
            return Err(BotError::Custom(
                "MP4 tfhd default duration truncated".to_string(),
            ));
        }
        let value = u32::from_be_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]);
        off += 4;
        value
    } else {
        0
    };
    let default_sample_size = if flags & 0x000010 != 0 {
        if off + 4 > body.len() {
            return Err(BotError::Custom(
                "MP4 tfhd default size truncated".to_string(),
            ));
        }
        u32::from_be_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]])
    } else {
        0
    };
    Ok(FragmentDefaults {
        default_sample_size,
        default_sample_duration,
    })
}

// trun data_offset is signed and comes straight from downloaded bytes; a
// negative sum must bail instead of wrapping to a huge usize (release builds
// have overflow checks off and panic = "abort").
fn trun_run_start(
    moof_start: usize,
    data_offset: Option<i32>,
    fallback: usize,
) -> Result<usize, BotError> {
    let Some(offset) = data_offset else {
        return Ok(fallback);
    };
    usize::try_from(moof_start as i64 + i64::from(offset))
        .map_err(|_| BotError::Custom("MP4 trun data_offset 非法".to_string()))
}

fn checked_sample_end(pos: usize, size: u32, limit: usize) -> Result<usize, BotError> {
    pos.checked_add(size as usize)
        .filter(|end| *end <= limit)
        .ok_or_else(|| BotError::Custom("MP4 sample 超出文件范围".to_string()))
}

fn parse_trun_samples(
    body: &[u8],
    defaults: FragmentDefaults,
) -> Result<(Option<i32>, Vec<TrunSampleInfo>), BotError> {
    if body.len() < 8 {
        return Err(BotError::Custom("MP4 trun truncated".to_string()));
    }
    let flags = u32::from_be_bytes([0, body[1], body[2], body[3]]);
    let sample_count = u32::from_be_bytes([body[4], body[5], body[6], body[7]]) as usize;
    let mut off = 8usize;
    let data_offset = if flags & 0x000001 != 0 {
        if off + 4 > body.len() {
            return Err(BotError::Custom(
                "MP4 trun data_offset truncated".to_string(),
            ));
        }
        let value = i32::from_be_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]);
        off += 4;
        Some(value)
    } else {
        None
    };
    if flags & 0x000004 != 0 {
        off += 4;
    }
    let mut samples = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let duration = if flags & 0x000100 != 0 {
            if off + 4 > body.len() {
                return Err(BotError::Custom(
                    "MP4 trun sample duration truncated".to_string(),
                ));
            }
            let value =
                u32::from_be_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]);
            off += 4;
            value
        } else {
            defaults.default_sample_duration
        };
        let size = if flags & 0x000200 != 0 {
            if off + 4 > body.len() {
                return Err(BotError::Custom(
                    "MP4 trun sample size truncated".to_string(),
                ));
            }
            let value =
                u32::from_be_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]);
            off += 4;
            value
        } else {
            defaults.default_sample_size
        };
        if flags & 0x000400 != 0 {
            off += 4;
        }
        if flags & 0x000800 != 0 {
            off += 4;
        }
        if size == 0 {
            return Err(BotError::Custom("MP4 trun sample size 缺失".to_string()));
        }
        samples.push(TrunSampleInfo { size, duration });
    }
    Ok((data_offset, samples))
}

fn parse_traf(data: &[u8], traf: Mp4Box, tenc: &TencBox) -> Result<ParsedTraf, BotError> {
    let tfhd = find_child_box(data, traf.payload_start(), traf.end(), b"tfhd")
        .ok_or_else(|| BotError::Custom("MP4 traf 缺少 tfhd".to_string()))?;
    let defaults = parse_tfhd_defaults(&data[tfhd.payload_start()..tfhd.end()])?;
    let mut runs = Vec::new();
    let mut senc_sample_offset = 0usize;
    let mut child_pos = traf.payload_start();
    while let Some(child) = read_mp4_box(data, child_pos)? {
        if child.end() > traf.end() {
            break;
        }
        if &child.typ == b"trun" {
            let (data_offset, samples) =
                parse_trun_samples(&data[child.payload_start()..child.end()], defaults)?;
            let sample_count = samples.len();
            runs.push(ParsedTrun {
                data_offset,
                samples,
                senc_sample_offset,
            });
            senc_sample_offset += sample_count;
        }
        child_pos = child.end();
    }
    if runs.is_empty() {
        return Err(BotError::Custom("MP4 traf 缺少 trun".to_string()));
    }
    let senc = find_child_box(data, traf.payload_start(), traf.end(), b"senc")
        .map(|senc| {
            parse_senc(
                &data[senc.payload_start()..senc.end()],
                tenc.default_per_sample_iv_size,
            )
            .map_err(|e| BotError::Custom(format!("Apple Music senc 解析失败：{e}")))
        })
        .transpose()?;
    Ok(ParsedTraf { runs, senc })
}

async fn decrypt_fmp4_with_wrapper(
    endpoint: &AppleWrapperEndpoint,
    track_id: &str,
    mut data: Vec<u8>,
    seg_keys: &[String],
) -> Result<AppleWrapperDecryptOutput, BotError> {
    let tenc = parse_tenc_from_init(&data)?;
    let addr = endpoint.decrypt_addr.clone();
    let commands = collect_wrapper_decrypt_commands(&data, track_id, seg_keys, &tenc)?;

    let _track_permit = APPLE_WRAPPER_TRACK_DECRYPT_SEMAPHORE
        .acquire()
        .await
        .map_err(|_| BotError::Custom("Apple Music wrapper track semaphore closed".into()))?;
    let _endpoint_permit = if let Some(index) = endpoint.index {
        Some(
            APPLE_WRAPPER_ENDPOINT_DECRYPT_SEMAPHORES
                .get(index)
                .ok_or_else(|| {
                    BotError::Custom("Apple Music wrapper endpoint semaphore missing".into())
                })?
                .acquire()
                .await
                .map_err(|_| {
                    BotError::Custom("Apple Music wrapper endpoint semaphore closed".into())
                })?,
        )
    } else {
        None
    };
    let started = Instant::now();
    run_wrapper_decrypt_session(&addr, commands, &mut data).await?;
    let decrypt_elapsed = started.elapsed();

    // The remux (box re-parse + ALAC bit scan) is seconds of CPU work for a
    // lossless track; keep it off the async worker threads.
    let audio = tokio::task::spawn_blocking(move || remux_decrypted_fmp4_to_progressive(&data))
        .await
        .map_err(|e| BotError::Custom(format!("Apple Music remux 任务失败：{e}")))??;

    Ok(AppleWrapperDecryptOutput {
        audio,
        decrypt_elapsed,
    })
}

#[derive(Clone)]
enum AppleWrapperDecryptCommand {
    Key { adam: String, uri: String },
    Job(AppleWrapperDecryptJob),
}

#[derive(Clone)]
struct AppleWrapperDecryptJob {
    payload: Vec<u8>,
    ranges: Vec<Range<usize>>,
}

fn collect_wrapper_decrypt_commands(
    data: &[u8],
    track_id: &str,
    seg_keys: &[String],
    tenc: &TencBox,
) -> Result<Vec<AppleWrapperDecryptCommand>, BotError> {
    let mut commands = Vec::new();
    let mut pos = 0usize;
    let mut fragment_index = 0usize;
    while let Some(box_info) = read_mp4_box(data, pos)? {
        if &box_info.typ != b"moof" {
            pos = box_info.end();
            continue;
        }
        let moof = box_info;
        let mdat = read_mp4_box(data, moof.end())?
            .filter(|next| &next.typ == b"mdat")
            .ok_or_else(|| BotError::Custom("MP4 moof 后缺少 mdat".to_string()))?;
        let key_uri = seg_keys
            .get(fragment_index)
            .ok_or_else(|| BotError::Custom("Apple Music segment key 数量不足".to_string()))?;
        let adam = if key_uri == APPLE_PREFETCH_KEY_URI {
            "0"
        } else {
            track_id
        };
        commands.push(AppleWrapperDecryptCommand::Key {
            adam: adam.to_string(),
            uri: key_uri.to_string(),
        });

        let mut traf_pos = moof.payload_start();
        while let Some(traf) = read_mp4_box(data, traf_pos)? {
            if traf.end() > moof.end() {
                break;
            }
            if &traf.typ == b"traf" {
                let parsed = parse_traf(data, traf, tenc)?;
                let mut next_sample_start = mdat.payload_start();
                for run in &parsed.runs {
                    let start = trun_run_start(moof.start, run.data_offset, next_sample_start)?;
                    next_sample_start = collect_wrapper_decrypt_sample_jobs(
                        &mut commands,
                        data,
                        start,
                        mdat.end(),
                        &run.samples,
                        parsed.senc.as_ref(),
                        run.senc_sample_offset,
                        tenc,
                    )?;
                }
            }
            traf_pos = traf.end();
        }

        fragment_index += 1;
        pos = mdat.end();
    }
    Ok(commands)
}

#[derive(Clone)]
struct AppleWrapperDecryptFragmentGroup {
    adam: String,
    uri: String,
    jobs: Vec<AppleWrapperDecryptJob>,
}

fn group_wrapper_commands_by_fragment(
    commands: Vec<AppleWrapperDecryptCommand>,
) -> Result<Vec<AppleWrapperDecryptFragmentGroup>, BotError> {
    let mut groups = Vec::new();
    let mut current: Option<AppleWrapperDecryptFragmentGroup> = None;
    for command in commands {
        match command {
            AppleWrapperDecryptCommand::Key { adam, uri } => {
                if let Some(group) = current.take() {
                    groups.push(group);
                }
                current = Some(AppleWrapperDecryptFragmentGroup {
                    adam,
                    uri,
                    jobs: Vec::new(),
                });
            }
            AppleWrapperDecryptCommand::Job(job) => {
                let group = current.as_mut().ok_or_else(|| {
                    BotError::Custom("Apple Music wrapper fragment 缺少 key".to_string())
                })?;
                group.jobs.push(job);
            }
        }
    }
    if let Some(group) = current {
        groups.push(group);
    }
    Ok(groups)
}

async fn run_wrapper_decrypt_session(
    addr: &str,
    commands: Vec<AppleWrapperDecryptCommand>,
    data: &mut [u8],
) -> Result<(), BotError> {
    let groups = group_wrapper_commands_by_fragment(commands)?;
    let _global_permit = APPLE_WRAPPER_DECRYPT_SEMAPHORE
        .acquire()
        .await
        .map_err(|_| BotError::Custom("Apple Music wrapper decrypt semaphore closed".into()))?;
    let stream = tokio::time::timeout(apple_timeout(), tokio::net::TcpStream::connect(addr))
        .await
        .map_err(|_| BotError::Custom(format!("Apple Music wrapper decrypt 连接超时：{addr}")))??;
    stream.set_nodelay(true)?;
    let mut stream = stream;

    let mut decrypted = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        if index > 0 {
            wrapper_write_all(&mut stream, &[0, 0, 0, 0], "fragment decrypt switch key").await?;
        }
        write_len_prefixed(&mut stream, &group.adam).await?;
        write_len_prefixed(&mut stream, &group.uri).await?;
        for job in group.jobs {
            let len = u32::try_from(job.payload.len()).map_err(|_| {
                BotError::Custom("Apple Music wrapper decrypt payload 过大".to_string())
            })?;
            wrapper_write_all(&mut stream, &len.to_le_bytes(), "fragment decrypt length").await?;
            wrapper_write_all(&mut stream, &job.payload, "fragment decrypt payload").await?;
            wrapper_flush(&mut stream, "fragment decrypt").await?;
            decrypted.resize(job.payload.len(), 0);
            wrapper_read_exact(&mut stream, &mut decrypted, "fragment decrypt result").await?;
            let mut src = 0usize;
            for range in job.ranges {
                let len = range.end - range.start;
                data[range].copy_from_slice(&decrypted[src..src + len]);
                src += len;
            }
        }
    }
    wrapper_write_all(&mut stream, &[0, 0, 0, 0, 0], "fragment decrypt finish").await?;
    wrapper_flush(&mut stream, "fragment decrypt finish").await?;
    Ok(())
}

#[derive(Clone, Debug)]
struct ProgressiveSample {
    // Range into the source fmp4 buffer; owning the bytes here would allocate
    // one Vec per sample and hold a second full copy of the track.
    range: Range<usize>,
    duration: u32,
    size: u32,
}

fn remux_decrypted_fmp4_to_progressive(data: &[u8]) -> Result<Vec<u8>, BotError> {
    let ftyp = find_child_box(data, 0, data.len(), b"ftyp")
        .ok_or_else(|| BotError::Custom("MP4 缺少 ftyp".to_string()))?;
    let moov = find_child_box(data, 0, data.len(), b"moov")
        .ok_or_else(|| BotError::Custom("MP4 缺少 moov".to_string()))?;
    let tenc = parse_tenc_from_init(data)?;
    let samples = collect_fragment_samples(data, &tenc)?;
    if samples.is_empty() {
        return Err(BotError::Custom("MP4 fragments 没有 samples".to_string()));
    }

    let movie_timescale = find_box_recursive(data, moov.payload_start(), moov.end(), b"mvhd")
        .and_then(|mvhd| parse_mvhd_timescale(&data[mvhd.payload_start()..mvhd.end()]))
        .unwrap_or(1000);
    let media_timescale = find_box_recursive(data, moov.payload_start(), moov.end(), b"mdhd")
        .and_then(|mdhd| parse_mdhd_timescale(&data[mdhd.payload_start()..mdhd.end()]))
        .unwrap_or(movie_timescale);
    let total_media_duration = samples
        .iter()
        .map(|sample| sample.duration as u64)
        .sum::<u64>();
    let total_movie_duration = if media_timescale > 0 {
        total_media_duration.saturating_mul(movie_timescale as u64) / media_timescale as u64
    } else {
        total_media_duration
    };

    let mut moov_bytes = rewrite_box_for_progressive(
        data,
        moov,
        &samples,
        total_media_duration,
        total_movie_duration,
    )?;
    let ftyp_bytes = data[ftyp.start..ftyp.end()].to_vec();
    let mdat_payload_offset = ftyp_bytes.len() + moov_bytes.len() + 8;
    patch_first_stco_offset(&mut moov_bytes, mdat_payload_offset as u32)?;

    let mdat_payload_size = samples
        .iter()
        .map(|sample| sample.range.len())
        .sum::<usize>();
    let mut out = Vec::with_capacity(ftyp_bytes.len() + moov_bytes.len() + 8 + mdat_payload_size);
    out.extend_from_slice(&ftyp_bytes);
    out.extend_from_slice(&moov_bytes);
    let mdat_size = 8usize + mdat_payload_size;
    out.extend_from_slice(&(mdat_size as u32).to_be_bytes());
    out.extend_from_slice(b"mdat");
    for sample in &samples {
        out.extend_from_slice(&data[sample.range.clone()]);
    }
    fix_alac_packets_in_progressive(&mut out)?;
    Ok(out)
}

fn collect_fragment_samples(
    data: &[u8],
    tenc: &TencBox,
) -> Result<Vec<ProgressiveSample>, BotError> {
    let mut samples = Vec::new();
    let mut pos = 0usize;
    while let Some(box_info) = read_mp4_box(data, pos)? {
        if &box_info.typ != b"moof" {
            pos = box_info.end();
            continue;
        }
        let moof = box_info;
        let mdat = read_mp4_box(data, moof.end())?
            .filter(|next| &next.typ == b"mdat")
            .ok_or_else(|| BotError::Custom("MP4 moof 后缺少 mdat".to_string()))?;
        let mut traf_pos = moof.payload_start();
        while let Some(traf) = read_mp4_box(data, traf_pos)? {
            if traf.end() > moof.end() {
                break;
            }
            if &traf.typ == b"traf" {
                let parsed = parse_traf(data, traf, tenc)?;
                let mut next_sample_start = mdat.payload_start();
                for run in parsed.runs {
                    let mut sample_pos =
                        trun_run_start(moof.start, run.data_offset, next_sample_start)?;
                    for sample in run.samples {
                        let end = checked_sample_end(
                            sample_pos,
                            sample.size,
                            mdat.end().min(data.len()),
                        )?;
                        samples.push(ProgressiveSample {
                            range: sample_pos..end,
                            duration: sample.duration,
                            size: sample.size,
                        });
                        sample_pos = end;
                    }
                    next_sample_start = sample_pos;
                }
            }
            traf_pos = traf.end();
        }
        pos = mdat.end();
    }
    Ok(samples)
}

fn rewrite_box_for_progressive(
    data: &[u8],
    box_info: Mp4Box,
    samples: &[ProgressiveSample],
    total_media_duration: u64,
    total_movie_duration: u64,
) -> Result<Vec<u8>, BotError> {
    match &box_info.typ {
        b"moov" | b"trak" | b"mdia" | b"minf" => {
            let mut payload = Vec::new();
            let mut pos = box_info.payload_start();
            while let Some(child) = read_mp4_box(data, pos)? {
                if child.end() > box_info.end() {
                    break;
                }
                if (&child.typ == b"mvex" && &box_info.typ == b"moov")
                    || (&child.typ == b"edts" && &box_info.typ == b"trak")
                {
                    pos = child.end();
                    continue;
                }
                payload.extend_from_slice(&rewrite_box_for_progressive(
                    data,
                    child,
                    samples,
                    total_media_duration,
                    total_movie_duration,
                )?);
                pos = child.end();
            }
            Ok(build_box(&box_info.typ, payload))
        }
        b"stbl" => build_progressive_stbl(data, box_info, samples),
        b"mvhd" => {
            let mut payload = data[box_info.payload_start()..box_info.end()].to_vec();
            patch_mvhd_duration(&mut payload, total_movie_duration);
            Ok(build_box(b"mvhd", payload))
        }
        b"mdhd" => {
            let mut payload = data[box_info.payload_start()..box_info.end()].to_vec();
            patch_mdhd_duration(&mut payload, total_media_duration);
            Ok(build_box(b"mdhd", payload))
        }
        b"tkhd" => {
            let mut payload = data[box_info.payload_start()..box_info.end()].to_vec();
            patch_tkhd_duration(&mut payload, total_movie_duration);
            Ok(build_box(b"tkhd", payload))
        }
        _ => Ok(data[box_info.start..box_info.end()].to_vec()),
    }
}

fn build_progressive_stbl(
    data: &[u8],
    stbl: Mp4Box,
    samples: &[ProgressiveSample],
) -> Result<Vec<u8>, BotError> {
    let stsd = find_child_box(data, stbl.payload_start(), stbl.end(), b"stsd")
        .ok_or_else(|| BotError::Custom("MP4 stbl 缺少 stsd".to_string()))?;
    let mut payload = Vec::new();
    payload.extend_from_slice(&sanitize_stsd(data, stsd)?);
    payload.extend_from_slice(&build_stts(samples));
    payload.extend_from_slice(&build_stsc(samples.len() as u32));
    payload.extend_from_slice(&build_stsz(samples));
    payload.extend_from_slice(&build_stco(0));
    Ok(build_box(b"stbl", payload))
}

fn sanitize_stsd(data: &[u8], stsd: Mp4Box) -> Result<Vec<u8>, BotError> {
    let payload = &data[stsd.payload_start()..stsd.end()];
    if payload.len() < 16 {
        return Err(BotError::Custom("MP4 stsd truncated".to_string()));
    }
    let entry = read_mp4_box(data, stsd.payload_start() + 8)?
        .ok_or_else(|| BotError::Custom("MP4 stsd 缺少 sample entry".to_string()))?;
    let sanitized_entry = sanitize_sample_entry(data, entry)?;
    let mut out_payload = payload[..8].to_vec();
    out_payload[4..8].copy_from_slice(&1u32.to_be_bytes());
    out_payload.extend_from_slice(&sanitized_entry);
    Ok(build_box(b"stsd", out_payload))
}

fn sanitize_sample_entry(data: &[u8], entry: Mp4Box) -> Result<Vec<u8>, BotError> {
    let mut typ = entry.typ;
    if &entry.typ == b"enca" || &entry.typ == b"encv" {
        if let Some(frma) = find_box_recursive(data, entry.payload_start(), entry.end(), b"frma") {
            let body = &data[frma.payload_start()..frma.end()];
            if body.len() >= 4 {
                typ.copy_from_slice(&body[..4]);
            } else if &entry.typ == b"enca" {
                typ.copy_from_slice(b"alac");
            }
        } else if &entry.typ == b"enca" {
            typ.copy_from_slice(b"alac");
        }
    }

    let fixed = match &entry.typ {
        b"enca" | b"mp4a" | b"alac" => 28,
        b"encv" => 78,
        _ => entry.end().saturating_sub(entry.payload_start()),
    };
    let fixed_end = (entry.payload_start() + fixed).min(entry.end());
    let mut payload = data[entry.payload_start()..fixed_end].to_vec();
    let mut pos = fixed_end;
    while let Some(child) = read_mp4_box(data, pos)? {
        if child.end() > entry.end() {
            break;
        }
        if &child.typ != b"sinf" {
            payload.extend_from_slice(&data[child.start..child.end()]);
        }
        pos = child.end();
    }
    Ok(build_box(&typ, payload))
}

fn build_stts(samples: &[ProgressiveSample]) -> Vec<u8> {
    let mut entries: Vec<(u32, u32)> = Vec::new();
    for sample in samples {
        if let Some((count, duration)) = entries.last_mut()
            && *duration == sample.duration
        {
            *count += 1;
            continue;
        }
        entries.push((1, sample.duration));
    }
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (count, duration) in entries {
        payload.extend_from_slice(&count.to_be_bytes());
        payload.extend_from_slice(&duration.to_be_bytes());
    }
    build_box(b"stts", payload)
}

fn build_stsc(sample_count: u32) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&1u32.to_be_bytes());
    payload.extend_from_slice(&1u32.to_be_bytes());
    payload.extend_from_slice(&sample_count.to_be_bytes());
    payload.extend_from_slice(&1u32.to_be_bytes());
    build_box(b"stsc", payload)
}

fn build_stsz(samples: &[ProgressiveSample]) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&(samples.len() as u32).to_be_bytes());
    for sample in samples {
        payload.extend_from_slice(&sample.size.to_be_bytes());
    }
    build_box(b"stsz", payload)
}

fn build_stco(offset: u32) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&1u32.to_be_bytes());
    payload.extend_from_slice(&offset.to_be_bytes());
    build_box(b"stco", payload)
}

fn build_box(typ: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(&payload);
    out
}

fn parse_mvhd_timescale(payload: &[u8]) -> Option<u32> {
    match payload.first().copied()? {
        0 if payload.len() >= 20 => Some(u32::from_be_bytes([
            payload[12],
            payload[13],
            payload[14],
            payload[15],
        ])),
        _ if payload.len() >= 32 => Some(u32::from_be_bytes([
            payload[20],
            payload[21],
            payload[22],
            payload[23],
        ])),
        _ => None,
    }
}

fn parse_mdhd_timescale(payload: &[u8]) -> Option<u32> {
    parse_mvhd_timescale(payload)
}

fn patch_mvhd_duration(payload: &mut [u8], duration: u64) {
    match payload.first().copied() {
        Some(0) if payload.len() >= 20 => {
            payload[16..20].copy_from_slice(&(duration as u32).to_be_bytes());
        }
        Some(_) if payload.len() >= 32 => {
            payload[24..32].copy_from_slice(&duration.to_be_bytes());
        }
        _ => {}
    }
}

fn patch_mdhd_duration(payload: &mut [u8], duration: u64) {
    patch_mvhd_duration(payload, duration);
}

fn patch_tkhd_duration(payload: &mut [u8], duration: u64) {
    match payload.first().copied() {
        Some(0) if payload.len() >= 24 => {
            payload[20..24].copy_from_slice(&(duration as u32).to_be_bytes());
        }
        Some(_) if payload.len() >= 36 => {
            payload[28..36].copy_from_slice(&duration.to_be_bytes());
        }
        _ => {}
    }
}

fn patch_first_stco_offset(moov: &mut [u8], offset: u32) -> Result<(), BotError> {
    let stco = find_box_recursive(moov, 0, moov.len(), b"stco")
        .ok_or_else(|| BotError::Custom("progressive MP4 缺少 stco".to_string()))?;
    let patch = stco.payload_start() + 8;
    if patch + 4 > stco.end() {
        return Err(BotError::Custom(
            "progressive MP4 stco truncated".to_string(),
        ));
    }
    moov[patch..patch + 4].copy_from_slice(&offset.to_be_bytes());
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct AlacParams {
    max_samples_per_frame: u32,
    sample_size: u8,
    rice_history_mult: u8,
    rice_initial_history: u8,
    rice_limit: u8,
    channels: u8,
}

#[derive(Clone, Copy, Debug)]
struct AlacPacketLoc {
    offset: usize,
    size: usize,
}

#[derive(Debug)]
struct AlacBitReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> AlacBitReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn left(&self) -> usize {
        self.buf.len() * 8 - self.pos
    }

    fn read(&mut self, n: usize) -> Option<u32> {
        if self.pos + n > self.buf.len() * 8 {
            return None;
        }
        let mut value = 0u32;
        for _ in 0..n {
            value = (value << 1) | ((self.buf[self.pos >> 3] >> (7 - (self.pos & 7))) & 1) as u32;
            self.pos += 1;
        }
        Some(value)
    }

    fn show(&mut self, n: usize) -> Option<u32> {
        let save = self.pos;
        let value = self.read(n);
        self.pos = save;
        value
    }

    fn skip(&mut self, n: usize) -> bool {
        if self.pos + n > self.buf.len() * 8 {
            return false;
        }
        self.pos += n;
        true
    }

    fn read_signed(&mut self, n: usize) -> Option<i32> {
        let value = self.read(n)?;
        if value & (1 << (n - 1)) != 0 {
            Some(value as i32 - (1i32 << n))
        } else {
            Some(value as i32)
        }
    }

    fn unary09(&mut self) -> Option<u32> {
        let mut count = 0u32;
        while count < 9 {
            if self.read(1)? == 0 {
                return Some(count);
            }
            count += 1;
        }
        Some(9)
    }
}

fn fix_alac_packets_in_progressive(data: &mut [u8]) -> Result<(), BotError> {
    let Some(params) = find_alac_params(data)? else {
        return Ok(());
    };
    let locs = find_progressive_packet_locations(data)?;
    for loc in locs {
        if loc.offset + loc.size > data.len() {
            continue;
        }
        let packet = &data[loc.offset..loc.offset + loc.size];
        let body_end = alac_find_body_end_bit(packet, &params);
        if body_end < 0 || body_end as usize == loc.size * 8 {
            continue;
        }
        let mut reader = AlacBitReader::new(packet);
        if !reader.skip(body_end as usize) {
            continue;
        }
        if reader.left() >= 3 && reader.show(3) == Some(7) {
            continue;
        }
        alac_patch_in_place(data, loc.offset, loc.size, body_end as usize);
    }
    Ok(())
}

fn find_alac_params(data: &[u8]) -> Result<Option<AlacParams>, BotError> {
    let Some(stsd) = find_box_recursive(data, 0, data.len(), b"stsd") else {
        return Ok(None);
    };
    if stsd.payload_start() + 16 > stsd.end() {
        return Ok(None);
    }
    let Some(entry) = read_mp4_box(data, stsd.payload_start() + 8)? else {
        return Ok(None);
    };
    if &entry.typ != b"alac" {
        return Ok(None);
    }
    let child_start = entry.payload_start() + 28;
    let Some(config) = find_child_box(data, child_start, entry.end(), b"alac") else {
        return Ok(None);
    };
    let body = &data[config.payload_start()..config.end()];
    if body.len() < 24 {
        return Err(BotError::Custom("ALAC config too short".to_string()));
    }
    Ok(Some(AlacParams {
        max_samples_per_frame: u32::from_be_bytes([body[4], body[5], body[6], body[7]]),
        sample_size: body[9],
        rice_history_mult: body[10],
        rice_initial_history: body[11],
        rice_limit: body[12],
        channels: body[13],
    }))
}

fn find_progressive_packet_locations(data: &[u8]) -> Result<Vec<AlacPacketLoc>, BotError> {
    let stsz = find_box_recursive(data, 0, data.len(), b"stsz")
        .ok_or_else(|| BotError::Custom("progressive MP4 缺少 stsz".to_string()))?;
    let stsz_body = &data[stsz.payload_start()..stsz.end()];
    if stsz_body.len() < 12 {
        return Err(BotError::Custom(
            "progressive MP4 stsz truncated".to_string(),
        ));
    }
    let default_size = u32::from_be_bytes([stsz_body[4], stsz_body[5], stsz_body[6], stsz_body[7]]);
    let sample_count =
        u32::from_be_bytes([stsz_body[8], stsz_body[9], stsz_body[10], stsz_body[11]]) as usize;
    if sample_count > data.len() || (default_size == 0 && sample_count > (stsz_body.len() - 12) / 4)
    {
        return Err(BotError::Custom(
            "progressive MP4 sample count invalid".to_string(),
        ));
    }
    let mut sizes = Vec::with_capacity(sample_count);
    if default_size == 0 {
        let mut pos = 12usize;
        for _ in 0..sample_count {
            if pos + 4 > stsz_body.len() {
                return Err(BotError::Custom(
                    "progressive MP4 stsz sizes truncated".to_string(),
                ));
            }
            sizes.push(u32::from_be_bytes([
                stsz_body[pos],
                stsz_body[pos + 1],
                stsz_body[pos + 2],
                stsz_body[pos + 3],
            ]) as usize);
            pos += 4;
        }
    } else {
        sizes.resize(sample_count, default_size as usize);
    }

    let chunk_box = find_box_recursive(data, 0, data.len(), b"stco")
        .or_else(|| find_box_recursive(data, 0, data.len(), b"co64"))
        .ok_or_else(|| BotError::Custom("progressive MP4 缺少 chunk offsets".to_string()))?;
    let chunk_body = &data[chunk_box.payload_start()..chunk_box.end()];
    let chunk_width = if &chunk_box.typ == b"co64" { 8 } else { 4 };
    if chunk_body.len() < 8 {
        return Err(BotError::Custom(
            "progressive MP4 chunk offsets truncated".to_string(),
        ));
    }
    let chunk_count = u32::from_be_bytes(chunk_body[4..8].try_into().unwrap()) as usize;
    if chunk_count == 0 || chunk_count > (chunk_body.len() - 8) / chunk_width {
        return Err(BotError::Custom(
            "progressive MP4 chunk count invalid".to_string(),
        ));
    }
    let stsc = find_box_recursive(data, 0, data.len(), b"stsc")
        .ok_or_else(|| BotError::Custom("progressive MP4 缺少 stsc".to_string()))?;
    let stsc_body = &data[stsc.payload_start()..stsc.end()];
    if stsc_body.len() < 8 {
        return Err(BotError::Custom(
            "progressive MP4 stsc truncated".to_string(),
        ));
    }
    let run_count = u32::from_be_bytes(stsc_body[4..8].try_into().unwrap()) as usize;
    if run_count == 0 || run_count > (stsc_body.len() - 8) / 12 {
        return Err(BotError::Custom(
            "progressive MP4 stsc count invalid".to_string(),
        ));
    }
    let mut runs = Vec::with_capacity(run_count);
    for entry in stsc_body[8..8 + run_count * 12].chunks_exact(12) {
        let first = u32::from_be_bytes(entry[..4].try_into().unwrap()) as usize;
        let count = u32::from_be_bytes(entry[4..8].try_into().unwrap()) as usize;
        let description = u32::from_be_bytes(entry[8..12].try_into().unwrap());
        if first == 0
            || first > chunk_count
            || count == 0
            || description != 1
            || runs.last().is_some_and(|&(previous, _)| first <= previous)
        {
            return Err(BotError::Custom(
                "progressive MP4 stsc layout invalid".to_string(),
            ));
        }
        runs.push((first, count));
    }
    if runs[0].0 != 1 {
        return Err(BotError::Custom(
            "progressive MP4 stsc first chunk invalid".to_string(),
        ));
    }
    let mut locations = Vec::with_capacity(sample_count);
    let mut run = 0;
    for (index, chunk) in chunk_body[8..8 + chunk_count * chunk_width]
        .chunks_exact(chunk_width)
        .enumerate()
    {
        let index = index + 1;
        while run + 1 < runs.len() && runs[run + 1].0 <= index {
            run += 1;
        }
        let mut offset = if chunk_width == 8 {
            usize::try_from(u64::from_be_bytes(chunk.try_into().unwrap())).map_err(|_| {
                BotError::Custom("progressive MP4 chunk offset overflow".to_string())
            })?
        } else {
            u32::from_be_bytes(chunk.try_into().unwrap()) as usize
        };
        for _ in 0..runs[run].1 {
            let size = *sizes.get(locations.len()).ok_or_else(|| {
                BotError::Custom("progressive MP4 chunk sample count mismatch".to_string())
            })?;
            let end = offset
                .checked_add(size)
                .filter(|&end| end <= data.len())
                .ok_or_else(|| BotError::Custom("progressive MP4 packet truncated".to_string()))?;
            locations.push(AlacPacketLoc { offset, size });
            offset = end;
        }
    }
    if locations.len() != sample_count {
        return Err(BotError::Custom(
            "progressive MP4 sample count mismatch".to_string(),
        ));
    }
    Ok(locations)
}

fn alac_find_body_end_bit(packet: &[u8], params: &AlacParams) -> isize {
    let mut reader = AlacBitReader::new(packet);
    let mut channels_used = 0usize;
    let mut last_end = -1isize;
    while reader.left() >= 3 {
        let Some((channels, is_end)) = alac_scan_one_element(&mut reader, params) else {
            return -1;
        };
        if is_end {
            return reader.pos as isize;
        }
        last_end = reader.pos as isize;
        channels_used += channels;
        if channels_used >= params.channels as usize {
            return last_end;
        }
    }
    last_end
}

fn alac_scan_one_element(
    reader: &mut AlacBitReader<'_>,
    params: &AlacParams,
) -> Option<(usize, bool)> {
    let elem = reader.read(3)?;
    if elem == 7 {
        return Some((0, true));
    }
    if elem > 1 && elem != 3 {
        return None;
    }
    let channels = if elem == 1 { 2usize } else { 1usize };
    reader.skip(4).then_some(())?;
    reader.skip(12).then_some(())?;
    let has_size = reader.read(1)?;
    let extra_bits = (reader.read(2)? as usize) << 3;
    let bps = params.sample_size as isize - extra_bits as isize + channels as isize - 1;
    if !(1..=32).contains(&bps) {
        return None;
    }
    let is_compressed = reader.read(1)? == 0;
    let output_samples = if has_size != 0 {
        reader.read(32)?
    } else {
        params.max_samples_per_frame
    };
    if output_samples == 0 || output_samples > params.max_samples_per_frame {
        return None;
    }
    if is_compressed {
        reader.read(8)?;
        reader.read(8)?;
        let mut rhms = Vec::with_capacity(channels);
        for _ in 0..channels {
            reader.read(4)?;
            let lpc_quant = reader.read(4)?;
            let rhm = reader.read(3)?;
            let lpc_order = reader.read(5)?;
            if lpc_order >= params.max_samples_per_frame || lpc_quant == 0 {
                return None;
            }
            for _ in 0..lpc_order {
                reader.read_signed(16)?;
            }
            rhms.push(rhm);
        }
        if extra_bits != 0 {
            reader
                .skip(output_samples as usize * channels * extra_bits)
                .then_some(())?;
        }
        for rhm in rhms {
            let rhm_eff = (rhm * params.rice_history_mult as u32) / 4;
            alac_rice_decompress(
                reader,
                output_samples as usize,
                bps as usize,
                rhm_eff,
                params,
            )?;
        }
    } else {
        reader
            .skip(output_samples as usize * channels * params.sample_size as usize)
            .then_some(())?;
    }
    Some((channels, false))
}

fn alac_rice_decompress(
    reader: &mut AlacBitReader<'_>,
    nb_samples: usize,
    bps: usize,
    rhm_eff: u32,
    params: &AlacParams,
) -> Option<()> {
    let mut history = params.rice_initial_history as u32;
    let mut sign_mod = 0u32;
    let limit = params.rice_limit as usize;
    let mut i = 0usize;
    let mut iters = 0usize;
    while i < nb_samples {
        iters += 1;
        if iters > nb_samples * 4 + 100 || reader.left() == 0 {
            return None;
        }
        let mut k = alac_log2((history >> 9) + 3);
        if k > limit {
            k = limit;
        }
        let x = alac_decode_scalar(reader, k, bps)? + sign_mod;
        sign_mod = 0;
        if x > 0xffff {
            history = 0xffff;
        } else {
            history = history + x * rhm_eff - ((history * rhm_eff) >> 9);
        }
        if history < 128 && i + 1 < nb_samples {
            let mut k2 = 7usize
                .saturating_sub(alac_log2(history))
                .saturating_add(((history + 16) >> 6) as usize);
            if k2 > limit {
                k2 = limit;
            }
            let block_size = alac_decode_scalar(reader, k2, 16)?;
            if block_size > 0 {
                i += (block_size as usize).min(nb_samples - i - 1);
            }
            if block_size <= 0xffff {
                sign_mod = 1;
            }
            history = 0;
        }
        i += 1;
    }
    Some(())
}

fn alac_decode_scalar(reader: &mut AlacBitReader<'_>, k: usize, bps: usize) -> Option<u32> {
    let mut x = reader.unary09()?;
    if x > 8 {
        return reader.read(bps);
    }
    if k != 1 {
        let extra_bits = reader.show(k)?;
        x = (x << k) - x;
        if extra_bits > 1 {
            x += extra_bits - 1;
            reader.skip(k).then_some(())?;
        } else {
            reader.skip(k - 1).then_some(())?;
        }
    }
    Some(x)
}

fn alac_log2(mut value: u32) -> usize {
    let mut result = 0usize;
    while value > 1 {
        value >>= 1;
        result += 1;
    }
    result
}

fn alac_patch_in_place(data: &mut [u8], offset: usize, size: usize, body_end_bit: usize) -> bool {
    let total_bits = size * 8;
    if body_end_bit + 3 > total_bits || offset + size > data.len() {
        return false;
    }
    for i in 0..3 {
        let bit_pos = body_end_bit + i;
        let byte_index = offset + (bit_pos >> 3);
        let mask = 1u8 << (7 - (bit_pos & 7));
        data[byte_index] |= mask;
    }
    let pad_start = body_end_bit + 3;
    let mut byte_index = offset + (pad_start >> 3);
    let bit_in_byte = pad_start & 7;
    if bit_in_byte != 0 {
        let keep = 0xffu8 << (8 - bit_in_byte);
        data[byte_index] &= keep;
        byte_index += 1;
    }
    for byte in &mut data[byte_index..offset + size] {
        *byte = 0;
    }
    true
}

async fn write_len_prefixed<W>(stream: &mut W, value: &str) -> Result<(), BotError>
where
    W: AsyncWrite + Unpin,
{
    let bytes = value.as_bytes();
    if bytes.len() > u8::MAX as usize {
        return Err(BotError::Custom(
            "Apple Music wrapper string 过长".to_string(),
        ));
    }
    wrapper_write_all(stream, &[bytes.len() as u8], "length-prefixed header").await?;
    wrapper_write_all(stream, bytes, "length-prefixed payload").await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect_wrapper_decrypt_sample_jobs(
    commands: &mut Vec<AppleWrapperDecryptCommand>,
    data: &[u8],
    sample_start: usize,
    sample_end: usize,
    samples: &[TrunSampleInfo],
    senc: Option<&SencBox>,
    senc_sample_offset: usize,
    tenc: &TencBox,
) -> Result<usize, BotError> {
    let mut pos = sample_start;
    for (index, sample) in samples.iter().enumerate() {
        let end = checked_sample_end(pos, sample.size, sample_end.min(data.len()))?;
        let subsamples = senc
            .and_then(|senc| senc.samples.get(senc_sample_offset + index))
            .map(|sample| sample.subsamples.as_slice())
            .unwrap_or(&[]);
        collect_wrapper_decrypt_sample_job(
            commands,
            data,
            pos,
            sample.size as usize,
            subsamples,
            tenc,
        )?;
        pos = end;
    }
    Ok(pos)
}

fn collect_wrapper_decrypt_sample_job(
    commands: &mut Vec<AppleWrapperDecryptCommand>,
    data: &[u8],
    sample_start: usize,
    sample_size: usize,
    subsamples: &[SubsampleEntry],
    tenc: &TencBox,
) -> Result<(), BotError> {
    if subsamples.is_empty() {
        return collect_wrapper_decrypt_raw_job(commands, data, sample_start, sample_size, tenc);
    }
    let mut pos = 0usize;
    for subsample in subsamples {
        pos = pos
            .checked_add(subsample.bytes_of_clear_data as usize)
            .filter(|&offset| offset <= sample_size)
            .ok_or_else(|| BotError::Custom("MP4 subsample clear range 超出 sample".to_string()))?;
        let end = pos
            .checked_add(subsample.bytes_of_protected_data as usize)
            .filter(|&end| end <= sample_size)
            .ok_or_else(|| {
                BotError::Custom("MP4 subsample protected range 超出 sample".to_string())
            })?;
        collect_wrapper_decrypt_raw_job(commands, data, sample_start + pos, end - pos, tenc)?;
        pos = end;
    }
    Ok(())
}

fn collect_wrapper_decrypt_raw_job(
    commands: &mut Vec<AppleWrapperDecryptCommand>,
    data: &[u8],
    start: usize,
    size: usize,
    tenc: &TencBox,
) -> Result<(), BotError> {
    let decrypt_block_len = tenc.default_crypt_byte_block as usize * 16;
    let skip_block_len = tenc.default_skip_byte_block as usize * 16;
    if skip_block_len == 0 {
        let len = size & !0x0f;
        if len == 0 {
            return Ok(());
        }
        let end = start + len;
        if end > data.len() {
            return Err(BotError::Custom(
                "MP4 decrypt range 超出文件范围".to_string(),
            ));
        }
        commands.push(AppleWrapperDecryptCommand::Job(AppleWrapperDecryptJob {
            payload: data[start..end].to_vec(),
            ranges: std::iter::once(start..end).collect(),
        }));
        return Ok(());
    }

    if size < decrypt_block_len {
        return Ok(());
    }
    let mut ranges = Vec::new();
    let mut pos = 0usize;
    while size.saturating_sub(pos) >= decrypt_block_len {
        ranges.push(start + pos..start + pos + decrypt_block_len);
        pos += decrypt_block_len;
        if size.saturating_sub(pos) < skip_block_len {
            break;
        }
        pos += skip_block_len;
    }
    let total_len = ranges
        .iter()
        .map(|range| range.end - range.start)
        .sum::<usize>();
    if total_len == 0 {
        return Ok(());
    }
    let mut payload = Vec::with_capacity(total_len);
    for range in &ranges {
        if range.end > data.len() {
            return Err(BotError::Custom(
                "MP4 decrypt range 超出文件范围".to_string(),
            ));
        }
        payload.extend_from_slice(&data[range.clone()]);
    }
    commands.push(AppleWrapperDecryptCommand::Job(AppleWrapperDecryptJob {
        payload,
        ranges,
    }));
    Ok(())
}

async fn wrapper_write_all<W>(stream: &mut W, data: &[u8], context: &str) -> Result<(), BotError>
where
    W: AsyncWrite + Unpin,
{
    tokio::time::timeout(apple_wrapper_io_timeout(), stream.write_all(data))
        .await
        .map_err(|_| BotError::Custom(format!("Apple Music wrapper 写入超时：{context}")))??;
    Ok(())
}

async fn wrapper_flush<W>(stream: &mut W, context: &str) -> Result<(), BotError>
where
    W: AsyncWrite + Unpin,
{
    tokio::time::timeout(apple_wrapper_io_timeout(), stream.flush())
        .await
        .map_err(|_| BotError::Custom(format!("Apple Music wrapper flush 超时：{context}")))??;
    Ok(())
}

async fn wrapper_read_exact<S>(
    stream: &mut S,
    data: &mut [u8],
    context: &str,
) -> Result<(), BotError>
where
    S: AsyncRead + Unpin,
{
    tokio::time::timeout(apple_wrapper_io_timeout(), stream.read_exact(data))
        .await
        .map_err(|_| BotError::Custom(format!("Apple Music wrapper 读取超时：{context}")))??;
    Ok(())
}

trait EmptyDefault {
    fn if_empty(self, fallback: &str) -> String;
}

impl EmptyDefault for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.trim().is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires live Apple Music catalog access"]
    async fn live_collection_follows_track_pages_past_the_first_page() {
        let collection = AppleCollectionId {
            kind: AppleCollectionKind::Playlist,
            id: "pl.d25f5d1181894928af76c85c967f8f31".to_string(),
        };
        let expected = get_apple_collection(collection, 5001).await.unwrap();
        let url = format!(
            "{APPLE_MUSIC_API}/v1/catalog/{}/playlists/pl.d25f5d1181894928af76c85c967f8f31/tracks?limit=10&l={}",
            apple_storefront(),
            apple_language()
        );
        let first_page: AppleMusicResourceList = apple_get_json(&url, true).await.unwrap();
        assert_eq!(first_page.data.len(), 10);
        assert!(first_page.next.is_some());
        let items = collect_apple_collection_tracks(first_page, 5001)
            .await
            .unwrap();
        assert!(items.len() > 25);
        assert_eq!(
            items.iter().map(|item| &item.id).collect::<Vec<_>>(),
            expected
                .items
                .iter()
                .map(|item| &item.id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn collection_page_preserves_next_link_and_rejects_external_auth_targets() {
        let page: AppleMusicResourceList = serde_json::from_value(serde_json::json!({
            "data": [], "next": "/v1/catalog/us/playlists/pl.example/tracks?offset=100"
        }))
        .unwrap();
        let next = apple_collection_next_url(page.next.as_deref().unwrap()).unwrap();
        let parsed = Url::parse(&next).unwrap();
        assert_eq!(parsed.host_str(), Some("amp-api.music.apple.com"));
        assert!(
            parsed
                .query_pairs()
                .any(|(key, value)| key == "offset" && value == "100")
        );
        assert!(parsed.query_pairs().any(|(key, _)| key == "l"));
        assert!(apple_collection_next_url("https://example.test/v1/tracks").is_err());
        assert!(apple_collection_next_url("//example.test/v1/tracks").is_err());
        assert!(apple_collection_next_url("/not-an-api/tracks").is_err());
    }
    use crate::music::provider::download_track_media;

    #[test]
    fn parses_apple_music_url_query_song_id() {
        assert_eq!(
            parse_apple_track_id("https://music.apple.com/us/album/x/1440841360?i=1440841363"),
            Some("1440841363".to_string())
        );
    }

    #[test]
    fn parses_apple_prefixed_id() {
        assert_eq!(
            parse_apple_track_id("am:1440841363"),
            Some("1440841363".to_string())
        );
    }

    #[test]
    fn parses_apple_collection_urls() {
        assert_eq!(
            parse_apple_collection_id("https://music.apple.com/us/album/1989/1440935467"),
            Some(AppleCollectionId {
                kind: AppleCollectionKind::Album,
                id: "1440935467".to_string(),
            })
        );
        assert_eq!(
            parse_apple_collection_id("https://music.apple.com/us/playlist/test/pl.u-abc123DEF"),
            Some(AppleCollectionId {
                kind: AppleCollectionKind::Playlist,
                id: "pl.u-abc123DEF".to_string(),
            })
        );
        assert_eq!(
            parse_apple_collection_id("https://music.apple.com/cn/album/x/1624000713?i=1624001324"),
            None
        );
    }

    #[test]
    fn parses_apple_collection_urls_without_a_name_slug() {
        for (url, kind, id) in [
            (
                "https://music.apple.com/cn/playlist/pl.pm-example",
                AppleCollectionKind::Playlist,
                "pl.pm-example",
            ),
            (
                "https://music.apple.com/us/playlist/pl.catalog-example/?l=en-US",
                AppleCollectionKind::Playlist,
                "pl.catalog-example",
            ),
            (
                "https://music.apple.com/cn/album/1624000713",
                AppleCollectionKind::Album,
                "1624000713",
            ),
            (
                "https://music.apple.com/us/album/12345/?l=en-US",
                AppleCollectionKind::Album,
                "12345",
            ),
        ] {
            assert_eq!(
                parse_apple_collection_id(url),
                Some(AppleCollectionId {
                    kind,
                    id: id.into(),
                }),
                "{url}"
            );
        }
    }

    #[test]
    fn recommendation_card_urls_round_trip_through_collection_parser() {
        for (resource_type, kind, id) in [
            ("playlists", AppleCollectionKind::Playlist, "pl.pm-example"),
            (
                "playlists",
                AppleCollectionKind::Playlist,
                "pl.catalog-example",
            ),
            ("albums", AppleCollectionKind::Album, "1624000713"),
            ("albums", AppleCollectionKind::Album, "12345"),
        ] {
            let content = serde_json::from_value(serde_json::json!({
                "id": id, "type": resource_type,
                "attributes": {"name": "测试推荐", "url": "https://music.apple.com/cn/playlist/account-slug/pl.pm-example"}
            }))
            .unwrap();
            let card = recommendation_playlist("selection-1", &content);
            assert!(!card.url.contains("account-slug"));
            assert_eq!(
                parse_apple_collection_id(&card.url),
                Some(AppleCollectionId {
                    kind,
                    id: id.into()
                })
            );
        }
    }

    #[test]
    fn collection_urls_reject_other_hosts_resources_and_song_links() {
        for url in [
            "https://music.apple.com.example.test/cn/playlist/pl.pm-example",
            "https://example.test/cn/album/1624000713",
            "https://music.apple.com/cn/song/1624001324",
            "https://music.apple.com/cn/album/1624000713?i=1624001324",
            "https://music.apple.com/cn/album/1624000713?songId=1624001324",
            "https://music.apple.com/cn/album/0",
            "https://music.apple.com/cn/album/000000",
            "https://music.apple.com/cn/album/123456789012345678901",
            "https://music.apple.com/cn/album/invalid",
            "https://music.apple.com/cn/playlist/pl.pm-example/tracks",
            "https://music.apple.com/cn/playlist/extra/name/pl.pm-example",
        ] {
            assert_eq!(parse_apple_collection_id(url), None, "{url}");
        }
    }

    #[test]
    fn parses_apple_ttml_to_lrc() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml">
<body><div>
<p begin="27.395" end="30.0"><span>Hello</span><span> world</span></p>
<p begin="01:02.340" end="01:04.0">Rice &amp; Field</p>
</div></body>
</tt>"#;
        assert_eq!(
            parse_ttml_to_lrc(ttml),
            "[00:27.39]Hello world\n[01:02.34]Rice & Field"
        );
    }

    #[test]
    fn parses_nested_ttml_text_without_secondary_tracks() {
        let ttml = r#"<tt><body><div>
<p begin='1.5s'>Hello <span begin='1.5s' end='2s'>world</span><span role='x-translation'>你好</span></p>
<p begin='00:01:02.340'>Rice &amp; Field</p>
</div></body></tt>"#;
        assert_eq!(
            parse_ttml_to_lrc(ttml),
            "[00:01.50]Hello world\n[01:02.34]Rice & Field"
        );
    }

    #[test]
    fn builds_apple_search_url() {
        let url = apple_search_url("hello", 5);
        assert!(url.contains(&format!(
            "amp-api.music.apple.com/v1/catalog/{}/search",
            apple_storefront()
        )));
        assert!(url.contains("types=songs"));
        assert!(url.contains("limit=5"));
    }

    #[test]
    fn parses_personal_recommendation_playlist_ids() {
        let payload = r#"{
            "data": [{
                "relationships": {
                    "contents": {
                        "data": [
                            {"id": "pl.pm-123"},
                            {"id": "pl.pm-456"}
                        ]
                    }
                }
            }]
        }"#;
        let parsed: AppleMusicRecommendationsResponse = serde_json::from_str(payload).unwrap();
        let ids = parsed
            .data
            .into_iter()
            .flat_map(|recommendation| recommendation.relationships)
            .flat_map(|relationships| relationships.contents)
            .flat_map(|contents| contents.data)
            .map(|content| content.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["pl.pm-123", "pl.pm-456"]);
    }

    #[test]
    fn recommendation_cards_are_account_neutral() {
        let content: AppleMusicRecommendationContent = serde_json::from_str(
            r#"{
                "id": "pl.pm-example",
                "type": "playlists",
                "attributes": {
                    "name": "音乐新发现",
                    "curatorName": "Shared User的 Apple Music",
                    "description": {"standard": "发现新歌"},
                    "artwork": {"url": "https://example.test/{w}x{h}.jpg"},
                    "url": "https://music.apple.com/cn/playlist/test/pl.pm-example"
                }
            }"#,
        )
        .unwrap();
        let card = recommendation_playlist("recommendation", &content);
        assert_eq!(card.curator, "Apple Music");
        assert!(!card.curator.contains("Shared User"));
        assert!(card.description.is_empty());
        assert!(!card.url.contains("test/"));
    }

    #[test]
    fn personalized_mix_titles_do_not_expose_account_names() {
        let mut content: AppleMusicRecommendationContent = serde_json::from_value(serde_json::json!({
            "id": "pl.pm-example", "type": "playlists",
            "attributes": {"name": "Shared User 的私人推荐", "curatorName": "Shared User", "description": {"standard": "Shared User 经常听"}, "url": "https://music.apple.com/cn/playlist/shared-user/pl.pm-example"}
        })).unwrap();
        let card = recommendation_playlist("selection-1", &content);
        let serialized = serde_json::to_string(&card).unwrap();
        assert_eq!(card.name, "专属推荐歌单");
        assert!(!serialized.contains("Shared User"));
        assert!(!serialized.contains("shared-user"));
        content.attributes.as_mut().unwrap().name = Some("放松歌单".into());
        assert_eq!(
            recommendation_playlist("selection-1", &content).name,
            "放松歌单"
        );
    }

    #[test]
    fn extracts_media_user_token_from_cookie_string() {
        assert_eq!(
            normalize_media_user_token("foo=bar; media-user-token=token-value; baz=qux"),
            "token-value"
        );
        assert_eq!(normalize_media_user_token("token-only"), "token-only");
    }

    #[test]
    fn parses_user_quality_values() {
        assert_eq!(quality_from_str("standard"), AppleMusicQuality::Standard);
        assert_eq!(quality_from_str("lossless"), AppleMusicQuality::Lossless);
        assert_eq!(quality_from_str("hires"), AppleMusicQuality::HiRes);
        assert_eq!(AppleMusicQuality::Lossless.as_str(), "lossless");
    }

    #[test]
    fn apple_wrapper_route_preserves_requested_quality() {
        assert_eq!(
            apple_wrapper_internal_url("127.0.0.1", "1624001324", AppleMusicQuality::High, None),
            "applemusic-wrapper://127.0.0.1/1624001324?quality=high"
        );
        assert_eq!(
            apple_wrapper_internal_url(
                "127.0.0.1",
                "1624001324",
                AppleMusicQuality::Lossless,
                None
            ),
            "applemusic-wrapper://127.0.0.1/1624001324?quality=lossless"
        );
    }

    #[test]
    fn apple_wrapper_lite_route_preserves_requested_quality_and_duration() {
        assert_eq!(
            apple_wrapper_lite_internal_url("1624001324", AppleMusicQuality::HiRes, Some(241),),
            "applemusic-wrapper-lite://1624001324?quality=hires&duration=241"
        );
    }

    #[test]
    fn accepts_m3u8_urls_with_query_parameters() {
        assert!(is_m3u8_url("https://example.test/audio.m3u8?token=abc"));
        assert!(is_m3u8_url("https://example.test/audio.m3u8#fragment"));
        assert!(!is_m3u8_url("https://example.test/audio.mp4?format=m3u8"));
    }

    #[test]
    fn wrapper_lite_key_envelope_keeps_temari_fields_in_data() {
        let response: AppleWrapperLiteResponse<AppleWrapperLiteKeyData> =
            serde_json::from_str(
                r#"{"code":0,"msg":"SUCCESS","data":{"adamId":"1","keyUri":"skd://x","contentKey":"","ctx":"ctx","state":"state","rcx":"0x1","rax":"0x2","rdx":"0x3","r9":"0x4","rbp":"0x5"}}"#,
            )
            .unwrap();
        let data = response.data.unwrap();
        assert_eq!(data.adam_id, "1");
        assert_eq!(data.ctx, "ctx");
        assert_eq!(data.state, "state");
    }

    #[test]
    fn wrapper_lite_rejects_pattern_encryption_before_decrypting() {
        let tenc = TencBox {
            version: 0,
            default_is_protected: 1,
            default_per_sample_iv_size: 16,
            default_kid: [0; 16],
            default_crypt_byte_block: 1,
            default_skip_byte_block: 0,
            default_constant_iv: None,
        };
        let error = validate_wrapper_lite_cenc_layout(&[], &tenc).unwrap_err();
        assert!(error.to_string().contains("pattern encryption"));
    }

    fn wrapper_lite_test_tenc() -> TencBox {
        TencBox {
            version: 1,
            default_is_protected: 1,
            default_per_sample_iv_size: 0,
            default_kid: [0; 16],
            default_crypt_byte_block: 0,
            default_skip_byte_block: 0,
            default_constant_iv: Some(vec![0; 16]),
        }
    }

    #[test]
    fn wrapper_lite_single_protected_subsample_preserves_clear_bytes_and_tail() {
        let entries = [
            SubsampleEntry {
                bytes_of_clear_data: 5,
                bytes_of_protected_data: 35,
            },
            SubsampleEntry {
                bytes_of_clear_data: 8,
                bytes_of_protected_data: 0,
            },
        ];
        validate_wrapper_lite_subsamples(48, &entries).unwrap();
        let data = (0..48).collect::<Vec<u8>>();
        let mut commands = Vec::new();
        collect_wrapper_decrypt_sample_job(
            &mut commands,
            &data,
            0,
            48,
            &entries,
            &wrapper_lite_test_tenc(),
        )
        .unwrap();
        assert_eq!(commands.len(), 1);
        let AppleWrapperDecryptCommand::Job(job) = &commands[0] else {
            panic!("expected decrypt job");
        };
        assert_eq!(job.ranges, vec![5..37]);
        assert_eq!(job.payload, data[5..37]);
        // A fully clear sample has no decrypt job and must remain unchanged.
        let mut clear_commands = Vec::new();
        let clear = [SubsampleEntry {
            bytes_of_clear_data: 48,
            bytes_of_protected_data: 0,
        }];
        validate_wrapper_lite_subsamples(48, &clear).unwrap();
        collect_wrapper_decrypt_sample_job(
            &mut clear_commands,
            &data,
            0,
            48,
            &clear,
            &wrapper_lite_test_tenc(),
        )
        .unwrap();
        assert!(clear_commands.is_empty());
    }

    #[test]
    fn wrapper_lite_rejects_ambiguous_or_out_of_bounds_subsamples() {
        for entries in [
            vec![SubsampleEntry {
                bytes_of_clear_data: 49,
                bytes_of_protected_data: 0,
            }],
            vec![SubsampleEntry {
                bytes_of_clear_data: 1,
                bytes_of_protected_data: u32::MAX,
            }],
            vec![SubsampleEntry {
                bytes_of_clear_data: 0,
                bytes_of_protected_data: 47,
            }],
            vec![
                SubsampleEntry {
                    bytes_of_clear_data: 0,
                    bytes_of_protected_data: 32,
                },
                SubsampleEntry {
                    bytes_of_clear_data: 0,
                    bytes_of_protected_data: 16,
                },
            ],
        ] {
            assert!(validate_wrapper_lite_subsamples(48, &entries).is_err());
        }
    }

    #[test]
    fn wrapper_lite_validates_senc_sample_count_before_decrypting() {
        let mut tfhd = vec![0, 0, 0, 0x18];
        tfhd.extend_from_slice(&1u32.to_be_bytes());
        tfhd.extend_from_slice(&1024u32.to_be_bytes());
        tfhd.extend_from_slice(&48u32.to_be_bytes());
        let mut trun = vec![0; 4];
        trun.extend_from_slice(&2u32.to_be_bytes());
        let mut senc = vec![0, 0, 0, 2];
        senc.extend_from_slice(&1u32.to_be_bytes());
        senc.extend_from_slice(&1u16.to_be_bytes());
        senc.extend_from_slice(&0u16.to_be_bytes());
        senc.extend_from_slice(&48u32.to_be_bytes());
        let traf = [
            build_box(b"tfhd", tfhd),
            build_box(b"trun", trun),
            build_box(b"senc", senc),
        ]
        .concat();
        let fragment = build_box(b"moof", build_box(b"traf", traf));
        let error =
            validate_wrapper_lite_cenc_layout(&fragment, &wrapper_lite_test_tenc()).unwrap_err();
        assert!(error.to_string().contains("sample 数量"));
    }

    #[test]
    fn alac_stream_cookie_skips_atom_version_flags() {
        let cookie: Vec<u8> = (0..24).collect();
        let mut config_body = vec![0, 0, 0, 0];
        config_body.extend_from_slice(&cookie);
        let config = build_box(b"alac", config_body);
        let mut entry_body = vec![0u8; 28];
        entry_body.extend_from_slice(&config);
        let entry = build_box(b"alac", entry_body);
        let mut stsd_body = vec![0u8; 8];
        stsd_body[4..8].copy_from_slice(&1u32.to_be_bytes());
        stsd_body.extend_from_slice(&entry);
        let stsd = build_box(b"stsd", stsd_body);
        assert_eq!(find_alac_cookie(&stsd).unwrap(), cookie);
    }

    fn progressive_alac_test_file() -> Vec<u8> {
        let mut cookie = vec![0u8; 28];
        cookie[4..8].copy_from_slice(&4096u32.to_be_bytes());
        cookie[9] = 16;
        cookie[10] = 40;
        cookie[11] = 10;
        cookie[12] = 14;
        cookie[13] = 2;
        cookie[24..28].copy_from_slice(&44100u32.to_be_bytes());
        let mut entry = vec![0u8; 28];
        entry.extend(build_box(b"alac", cookie));
        let mut stsd = vec![0u8; 8];
        stsd[4..8].copy_from_slice(&1u32.to_be_bytes());
        stsd.extend(build_box(b"alac", entry));
        let mut out = build_box(b"stsd", stsd);
        let mut mdhd = vec![0u8; 24];
        mdhd[12..16].copy_from_slice(&44100u32.to_be_bytes());
        out.extend(build_box(b"mdhd", mdhd));
        let samples = [
            ProgressiveSample {
                range: 0..2,
                duration: 4096,
                size: 2,
            },
            ProgressiveSample {
                range: 2..4,
                duration: 1024,
                size: 2,
            },
        ];
        out.extend(build_stsz(&samples));
        out.extend(build_stts(&samples));
        out.extend(build_stsc(samples.len() as u32));
        let data_offset = out.len() + build_stco(0).len() + 8;
        out.extend(build_stco(data_offset as u32));
        // Deliberately opaque packets: transport reconstruction must preserve
        // bytes even if the optional Apple end-element repair cannot scan them.
        out.extend(build_box(b"mdat", vec![0xff, 0x12, 0xff, 0x34]));
        out
    }

    #[test]
    fn completed_alac_transport_preserves_packet_boundaries_and_timing() {
        let stream = alac_transport_from_progressive(&progressive_alac_test_file()).unwrap();
        assert_eq!(&stream[..8], ALAC_STREAM_MAGIC);
        assert_eq!(&stream[8..12], &2u32.to_be_bytes());
        assert_eq!(&stream[12..16], &44100u32.to_be_bytes());
        assert_eq!(&stream[16..20], &[2, 16, 0, 24]);
        assert_eq!(&stream[48..52], &2u32.to_be_bytes());
        assert_eq!(&stream[52..56], &4096u32.to_be_bytes());
        assert_eq!(&stream[56..58], &[0xff, 0x12]);
        assert_eq!(&stream[62..66], &1024u32.to_be_bytes());
        assert_eq!(&stream[66..], &[0xff, 0x34]);
    }

    #[test]
    fn completed_alac_transport_rejects_truncated_packet() {
        let mut file = progressive_alac_test_file();
        file.pop();
        assert!(alac_transport_from_progressive(&file).is_err());
    }

    #[test]
    fn completed_alac_transport_rejects_mismatched_timing() {
        let mut file = progressive_alac_test_file();
        let timing = find_box_recursive(&file, 0, file.len(), b"stts").unwrap();
        let count_offset = timing.payload_start() + 8;
        file[count_offset..count_offset + 4].copy_from_slice(&3u32.to_be_bytes());
        assert!(alac_transport_from_progressive(&file).is_err());
    }

    #[test]
    fn completed_alac_transport_honors_multiple_chunk_offsets() {
        for wide_offsets in [false, true] {
            let mut file = progressive_alac_test_file();
            let mapping = find_box_recursive(&file, 0, file.len(), b"stsc").unwrap();
            let samples_per_chunk = mapping.payload_start() + 12;
            file[samples_per_chunk..samples_per_chunk + 4].copy_from_slice(&1u32.to_be_bytes());
            let stco = find_box_recursive(&file, 0, file.len(), b"stco").unwrap();
            file.truncate(stco.start);
            let offset_width = if wide_offsets { 8 } else { 4 };
            let data_offset = file.len() + 16 + offset_width * 2 + 8;
            let mut offsets = vec![0u8; 4];
            offsets.extend_from_slice(&2u32.to_be_bytes());
            for offset in [data_offset, data_offset + 4] {
                if wide_offsets {
                    offsets.extend_from_slice(&(offset as u64).to_be_bytes());
                } else {
                    offsets.extend_from_slice(&(offset as u32).to_be_bytes());
                }
            }
            file.extend(build_box(
                if wide_offsets { b"co64" } else { b"stco" },
                offsets,
            ));
            file.extend(build_box(b"mdat", vec![0xff, 0x12, 0, 0, 0xff, 0x34]));
            let stream = alac_transport_from_progressive(&file).unwrap();
            assert_eq!(&stream[56..58], &[0xff, 0x12]);
            assert_eq!(&stream[66..], &[0xff, 0x34]);
        }
    }

    #[test]
    fn rejects_webplayback_preview_asset() {
        let asset = WebPlaybackAsset {
            url: "https://example.test/preview.m3u8".to_string(),
            flavor: Some("28:ctrp256".to_string()),
            metadata: WebPlaybackMetadata {
                bit_rate: 256,
                duration: 23_777,
            },
        };
        let err = validate_webplayback_asset_duration(&asset, Some(232))
            .expect_err("preview asset should be rejected");
        assert!(is_apple_preview_error(&err));
    }

    #[test]
    fn parses_wrapper_pool_endpoint_ports() {
        let default = parse_wrapper_endpoint("127.0.0.1").unwrap();
        assert_eq!(default.decrypt_addr, "127.0.0.1:10020");
        assert_eq!(default.m3u8_addr, "127.0.0.1:20020");

        let shifted = parse_wrapper_endpoint("127.0.0.1:10024").unwrap();
        assert_eq!(shifted.decrypt_addr, "127.0.0.1:10024");
        assert_eq!(shifted.m3u8_addr, "127.0.0.1:20024");
    }

    #[test]
    fn parses_enhanced_hls_master_and_selects_lossless() {
        let master = r#"#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=64000,AVERAGE-BANDWIDTH=64000,CODECS="mp4a.40.2",AUDIO="audio-stereo-44100"
low.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2116000,AVERAGE-BANDWIDTH=2116000,CODECS="alac",AUDIO="audio-alac-stereo-44100-16"
lossless.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=4600000,AVERAGE-BANDWIDTH=4600000,CODECS="alac",AUDIO="audio-alac-stereo-96000-24"
hires.m3u8
"#;
        let variants = parse_enhanced_hls_master(master).unwrap();
        assert_eq!(variants.len(), 3);
        assert_eq!(
            select_variant_for_quality(&variants, AppleMusicQuality::Lossless)
                .unwrap()
                .uri,
            "lossless.m3u8"
        );
        assert_eq!(
            select_variant_for_quality(&variants, AppleMusicQuality::HiRes)
                .unwrap()
                .uri,
            "hires.m3u8"
        );
        assert_eq!(
            select_variant_for_quality(&variants, AppleMusicQuality::Standard)
                .unwrap()
                .uri,
            "low.m3u8"
        );
    }

    fn alac_quality_variants(entries: &[(i64, i64, i64)]) -> Vec<EnhancedHlsVariant> {
        entries
            .iter()
            .map(|&(sample_rate, bit_depth, avg_bw)| EnhancedHlsVariant {
                codecs: "alac".to_string(),
                audio: format!("audio-alac-stereo-{sample_rate}-{bit_depth}"),
                avg_bw,
                uri: format!("{sample_rate}-{bit_depth}.m3u8"),
                sample_rate,
                bit_depth,
            })
            .collect()
    }

    #[test]
    fn lossless_and_hires_choose_distinct_sample_rate_tiers() {
        let variants = alac_quality_variants(&[
            (44_100, 24, 1_500_000),
            (48_000, 16, 2_500_000),
            (48_000, 24, 2_000_000),
            (96_000, 24, 4_000_000),
            (192_000, 24, 8_000_000),
        ]);
        let lossless = select_variant_for_quality(&variants, AppleMusicQuality::Lossless).unwrap();
        assert_eq!((lossless.sample_rate, lossless.bit_depth), (48_000, 24));
        let hires = select_variant_for_quality(&variants, AppleMusicQuality::HiRes).unwrap();
        assert_eq!((hires.sample_rate, hires.bit_depth), (192_000, 24));
    }

    #[test]
    fn lossless_fallback_uses_nearest_higher_alac_and_preserves_actual_metadata() {
        let variants = alac_quality_variants(&[
            (0, 0, 1_000_000),
            (96_000, 16, 5_000_000),
            (96_000, 24, 4_000_000),
            (192_000, 24, 8_000_000),
        ]);
        let selected = select_variant_for_quality(&variants, AppleMusicQuality::Lossless).unwrap();
        assert_eq!((selected.sample_rate, selected.bit_depth), (96_000, 24));
        assert_eq!(selected.uri, "96000-24.m3u8");
    }

    #[test]
    fn hires_falls_back_to_standard_lossless_without_fabricating_resolution() {
        let variants = alac_quality_variants(&[(44_100, 24, 1_500_000), (48_000, 24, 2_000_000)]);
        let selected = select_variant_for_quality(&variants, AppleMusicQuality::HiRes).unwrap();
        assert_eq!((selected.sample_rate, selected.bit_depth), (48_000, 24));
    }

    #[test]
    fn lossless_quality_never_silently_selects_aac() {
        let variants = parse_enhanced_hls_master(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"mp4a.40.2\",AUDIO=\"audio-stereo-44100\"\nstandard.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=256000,CODECS=\"mp4a.40.2\",AUDIO=\"audio-stereo-44100\"\nhigh.m3u8\n",
        )
        .unwrap();
        assert!(select_variant_for_quality(&variants, AppleMusicQuality::Lossless).is_none());
        assert!(select_variant_for_quality(&variants, AppleMusicQuality::HiRes).is_none());
        assert_eq!(
            select_variant_for_quality(&variants, AppleMusicQuality::Standard)
                .unwrap()
                .uri,
            "standard.m3u8"
        );
        assert_eq!(
            select_variant_for_quality(&variants, AppleMusicQuality::High)
                .unwrap()
                .uri,
            "high.m3u8"
        );
    }

    #[test]
    fn parses_enhanced_hls_media_keys_and_mp4() {
        let playlist = r#"#EXTM3U
#EXT-X-KEY:METHOD=SAMPLE-AES,URI="skd://itunes.apple.com/P000000000/s1/e1",KEYFORMAT="com.apple.streamingkeydelivery"
#EXT-X-KEY:METHOD=SAMPLE-AES,URI="data:text/plain;base64,AAAA",KEYFORMAT="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"
#EXT-X-MAP:URI="track.m4a",BYTERANGE="100@0"
#EXTINF:1,
#EXT-X-BYTERANGE:200@100
track.m4a
#EXT-X-KEY:METHOD=SAMPLE-AES,URI="skd://itunes.apple.com/p1/c2",KEYFORMAT="com.apple.streamingkeydelivery"
#EXTINF:1,
#EXT-X-BYTERANGE:300@300
track.m4a
"#;
        let media =
            parse_enhanced_hls_media("https://example.test/a/master.m3u8", playlist).unwrap();
        assert_eq!(media.mp4_url, "https://example.test/a/track.m4a");
        assert_eq!(media.duration_seconds, 2.0);
        assert_eq!(media.parts.len(), 3);
        assert_eq!(
            media
                .parts
                .iter()
                .map(|part| part.range)
                .collect::<Vec<_>>(),
            vec![
                Some(HlsByteRange {
                    offset: 0,
                    length: 100
                }),
                Some(HlsByteRange {
                    offset: 100,
                    length: 200
                }),
                Some(HlsByteRange {
                    offset: 300,
                    length: 300
                })
            ]
        );
        assert_eq!(
            media.seg_keys,
            vec![
                APPLE_PREFETCH_KEY_URI.to_string(),
                "skd://itunes.apple.com/p1/c2".to_string()
            ]
        );
    }

    #[test]
    fn finds_tenc_inside_stsd_enca_sample_entry() {
        fn mp4_box(typ: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
            out.extend_from_slice(typ);
            out.extend_from_slice(&payload);
            out
        }

        let mut tenc_payload = vec![0, 0, 0, 0, 0, 0, 1, 16];
        tenc_payload.extend_from_slice(&[7u8; 16]);
        let tenc = mp4_box(b"tenc", tenc_payload);
        let schi = mp4_box(b"schi", tenc);
        let sinf = mp4_box(b"sinf", schi);
        let mut enca_payload = vec![0u8; 28];
        enca_payload.extend_from_slice(&sinf);
        let enca = mp4_box(b"enca", enca_payload);
        let mut stsd_payload = vec![0, 0, 0, 0];
        stsd_payload.extend_from_slice(&1u32.to_be_bytes());
        stsd_payload.extend_from_slice(&enca);
        let stsd = mp4_box(b"stsd", stsd_payload);
        let stbl = mp4_box(b"stbl", stsd);
        let minf = mp4_box(b"minf", stbl);
        let mdia = mp4_box(b"mdia", minf);
        let trak = mp4_box(b"trak", mdia);
        let moov = mp4_box(b"moov", trak);

        let parsed = parse_tenc_from_init(&moov).unwrap();
        assert_eq!(parsed.default_per_sample_iv_size, 16);
        assert_eq!(parsed.default_kid, [7u8; 16]);
    }

    #[test]
    fn parses_multiple_trun_runs_in_one_traf() {
        let mut tfhd_payload = vec![0, 0, 0, 0x08];
        tfhd_payload.extend_from_slice(&1u32.to_be_bytes());
        tfhd_payload.extend_from_slice(&1024u32.to_be_bytes());

        let mut first_trun_payload = vec![0, 0, 0x03, 0x01];
        first_trun_payload.extend_from_slice(&2u32.to_be_bytes());
        first_trun_payload.extend_from_slice(&100i32.to_be_bytes());
        first_trun_payload.extend_from_slice(&10u32.to_be_bytes());
        first_trun_payload.extend_from_slice(&11u32.to_be_bytes());
        first_trun_payload.extend_from_slice(&12u32.to_be_bytes());
        first_trun_payload.extend_from_slice(&13u32.to_be_bytes());

        let mut second_trun_payload = vec![0, 0, 0x03, 0x01];
        second_trun_payload.extend_from_slice(&1u32.to_be_bytes());
        second_trun_payload.extend_from_slice(&124i32.to_be_bytes());
        second_trun_payload.extend_from_slice(&14u32.to_be_bytes());
        second_trun_payload.extend_from_slice(&15u32.to_be_bytes());

        let mut traf_payload = build_box(b"tfhd", tfhd_payload);
        traf_payload.extend_from_slice(&build_box(b"trun", first_trun_payload));
        traf_payload.extend_from_slice(&build_box(b"trun", second_trun_payload));
        let traf_data = build_box(b"traf", traf_payload);
        let traf = read_mp4_box(&traf_data, 0).unwrap().unwrap();
        let tenc = TencBox {
            version: 0,
            default_is_protected: 1,
            default_per_sample_iv_size: 16,
            default_kid: [0; 16],
            default_crypt_byte_block: 0,
            default_skip_byte_block: 0,
            default_constant_iv: None,
        };

        let parsed = parse_traf(&traf_data, traf, &tenc).unwrap();

        assert_eq!(parsed.runs.len(), 2);
        assert_eq!(parsed.runs[0].data_offset, Some(100));
        assert_eq!(parsed.runs[0].senc_sample_offset, 0);
        assert_eq!(
            parsed.runs[0]
                .samples
                .iter()
                .map(|sample| (sample.duration, sample.size))
                .collect::<Vec<_>>(),
            vec![(10, 11), (12, 13)]
        );
        assert_eq!(parsed.runs[1].data_offset, Some(124));
        assert_eq!(parsed.runs[1].senc_sample_offset, 2);
        assert_eq!(
            parsed.runs[1]
                .samples
                .iter()
                .map(|sample| (sample.duration, sample.size))
                .collect::<Vec<_>>(),
            vec![(14, 15)]
        );
    }

    #[test]
    fn sanitized_stsd_rewrites_entry_count_to_one() {
        fn mp4_box(typ: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
            out.extend_from_slice(typ);
            out.extend_from_slice(&payload);
            out
        }

        let enca = mp4_box(b"enca", vec![0u8; 28]);
        let mp4a = mp4_box(b"mp4a", vec![0u8; 28]);
        let mut stsd_payload = vec![0, 0, 0, 0];
        stsd_payload.extend_from_slice(&2u32.to_be_bytes());
        stsd_payload.extend_from_slice(&enca);
        stsd_payload.extend_from_slice(&mp4a);
        let data = mp4_box(b"stsd", stsd_payload);
        let stsd = read_mp4_box(&data, 0).unwrap().unwrap();

        let sanitized = sanitize_stsd(&data, stsd).unwrap();
        assert_eq!(&sanitized[12..16], &1u32.to_be_bytes());
        let entry = read_mp4_box(&sanitized, 16).unwrap().unwrap();
        assert_eq!(&entry.typ, b"alac");
        assert_eq!(entry.end(), sanitized.len());
    }

    #[test]
    fn sanitized_streaming_fragment_removes_cenc_aux_boxes_and_patches_offset() {
        fn mp4_box(typ: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
            out.extend_from_slice(typ);
            out.extend_from_slice(&payload);
            out
        }

        let tfhd = mp4_box(b"tfhd", vec![0, 0, 0, 0, 0, 0, 0, 1]);
        let mut trun_payload = vec![0, 0, 2, 1];
        trun_payload.extend_from_slice(&1u32.to_be_bytes());
        trun_payload.extend_from_slice(&0i32.to_be_bytes());
        trun_payload.extend_from_slice(&4u32.to_be_bytes());
        let trun = mp4_box(b"trun", trun_payload.clone());
        let senc = mp4_box(b"senc", vec![0, 0, 0, 0, 0, 0, 0, 1]);
        let traf = mp4_box(b"traf", [tfhd.clone(), trun, senc.clone()].concat());
        let moof = mp4_box(b"moof", traf);
        trun_payload[8..12].copy_from_slice(&((moof.len() + 8) as i32).to_be_bytes());
        let trun = mp4_box(b"trun", trun_payload);
        let moof = mp4_box(b"moof", mp4_box(b"traf", [tfhd, trun, senc].concat()));
        let mdat = mp4_box(b"mdat", vec![1, 2, 3, 4]);
        let input = [mp4_box(b"styp", vec![0; 4]), moof, mdat].concat();

        let output = sanitize_streaming_fragment(&input).unwrap();
        assert!(find_box_recursive(&output, 0, output.len(), b"senc").is_none());
        let moof = find_child_box(&output, 0, output.len(), b"moof").unwrap();
        let mdat = find_child_box(&output, moof.end(), output.len(), b"mdat").unwrap();
        let trun = find_box_recursive(&output, moof.payload_start(), moof.end(), b"trun").unwrap();
        let payload = &output[trun.payload_start()..trun.end()];
        let data_offset = i32::from_be_bytes(payload[8..12].try_into().unwrap());
        assert_eq!(data_offset as usize, mdat.payload_start() - moof.start);
        assert_eq!(&output[mdat.payload_start()..mdat.end()], &[1, 2, 3, 4]);
    }

    #[tokio::test]
    #[ignore = "requires live wrapper-lite and raw wrapper; compares identical public audio bytes"]
    async fn live_wrapper_lite_fragment_matches_legacy_plaintext() {
        let track_id =
            std::env::var("APPLE_MUSIC_TEST_ID").unwrap_or_else(|_| "1398581234".to_string());
        let quality_name =
            std::env::var("APPLE_MUSIC_TEST_QUALITY").unwrap_or_else(|_| "lossless".to_string());
        let quality = quality_from_str(&quality_name);
        let lite = APPLE_WRAPPER_LITE_ENDPOINTS
            .first()
            .expect("wrapper-lite configured");
        let legacy = APPLE_WRAPPER_ENDPOINTS
            .first()
            .expect("legacy wrapper configured");
        let master_url = wrapper_lite_m3u8_url(lite, &track_id).await.unwrap();
        let master = download_text(&master_url).await.unwrap();
        let variants = parse_enhanced_hls_master(&master).unwrap();
        let variant = select_variant_for_quality(&variants, quality).unwrap();
        let media_url = absolute_hls_url(&master_url, &variant.uri);
        let media = parse_enhanced_hls_media(&media_url, &download_text(&media_url).await.unwrap())
            .unwrap();
        let init = download_enhanced_hls_part(&media.parts[0]).await.unwrap();
        let encrypted = download_enhanced_hls_part(&media.parts[1]).await.unwrap();
        let tenc = parse_tenc_from_init(&init).unwrap();
        validate_wrapper_lite_cenc_layout(&encrypted, &tenc).unwrap();
        let key = &media.seg_keys[0];
        let mut lite_plain = encrypted.clone();
        decrypt_wrapper_lite_fragment(lite, &track_id, key, &mut lite_plain, &tenc)
            .await
            .unwrap();
        let mut legacy_plain = encrypted;
        let commands =
            collect_wrapper_decrypt_commands(&legacy_plain, &track_id, &[key.clone()], &tenc)
                .unwrap();
        run_wrapper_decrypt_session(&legacy.decrypt_addr, commands, &mut legacy_plain)
            .await
            .unwrap();
        assert!(
            lite_plain == legacy_plain,
            "Temari and raw wrapper plaintext differ"
        );
        let combined = [init.clone(), lite_plain.clone()].concat();
        let progressive = remux_decrypted_fmp4_to_progressive(&combined).unwrap();
        let folder = std::env::temp_dir().join("music-alac-live-fixture");
        tokio::fs::create_dir_all(&folder).await.unwrap();
        let path = folder.join(format!("{track_id}-{quality_name}.m4a"));
        tokio::fs::write(&path, &progressive).await.unwrap();
        let decoded = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        assert!(
            decoded.status.success() && decoded.stderr.is_empty(),
            "FFmpeg failed to decode verified fragment"
        );
        if variant.is_alac() {
            let transport = alac_transport_from_progressive(&progressive).unwrap();
            let mut end = 24 + u16::from_be_bytes(transport[18..20].try_into().unwrap()) as usize;
            for _ in 0..12 {
                if end >= transport.len() {
                    break;
                }
                let length =
                    u32::from_be_bytes(transport[end..end + 4].try_into().unwrap()) as usize;
                end += 8 + length;
            }
            tokio::fs::write(
                folder.join(format!("{track_id}-{quality_name}.head.alacstream")),
                &transport[..end],
            )
            .await
            .unwrap();
        }
        eprintln!(
            "verified public track {track_id}, quality={quality_name}, fragment_bytes={}, plaintext_identical=true, ffmpeg_decode=true",
            lite_plain.len()
        );
    }

    #[tokio::test]
    #[ignore = "requires live Apple Music credentials and wrapper"]
    async fn live_search_resolve_and_download_without_telegram() {
        let results = APPLE_MUSIC_PROVIDER.search("晴天", 1).await.unwrap();
        let first = results
            .first()
            .expect("Apple Music search returned no songs");
        let track = APPLE_MUSIC_PROVIDER
            .resolve(&first.id, Some(&first.id))
            .await
            .unwrap();
        let media = download_track_media(&track).await.unwrap();
        assert!(!media.audio.is_empty());
    }

    #[tokio::test]
    #[ignore = "requires live Apple Music credentials, wrapper, and a lossless-capable track"]
    async fn live_wrapper_lossless_download_without_telegram() {
        let results = APPLE_MUSIC_PROVIDER.search("晴天", 1).await.unwrap();
        let first = results
            .first()
            .expect("Apple Music search returned no songs");
        let host = SETTINGS.music.applemusic.wrapper_host.trim().to_string();
        assert!(!host.is_empty(), "wrapper_host is required");
        let audio = download_via_wrapper(&host, &first.id, AppleMusicQuality::Lossless)
            .await
            .unwrap();
        assert!(!audio.is_empty());
    }

    #[tokio::test]
    #[ignore = "requires live Apple Music credentials, wrapper, ffprobe, and a lossless-capable track"]
    async fn live_wrapper_lossless_output_is_probeable_without_telegram() {
        let results = APPLE_MUSIC_PROVIDER.search("晴天", 1).await.unwrap();
        let first = results
            .first()
            .expect("Apple Music search returned no songs");
        let host = SETTINGS.music.applemusic.wrapper_host.trim().to_string();
        assert!(!host.is_empty(), "wrapper_host is required");
        let audio = download_via_wrapper(&host, &first.id, AppleMusicQuality::Lossless)
            .await
            .unwrap();
        let path = std::env::temp_dir().join("bot-rs-applemusic-lossless.m4a");
        std::fs::write(&path, audio).unwrap();
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).trim().is_empty());
        let decode = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        assert!(
            decode.status.success(),
            "{}",
            String::from_utf8_lossy(&decode.stderr)
        );
    }

    #[test]
    fn rejects_short_enhanced_hls_media_playlist() {
        let err = validate_hls_media_duration(23.777, Some(232))
            .expect_err("preview media playlist should be rejected");
        assert!(is_apple_preview_error(&err));
        validate_hls_media_duration(232.48, Some(232)).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires live Apple Music credentials, wrapper, ffprobe, and ffmpeg"]
    async fn live_wrapper_lossless_specific_track_id_without_telegram() {
        let track_id =
            std::env::var("APPLE_MUSIC_TEST_ID").unwrap_or_else(|_| "1624001324".to_string());
        let track = resolve_with_quality(&track_id, Some(&track_id), "lossless")
            .await
            .unwrap();
        let media = download_track_media(&track).await.unwrap();
        let path = std::env::temp_dir().join(format!("bot-rs-applemusic-{track_id}.m4a"));
        std::fs::write(&path, &media.audio).unwrap();
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let duration = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<f64>()
            .expect("ffprobe duration");
        if let Some(expected) = track.duration {
            assert!(
                duration >= expected.saturating_sub(5) as f64,
                "duration={duration}, expected={expected}"
            );
        }
        eprintln!("{} -> {} bytes", track.file_name(), media.audio.len());
    }

    #[tokio::test]
    #[ignore = "requires live Apple Music credentials, wrapper, ffprobe, and ffmpeg"]
    async fn live_high_specific_track_id_is_complete_without_telegram() {
        let track_id =
            std::env::var("APPLE_MUSIC_TEST_ID").unwrap_or_else(|_| "1624001324".to_string());
        let track = resolve_with_quality(&track_id, Some(&track_id), "high")
            .await
            .unwrap();
        let media = download_track_media(&track).await.unwrap();
        let path = std::env::temp_dir().join(format!("bot-rs-applemusic-high-{track_id}.m4a"));
        std::fs::write(&path, &media.audio).unwrap();
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name:format=duration",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("codec_name=aac"), "{stdout}");
        let duration = stdout
            .lines()
            .find_map(|line| line.strip_prefix("duration="))
            .and_then(|value| value.parse::<f64>().ok())
            .expect("ffprobe duration");
        if let Some(expected) = track.duration {
            assert!(
                duration >= expected.saturating_sub(5) as f64,
                "duration={duration}, expected={expected}, output={stdout}"
            );
        }
        let decode = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        assert!(
            decode.status.success(),
            "{}",
            String::from_utf8_lossy(&decode.stderr)
        );
        eprintln!(
            "{} -> {} bytes\n{stdout}",
            track.file_name(),
            media.audio.len()
        );
    }
}
