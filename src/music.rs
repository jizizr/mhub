#![allow(dead_code)]

mod applemusic;
mod bilibili;
mod kugou;
mod lyric_credits;
mod netease;
mod netease_session;
mod online_lyrics;
mod provider;
mod qrc_crypto;
pub(crate) mod search_gate;
mod soda;
pub(crate) mod streaming;
mod tencent;
mod word_lyrics;

pub use word_lyrics::WordLine;
mod youtubemusic;

use crate::{ApiError, ApiResult};
pub use provider::{DownloadProgress, MusicPlatform, MusicSearchItem, MusicTrack};
use provider::{
    MusicCollection, MusicMedia, MusicProvider, download_track_media_with_cover_progress,
};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, Serialize)]
pub struct RecommendationTag {
    pub id: String,
    pub label: String,
    pub subtitle: String,
    pub playlist_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecommendationPlaylist {
    pub id: String,
    pub resource_type: String,
    pub name: String,
    pub curator: String,
    pub description: String,
    pub cover: String,
    pub url: String,
    pub recommendation_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecommendationHome {
    pub source: String,
    pub tags: Vec<RecommendationTag>,
    pub playlists: Vec<RecommendationPlaylist>,
    pub tracks: Vec<MusicSearchItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecommendationContinuation {
    pub source: String,
    pub tracks: Vec<MusicSearchItem>,
    /// Apple Music AutoPlay station to continue with `next-tracks`; the client
    /// sends it back for the following batches of the same queue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlatformInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub aliases: &'static [&'static str],
}

/// What a share link needs to show a song: catalog metadata looked up by
/// platform and id alone, without resolving any audio.
#[derive(Clone, Debug, Serialize)]
pub struct TrackInfo {
    pub platform: &'static str,
    pub id: String,
    pub song: String,
    pub singer: String,
    pub album: String,
    pub cover: String,
}

#[derive(Clone, Debug)]
pub struct ResolveOptions {
    pub platform: MusicPlatform,
    pub query: String,
    pub selected_id: Option<String>,
    pub quality: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Lyrics {
    pub plain: String,
    pub translation: String,
    pub words: Vec<WordLine>,
    pub roman: String,
    pub credits: Vec<String>,
}

impl From<provider::MusicLyrics> for Lyrics {
    fn from(mut lyrics: provider::MusicLyrics) -> Self {
        lyric_credits::extract(&mut lyrics);
        Self {
            plain: lyrics.plain,
            translation: lyrics.translation,
            words: lyrics.words,
            roman: lyrics.roman,
            credits: lyrics.credits,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Collection {
    pub platform: String,
    pub id: String,
    pub title: String,
    pub items: Vec<CollectionItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CollectionItem {
    pub platform: String,
    pub id: String,
    pub song: String,
    pub singer: String,
    pub cover: String,
}

/// The order the web app lists platforms in, under the search box.
const LISTED_ORDER: [MusicPlatform; 5] = [
    MusicPlatform::AppleMusic,
    MusicPlatform::Netease,
    MusicPlatform::YouTubeMusic,
    MusicPlatform::Bilibili,
    MusicPlatform::Tencent,
];

pub fn platforms() -> Vec<PlatformInfo> {
    let rank = |platform: MusicPlatform| {
        LISTED_ORDER
            .iter()
            .position(|listed| *listed == platform)
            .unwrap_or(LISTED_ORDER.len())
    };
    let mut online: Vec<_> = MusicPlatform::online().collect();
    online.sort_by_key(|platform| rank(*platform));
    online
        .into_iter()
        .map(|platform| PlatformInfo {
            id: platform.id(),
            label: platform.label(),
            aliases: platform.aliases(),
        })
        .collect()
}

/// A platform MHub still serves; offline ones are refused with a message.
pub fn parse_platform(value: &str) -> ApiResult<MusicPlatform> {
    let platform = parse_any_platform(value)?;
    ensure_online(platform)?;
    Ok(platform)
}

/// Any known platform, offline ones included: for data that merely names a
/// platform, such as the source tracks of a playlist being matched elsewhere.
pub fn parse_any_platform(value: &str) -> ApiResult<MusicPlatform> {
    MusicPlatform::from_alias(value)
        .ok_or_else(|| ApiError::Custom(format!("暂不支持音乐平台：{value}")))
}

pub fn ensure_online(platform: MusicPlatform) -> ApiResult<()> {
    if platform.is_offline() {
        return Err(ApiError::Http {
            status: axum::http::StatusCode::GONE,
            message: format!("{}已下线", platform.label()),
        });
    }
    Ok(())
}

lazy_static::lazy_static! {
    static ref SEARCH_GATE: search_gate::SearchGate =
        search_gate::SearchGate::new(search_gate::Tuning::default());
}

/// A search for someone waiting on the search box.
pub async fn search(
    platform: MusicPlatform,
    keyword: &str,
    limit: usize,
) -> ApiResult<Vec<MusicSearchItem>> {
    match search_in_lane(platform, keyword, limit, search_gate::Lane::Interactive).await {
        // Someone is waiting: while the catalog API rate-limits this server,
        // Apple's public search page usually still answers. Bulk work keeps
        // the gate's pacing instead.
        Err(ApiError::RateLimited { .. }) if platform == MusicPlatform::AppleMusic => {
            crate::upstream_limit::metered(
                crate::upstream_limit::Kind::Search,
                applemusic::search_web(keyword, limit),
            )
            .await
        }
        result => result,
    }
}

/// A paced, cached search for resumable bulk work such as playlist import.
/// Returns `ApiError::RateLimited` instead of waiting past `deadline`.
pub async fn search_bulk(
    platform: MusicPlatform,
    keyword: &str,
    limit: usize,
    deadline: std::time::Instant,
) -> ApiResult<Vec<MusicSearchItem>> {
    search_in_lane(
        platform,
        keyword,
        limit,
        search_gate::Lane::Bulk { deadline },
    )
    .await
}

async fn search_in_lane(
    platform: MusicPlatform,
    keyword: &str,
    limit: usize,
    lane: search_gate::Lane,
) -> ApiResult<Vec<MusicSearchItem>> {
    SEARCH_GATE
        .search(platform, keyword, limit, lane, || {
            provider_for(platform).search(keyword, limit)
        })
        .await
}

pub async fn recommendations(
    platform: MusicPlatform,
    limit: usize,
    refresh: bool,
    exclude: &[String],
) -> ApiResult<Vec<MusicSearchItem>> {
    match platform {
        MusicPlatform::AppleMusic => {
            applemusic::personal_recommendations(limit, refresh, exclude).await
        }
        _ => Err(ApiError::Custom(format!(
            "{} 暂不支持个性推荐",
            platform.label()
        ))),
    }
}

pub async fn recommendation_home(
    platform: MusicPlatform,
    limit: usize,
    refresh: bool,
    exclude: &[String],
) -> ApiResult<RecommendationHome> {
    match platform {
        MusicPlatform::AppleMusic => {
            applemusic::personal_recommendation_home(limit, refresh, exclude).await
        }
        _ => Err(ApiError::Custom(format!(
            "{} 暂不支持个性推荐",
            platform.label()
        ))),
    }
}

pub async fn recommendation_continuation(
    platform: MusicPlatform,
    seed_id: &str,
    seeds: &[String],
    station: Option<&str>,
    exclude: &[String],
    limit: usize,
) -> ApiResult<RecommendationContinuation> {
    match platform {
        MusicPlatform::AppleMusic => {
            applemusic::continuation_recommendations(seed_id, seeds, station, exclude, limit).await
        }
        _ => Err(ApiError::Custom(format!(
            "{} 暂不支持自动续播推荐",
            platform.label()
        ))),
    }
}

pub const MAX_COLLECTION_ITEMS: usize = 5000;

tokio::task_local! {
    static ALLOW_INCOMPLETE_COLLECTION: bool;
}

/// Whether the current collection request accepts a playlist the platform
/// listed only partly. Off unless the web user confirmed it; gateway
/// clients and playlist sync always get the strict check.
pub(crate) fn incomplete_collection_allowed() -> bool {
    ALLOW_INCOMPLETE_COLLECTION
        .try_with(|allowed| *allowed)
        .unwrap_or(false)
}

pub async fn collection(
    platform: MusicPlatform,
    keyword: &str,
    limit: usize,
    allow_incomplete: bool,
) -> ApiResult<Collection> {
    let collection = ALLOW_INCOMPLETE_COLLECTION
        .scope(
            allow_incomplete,
            provider_collection(platform, keyword, limit),
        )
        .await?
        .ok_or_else(|| ApiError::Custom("该平台暂不支持此歌单链接".to_string()))?;
    Ok(collection_to_api(collection))
}

/// `None` when the keyword is not a playlist or album of the platform.
pub async fn provider_collection(
    platform: MusicPlatform,
    keyword: &str,
    limit: usize,
) -> ApiResult<Option<MusicCollection>> {
    provider_for(platform)
        .collection(keyword, limit.clamp(1, MAX_COLLECTION_ITEMS + 1))
        .await
}

fn collection_to_api(collection: MusicCollection) -> Collection {
    Collection {
        platform: collection.platform.id().to_string(),
        id: collection.id,
        title: collection.title,
        items: collection
            .items
            .into_iter()
            .map(|item| CollectionItem {
                platform: item.platform.id().to_string(),
                id: item.id,
                song: item.song,
                singer: item.singer,
                cover: item.cover,
            })
            .collect(),
    }
}

pub async fn resolve(options: ResolveOptions) -> ApiResult<MusicTrack> {
    let selected_id = options.selected_id.as_deref();
    let quality = options.quality.as_deref().unwrap_or("high");
    match options.platform {
        MusicPlatform::AppleMusic => {
            applemusic::resolve_with_quality(&options.query, selected_id, quality).await
        }
        MusicPlatform::Soda => {
            soda::resolve_with_quality(&options.query, selected_id, quality).await
        }
        MusicPlatform::Netease => {
            netease::resolve_with_quality(&options.query, selected_id, quality).await
        }
        MusicPlatform::Tencent => {
            tencent::resolve_with_quality(&options.query, selected_id, quality).await
        }
        MusicPlatform::Bilibili => {
            bilibili::resolve_with_quality(&options.query, selected_id, quality).await
        }
        platform => {
            provider_for(platform)
                .resolve(&options.query, selected_id)
                .await
        }
    }
}

/// Catalog metadata for one song id. Only platforms with a cheap detail
/// endpoint are supported; none of them resolves audio here.
pub async fn track_info(platform: MusicPlatform, id: &str) -> ApiResult<TrackInfo> {
    let id = id.trim();
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(ApiError::Custom("歌曲 ID 无效".to_string()));
    }
    match platform {
        MusicPlatform::AppleMusic => applemusic::track_info(id).await,
        MusicPlatform::Netease => netease::track_info(id).await,
        MusicPlatform::Tencent => tencent::track_info(id).await,
        MusicPlatform::Soda => soda::track_info(id).await,
        _ => Err(ApiError::Custom("该平台暂不支持分享链接".to_string())),
    }
}

pub async fn download<F>(
    track: &MusicTrack,
    include_cover: bool,
    progress: &mut F,
) -> ApiResult<MusicMedia>
where
    F: FnMut(DownloadProgress) + Send,
{
    download_track_media_with_cover_progress(track, include_cover, progress).await
}

/// Whether the resolved track can be served through the low-latency
/// fragmented-MP4 path while the download worker is still running.
///
/// The default path is intentionally conservative: only AAC is exposed to a
/// native `<audio>` element.  `wasm_alac` is an explicit client capability
/// request and returns decrypted ALAC fMP4 fragments for the WebAssembly
/// decoder path.  Keeping the mode explicit prevents an ALAC
/// task from accidentally being handed to browsers that cannot decode it.
pub fn can_stream_track(track: &MusicTrack) -> bool {
    can_stream_track_with_mode(track, None)
}

pub fn can_stream_track_with_mode(track: &MusicTrack, streaming_mode: Option<&str>) -> bool {
    matches!(track.platform, MusicPlatform::AppleMusic)
        && applemusic::can_stream_track_with_mode(&track.url, streaming_mode)
}

/// Plain CDN audio (NetEase MP3/FLAC) that a native `<audio>` element can play
/// from a growing file. It never uses a packet transport.
pub fn can_stream_progressive_http(track: &MusicTrack) -> bool {
    provider::can_stream_progressive_http(track)
}

/// Mirror `track.url` into a growing file; `on_ready` receives the declared
/// total length once a playable prefix is on disk.
pub async fn stream_http_track_to_path<F>(
    track: &MusicTrack,
    path: &Path,
    on_ready: &mut F,
) -> ApiResult<Vec<u8>>
where
    F: FnMut(Option<u64>) + Send,
{
    provider::stream_http_audio_to_path(track, path, on_ready).await
}

/// Stream a supported track into a growing browser file and return the normal
/// completed audio bytes for the existing cache/remux pipeline.
pub async fn stream_track_to_path<F>(
    track: &MusicTrack,
    path: &Path,
    on_ready: &mut F,
) -> ApiResult<(Vec<u8>, Option<Duration>)>
where
    F: FnMut() + Send,
{
    stream_track_to_path_with_mode(track, path, None, on_ready).await
}

pub async fn stream_track_to_path_with_mode<F>(
    track: &MusicTrack,
    path: &Path,
    streaming_mode: Option<&str>,
    on_ready: &mut F,
) -> ApiResult<(Vec<u8>, Option<Duration>)>
where
    F: FnMut() + Send,
{
    applemusic::stream_wrapper_lite_to_path_with_mode(&track.url, path, streaming_mode, on_ready)
        .await
}

/// Log the NetEase login state and run the opt-in Cookie refresh.
pub fn spawn_netease_session_keeper(data_dir: PathBuf) {
    netease_session::spawn_keeper(data_dir);
}

pub fn wasm_alac_stream_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.alacstream", path.display()))
}

pub fn alac_transport_from_progressive(data: &[u8]) -> ApiResult<Vec<u8>> {
    applemusic::alac_transport_from_progressive(data)
}

pub async fn download_cover(track: &MusicTrack) -> ApiResult<Vec<u8>> {
    if track.cover.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(provider::download_url(&track.cover)
        .await
        .unwrap_or_default())
}

pub async fn lyrics(platform: MusicPlatform, track_id: &str) -> ApiResult<Option<Lyrics>> {
    Ok(provider_for(platform)
        .lyrics(track_id)
        .await?
        .map(Lyrics::from))
}

pub async fn lyrics_for_track(
    platform: MusicPlatform,
    track_id: &str,
    song: &str,
    singer: &str,
    album: &str,
    duration: Option<u32>,
) -> ApiResult<Option<Lyrics>> {
    // A failed or slow native provider must not prevent metadata-based fallback.
    let native = match tokio::time::timeout(
        std::time::Duration::from_secs(8),
        provider_for(platform).lyrics(track_id),
    )
    .await
    {
        Ok(Ok(lyrics)) => lyrics,
        Ok(Err(error)) if song.trim().is_empty() => return Err(error),
        _ => None,
    };
    let completed =
        online_lyrics::complete(platform, track_id, song, singer, album, duration, native).await?;
    Ok(completed.map(Lyrics::from))
}

fn provider_for(platform: MusicPlatform) -> &'static dyn MusicProvider {
    match platform {
        MusicPlatform::AppleMusic => &applemusic::APPLE_MUSIC_PROVIDER,
        MusicPlatform::Bilibili => &bilibili::BILIBILI_PROVIDER,
        MusicPlatform::Kugou => &kugou::KUGOU_PROVIDER,
        MusicPlatform::Netease => &netease::NETEASE_PROVIDER,
        MusicPlatform::Soda => &soda::SODA_PROVIDER,
        MusicPlatform::Tencent => &tencent::TENCENT_PROVIDER,
        MusicPlatform::YouTubeMusic => &youtubemusic::YOUTUBE_MUSIC_PROVIDER,
    }
}

#[cfg(test)]
mod offline_platform_tests {
    use super::*;

    #[test]
    fn soda_is_refused_but_still_known_as_a_source() {
        let ids: Vec<_> = platforms().iter().map(|platform| platform.id).collect();
        assert_eq!(
            ids,
            [
                "applemusic",
                "netease",
                "youtubemusic",
                "bilibili",
                "tencent"
            ]
        );
        assert_eq!(
            parse_platform("kugou").err().map(|error| error.to_string()),
            Some(
                ApiError::Http {
                    status: axum::http::StatusCode::GONE,
                    message: "酷狗音乐已下线".to_string(),
                }
                .to_string()
            )
        );
        match parse_platform("汽水") {
            Err(ApiError::Http { status, message }) => {
                assert_eq!(status, axum::http::StatusCode::GONE);
                assert_eq!(message, "汽水音乐已下线");
            }
            other => panic!("expected 410, got {:?}", other.map(|p| p.id())),
        }
        assert_eq!(parse_any_platform("soda").unwrap(), MusicPlatform::Soda);
        assert_eq!(parse_platform("am").unwrap(), MusicPlatform::AppleMusic);
    }
}

#[cfg(test)]
mod track_info_tests {
    use super::*;

    #[tokio::test]
    async fn malformed_ids_are_rejected_without_a_request() {
        for id in ["", "  ", "1/2", "../x", "a b", &"9".repeat(65)] {
            assert!(
                track_info(MusicPlatform::Netease, id).await.is_err(),
                "{id:?}"
            );
        }
        assert!(track_info(MusicPlatform::Netease, "12a").await.is_err());
        assert!(track_info(MusicPlatform::Bilibili, "123").await.is_err());
    }

    #[tokio::test]
    #[ignore = "live network"]
    async fn live_track_info() {
        let netease = track_info(MusicPlatform::Netease, "65766").await.unwrap();
        println!("{netease:?}");
        assert!(!netease.song.is_empty() && netease.cover.starts_with("http"));
        match track_info(MusicPlatform::Tencent, "0039MnYb0qxYhV").await {
            Ok(info) => println!("{info:?}"),
            Err(error) => println!("tencent: {error}"),
        }
    }
}
