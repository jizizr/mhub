//! Songs users send the bot, kept in the shared Telegram cache for songs
//! MHub's own accounts cannot download (a region-locked Apple Music song,
//! a NetEase song the account has no rights to).
//!
//! The file is fetched back from Telegram and checked against the song it
//! claims to be: a playable codec for the platform, and the platform's
//! length when it lists one. A copy at the same or a better tier already
//! cached wins; otherwise MHub writes lyrics in when the file has none and
//! uploads its own copy, so the entry does not depend on the user's message.

use super::*;
use crate::{
    audio::{self, AudioProbe, Codec},
    cache::{find_music_cache_by_source, upsert_music_cache},
    lyric_tags::{self, LYRICS_TAGGED},
};
use music_gateway::{
    ContributeOutcome, ContributeRequest, ContributeResult, LyricsState, Media, Platform, Quality,
    Track, TrackHint,
};
use std::{collections::HashSet, sync::Mutex};
use tokio::sync::Semaphore;

/// Each contribution holds a whole song in memory.
static SLOTS: Semaphore = Semaphore::const_new(2);
const FILE_SIZE_MAX: u64 = 400 * 1024 * 1024;
/// Shorter than this is a preview clip, whatever the song.
const DURATION_MIN: u32 = 30;
/// AAC and MP3 at or above this are the platforms' "high" tier (256/320).
const HIGH_BITRATE_MIN: u32 = 250_000;

lazy_static::lazy_static! {
    /// Songs being contributed right now, so two users sending the same one
    /// do not both upload it.
    static ref RUNNING: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
}

struct Running(String);

impl Running {
    fn start(key: String) -> Option<Self> {
        let mut running = RUNNING.lock().unwrap_or_else(|poison| poison.into_inner());
        running.insert(key.clone()).then_some(Self(key))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&self.0);
    }
}

fn rejected(message: impl Into<String>) -> ApiError {
    ApiError::Http {
        status: StatusCode::BAD_REQUEST,
        message: message.into(),
    }
}

pub(super) async fn contribute(request: ContributeRequest) -> ApiResult<ContributeResult> {
    let file_id = request.file_id.trim();
    if file_id.is_empty() {
        return Err(rejected("缺少音频文件"));
    }
    let (link, platform, track_id) = request
        .link
        .split_whitespace()
        .find_map(|link| music::parse_track_link(link).map(|(platform, id)| (link, platform, id)))
        .ok_or_else(|| rejected("没有认出歌曲链接，请发送歌曲页面的链接"))?;
    music::ensure_online(platform)?;
    if request.file_size.is_some_and(|size| size > FILE_SIZE_MAX) {
        return Err(rejected("文件太大"));
    }
    let _running = Running::start(format!("{}:{track_id}", platform.id()))
        .ok_or_else(|| rejected("这首歌正在入库，请稍后再试"))?;
    let _slot = SLOTS
        .acquire()
        .await
        .map_err(|_| ApiError::Custom("入库暂不可用".to_string()))?;

    let details = song_details(platform, &track_id, link, &request.hint).await?;
    let audio = telegram_cache::read_file(file_id, request.file_size).await?;
    if audio.len() as u64 > FILE_SIZE_MAX {
        return Err(rejected("文件太大"));
    }
    let probe = audio::probe(&audio)
        .ok_or_else(|| rejected("不是能识别的音频文件，支持 MP3、FLAC 和 M4A"))?;
    check_codec(platform, probe.codec)?;
    let duration = probe
        .duration_ms
        .map(|millis| ((millis + 500) / 1000) as u32)
        .or(request.duration)
        .ok_or_else(|| rejected("读不出文件时长"))?;
    check_duration(duration, details.duration)?;
    let offered = tier(&probe, audio.len() as u64, duration);

    if let Some(existing) = best_cached(platform, &track_id).await?
        && row_quality(&existing).is_some_and(|cached| rank(cached) >= rank(offered))
    {
        let (row, lyrics) = match lyric_upload::ensure_embedded(&existing).await {
            Ok(checked) => checked,
            Err(error) => {
                tracing::warn!(
                    platform = %platform,
                    track_id = %track_id,
                    "checking lyrics of a cached song failed: {}",
                    redact_error_message(&error)
                );
                (existing, LyricsState::Pending)
            }
        };
        return Ok(ContributeResult {
            outcome: ContributeOutcome::AlreadyCached,
            media: media_from_row(platform, &row, &details.cover)?,
            lyrics,
            offered,
        });
    }

    let mut cache = MusicCache {
        platform: platform.id().to_string(),
        track_id: track_id.clone(),
        quality: offered.as_str().to_string(),
        song: details.song.clone(),
        singer: details.singer.clone(),
        album: details.album.clone(),
        link: details.link.clone(),
        file_ext: probe.codec.file_ext().to_string(),
        music_size: audio.len().min(i64::MAX as usize) as i64,
        bitrate: bitrate(audio.len() as u64, duration).min(i32::MAX as u32) as i32,
        duration: Some(duration.min(i32::MAX as u32) as i32),
        file_id: String::new(),
        thumb_file_id: String::new(),
        from_user_id: request.user_id,
        from_chat_id: cache_upload_chat_id(),
        lyrics_tag: 0,
    };

    let (source, audio) = tokio::task::spawn_blocking(move || (audio_sha256(&audio), audio))
        .await
        .map_err(|error| ApiError::Custom(error.to_string()))?;
    // The same bytes were already uploaded, for this song or another ID.
    if let Ok(Some(existing)) = find_music_cache_by_source(platform.id(), &source).await {
        let cache = reuse_upload(cache, &existing);
        upsert_music_cache(&cache, Some(&source)).await?;
        let (row, lyrics) = lyric_upload::ensure_embedded(&cache)
            .await
            .unwrap_or((cache, LyricsState::Pending));
        return Ok(ContributeResult {
            outcome: ContributeOutcome::Added,
            media: media_from_row(platform, &row, &details.cover)?,
            lyrics,
            offered,
        });
    }

    let thumbnail = thumbnail(&details.cover, request.thumb_file_id.as_deref());
    let (prepared, lyrics) = if lyric_tags::has_lyrics(&audio) {
        (
            lyric_upload::PreparedAudio {
                audio,
                lyrics_tag: LYRICS_TAGGED,
                embedded: false,
            },
            LyricsState::Embedded,
        )
    } else {
        let prepared = lyric_upload::prepare(&cache, audio).await;
        let state = match (prepared.embedded, prepared.lyrics_tag) {
            (true, _) => LyricsState::Embedded,
            (false, 0) => LyricsState::Pending,
            (false, _) => LyricsState::Unavailable,
        };
        (prepared, state)
    };
    let thumbnail = thumbnail.await;
    let uploaded_len = prepared.audio.len();
    let uploaded = telegram_cache::upload_audio(
        cache.from_chat_id,
        &cache_audio_filename(&cache),
        &content_type_from_ext(&cache.file_ext),
        prepared.audio,
        thumbnail,
        &cache.song,
        &cache.singer,
        Some(duration),
    )
    .await?;
    cache.file_id = uploaded.file_id;
    cache.thumb_file_id = uploaded.thumb_file_id.unwrap_or_default();
    cache.music_size = uploaded_len.min(i64::MAX as usize) as i64;
    cache.lyrics_tag = prepared.lyrics_tag;
    upsert_music_cache(&cache, Some(&source)).await?;
    tracing::info!(
        platform = %platform,
        track_id = %track_id,
        quality = %cache.quality,
        user_id = request.user_id,
        "added a contributed song to the telegram cache"
    );
    Ok(ContributeResult {
        outcome: ContributeOutcome::Added,
        media: media_from_row(platform, &cache, &details.cover)?,
        lyrics,
        offered,
    })
}

/// The platform's details, or what the forwarded post said when the
/// platform does not answer for this song (which is often why it is being
/// contributed).
async fn song_details(
    platform: Platform,
    track_id: &str,
    link: &str,
    hint: &TrackHint,
) -> ApiResult<music::SongDetails> {
    let linked = || {
        if link.starts_with("http://") || link.starts_with("https://") {
            link.to_string()
        } else {
            String::new()
        }
    };
    match music::song_details(platform, track_id, link).await {
        Ok(mut details) => {
            if details.link.is_empty() {
                details.link = linked();
            }
            Ok(details)
        }
        Err(error) => {
            if hint.song.trim().is_empty() || hint.singer.trim().is_empty() {
                tracing::info!(
                    platform = %platform,
                    track_id,
                    "contributed song not found: {}",
                    redact_error_message(&error)
                );
                return Err(ApiError::Http {
                    status: StatusCode::NOT_FOUND,
                    message: format!(
                        "{}查不到这首歌，请补充歌名和歌手（「歌名」- 歌手）",
                        platform.label()
                    ),
                });
            }
            Ok(music::SongDetails {
                song: hint.song.trim().to_string(),
                singer: hint.singer.trim().to_string(),
                album: hint.album.trim().to_string(),
                cover: String::new(),
                link: linked(),
                duration: None,
            })
        }
    }
}

/// What each platform's cached files may be: the web player streams Apple
/// Music lossless as ALAC, and plays other platforms' M4A natively, which
/// only Safari can for ALAC.
fn check_codec(platform: Platform, codec: Codec) -> ApiResult<()> {
    let allowed = match platform {
        Platform::AppleMusic => matches!(codec, Codec::Alac | Codec::Aac),
        _ => codec != Codec::Alac,
    };
    if allowed {
        return Ok(());
    }
    Err(rejected(match codec {
        Codec::Alac => "这个平台不收 ALAC 文件，请发送 FLAC",
        _ => "Apple Music 歌曲请发送 M4A（ALAC 或 AAC）文件",
    }))
}

fn check_duration(file: u32, listed: Option<u32>) -> ApiResult<()> {
    if file < DURATION_MIN {
        return Err(rejected("文件太短，像是试听片段"));
    }
    let Some(listed) = listed.filter(|listed| *listed > 0) else {
        return Ok(());
    };
    let tolerance = (listed / 50).max(3);
    if file.abs_diff(listed) > tolerance {
        return Err(rejected(format!(
            "文件时长 {} 与歌曲时长 {} 不符，可能不是同一首或版本不同",
            clock(file),
            clock(listed)
        )));
    }
    Ok(())
}

fn clock(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn bitrate(bytes: u64, seconds: u32) -> u32 {
    (bytes.saturating_mul(8) / u64::from(seconds.max(1))).min(u64::from(u32::MAX)) as u32
}

fn tier(probe: &AudioProbe, bytes: u64, seconds: u32) -> Quality {
    if probe.codec.is_lossless() {
        let deep = probe.bits_per_sample.is_some_and(|bits| bits > 16)
            || probe.sample_rate.is_some_and(|rate| rate > 48_000);
        return if deep {
            Quality::HiRes
        } else {
            Quality::Lossless
        };
    }
    if bitrate(bytes, seconds) >= HIGH_BITRATE_MIN {
        Quality::High
    } else {
        Quality::Standard
    }
}

fn rank(quality: Quality) -> usize {
    Quality::ALL
        .iter()
        .position(|tier| *tier == quality)
        .unwrap_or(0)
}

/// The best tier the song is cached at, legacy bot keys included.
async fn best_cached(platform: Platform, track_id: &str) -> ApiResult<Option<MusicCache>> {
    for quality in Quality::ALL.into_iter().rev() {
        for key in gateway::cache_keys(platform, quality.as_str()) {
            if let Some(row) = find_music_cache(platform.id(), track_id, &key).await?
                && !row.file_id.trim().is_empty()
            {
                return Ok(Some(row));
            }
        }
    }
    Ok(None)
}

/// A row's tier, legacy key suffixes (`lossless:tg-m4a-v2`) ignored.
fn row_quality(row: &MusicCache) -> Option<Quality> {
    Quality::parse(row.quality.split(':').next().unwrap_or_default())
}

fn media_from_row(platform: Platform, row: &MusicCache, cover: &str) -> ApiResult<Media> {
    let quality =
        row_quality(row).ok_or_else(|| ApiError::Custom("缓存记录的音质无效".to_string()))?;
    Ok(Media {
        track: Track {
            platform,
            id: row.track_id.clone(),
            song: row.song.clone(),
            singer: row.singer.clone(),
            album: row.album.clone(),
            cover: cover.to_string(),
            link: row.link.clone(),
            duration: row.duration.map(|duration| duration.max(0) as u32),
            bitrate: (row.bitrate > 0).then_some(row.bitrate as u32),
            format: Some(row.file_ext.clone()).filter(|ext| !ext.trim().is_empty()),
        },
        quality,
        file_id: row.file_id.clone(),
        thumb_file_id: Some(row.thumb_file_id.clone()).filter(|id| !id.trim().is_empty()),
        byte_size: row.music_size.max(0) as u64,
        decrypt_elapsed_ms: None,
        cached: true,
    })
}

/// The platform's cover when it has one, else the thumbnail the user's
/// file came with.
async fn thumbnail(cover: &str, thumb_file_id: Option<&str>) -> Option<Vec<u8>> {
    let mut image = music::download_cover_url(cover).await;
    if image.is_empty()
        && let Some(thumb) = thumb_file_id.map(str::trim).filter(|id| !id.is_empty())
    {
        image = telegram_cache::read_file(thumb, None)
            .await
            .unwrap_or_default();
    }
    if image.is_empty() {
        return None;
    }
    tokio::task::spawn_blocking(move || telegram_cache::audio_thumbnail(&image))
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(codec: Codec, bits: Option<u16>, rate: Option<u32>) -> AudioProbe {
        AudioProbe {
            codec,
            sample_rate: rate,
            bits_per_sample: bits,
            duration_ms: None,
        }
    }

    #[test]
    fn tiers_follow_codec_depth_and_bitrate() {
        let mb = 1024 * 1024;
        assert_eq!(
            tier(&probe(Codec::Alac, Some(16), Some(44_100)), 22 * mb, 216),
            Quality::Lossless
        );
        assert_eq!(
            tier(&probe(Codec::Flac, Some(24), Some(96_000)), 90 * mb, 216),
            Quality::HiRes
        );
        assert_eq!(
            tier(&probe(Codec::Aac, None, None), 256_000 / 8 * 216, 216),
            Quality::High
        );
        assert_eq!(
            tier(&probe(Codec::Mp3, None, None), 128_000 / 8 * 216, 216),
            Quality::Standard
        );
    }

    #[test]
    fn durations_must_match_the_listed_length() {
        assert!(check_duration(20, None).is_err());
        assert!(check_duration(216, None).is_ok());
        assert!(check_duration(218, Some(216)).is_ok());
        assert!(check_duration(250, Some(216)).is_err());
        // 2% of a long track.
        assert!(check_duration(605, Some(600)).is_ok());
    }

    #[test]
    fn codecs_fit_how_each_platform_is_played() {
        assert!(check_codec(Platform::AppleMusic, Codec::Alac).is_ok());
        assert!(check_codec(Platform::AppleMusic, Codec::Flac).is_err());
        assert!(check_codec(Platform::Netease, Codec::Flac).is_ok());
        assert!(check_codec(Platform::Netease, Codec::Alac).is_err());
    }
}
