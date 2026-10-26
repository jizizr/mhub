#![allow(dead_code)]

mod applemusic;
mod bilibili;
mod kugou;
mod netease;
mod online_lyrics;
mod provider;
mod qrc_crypto;
mod soda;
pub(crate) mod streaming;
mod tencent;
mod youtubemusic;

use crate::{ApiError, ApiResult};
use provider::{
    MusicCollection, MusicMedia, MusicPlatform, MusicProvider,
    download_track_media_with_cover_progress,
};
pub use provider::{MusicSearchItem, MusicTrack};
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

pub fn platforms() -> Vec<PlatformInfo> {
    vec![
        PlatformInfo {
            id: "applemusic",
            label: "Apple Music",
            aliases: &[
                "am",
                "apple",
                "applemusic",
                "apple-music",
                "apple_music",
                "苹果音乐",
            ],
        },
        PlatformInfo {
            id: "bilibili",
            label: "Bilibili",
            aliases: &["b", "bili", "bilibili", "哔哩哔哩", "b站"],
        },
        PlatformInfo {
            id: "kugou",
            label: "酷狗音乐",
            aliases: &["kg", "kugou", "酷狗", "酷狗音乐"],
        },
        PlatformInfo {
            id: "netease",
            label: "网易云音乐",
            aliases: &["163", "netease", "wyy", "网易", "网易云", "网易云音乐"],
        },
        PlatformInfo {
            id: "soda",
            label: "汽水音乐",
            aliases: &["qs", "soda", "qishui", "汽水", "汽水音乐"],
        },
        PlatformInfo {
            id: "tencent",
            label: "QQ音乐",
            aliases: &[
                "qq",
                "qqmusic",
                "tencent",
                "tx",
                "qq音乐",
                "腾讯",
                "腾讯音乐",
            ],
        },
        PlatformInfo {
            id: "youtubemusic",
            label: "YouTube Music",
            aliases: &[
                "yt",
                "ytm",
                "youtube",
                "youtubemusic",
                "youtube-music",
                "youtube_music",
            ],
        },
    ]
}

pub fn parse_platform(value: &str) -> ApiResult<MusicPlatform> {
    MusicPlatform::from_alias(value)
        .ok_or_else(|| ApiError::Custom(format!("暂不支持音乐平台：{value}")))
}

pub async fn search(
    platform: MusicPlatform,
    keyword: &str,
    limit: usize,
) -> ApiResult<Vec<MusicSearchItem>> {
    provider_for(platform).search(keyword, limit).await
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

pub async fn collection(
    platform: MusicPlatform,
    keyword: &str,
    limit: usize,
) -> ApiResult<Collection> {
    let collection = provider_for(platform)
        .collection(keyword, limit.clamp(1, MAX_COLLECTION_ITEMS + 1))
        .await?
        .ok_or_else(|| ApiError::Custom("该平台暂不支持此歌单链接".to_string()))?;
    Ok(collection_to_api(collection))
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
        platform => {
            provider_for(platform)
                .resolve(&options.query, selected_id)
                .await
        }
    }
}

pub async fn download(track: &MusicTrack, include_cover: bool) -> ApiResult<MusicMedia> {
    let mut progress = |_| {};
    download_track_media_with_cover_progress(track, include_cover, &mut progress).await
}

/// Whether the resolved track can be served through the low-latency
/// fragmented-MP4 path while the download worker is still running.
///
/// The default path is intentionally conservative: only AAC is exposed to a
/// native `<audio>` element.  `wasm_alac` is an explicit client capability
/// request and returns decrypted ALAC fMP4 fragments for the WebAssembly
/// decoder/AudioWorklet path.  Keeping the mode explicit prevents an ALAC
/// task from accidentally being handed to browsers that cannot decode it.
pub fn can_stream_track(track: &MusicTrack) -> bool {
    can_stream_track_with_mode(track, None)
}

pub fn can_stream_track_with_mode(track: &MusicTrack, streaming_mode: Option<&str>) -> bool {
    matches!(track.platform, MusicPlatform::AppleMusic)
        && applemusic::can_stream_track_with_mode(&track.url, streaming_mode)
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
        .map(|lyrics| Lyrics {
            plain: lyrics.plain,
            translation: lyrics.translation,
        }))
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
    Ok(completed.map(|lyrics| Lyrics {
        plain: lyrics.plain,
        translation: lyrics.translation,
    }))
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
