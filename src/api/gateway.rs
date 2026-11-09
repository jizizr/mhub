//! Loopback gateway lending MHub's sources, accounts and Telegram cache to
//! other local services such as the Telegram bot. The contract lives in the
//! `music-gateway` crate. Media requests run through the same task, inflight
//! and upload pipeline as the web player, so a song requested by both is
//! downloaded and uploaded once, with MHub as the only writer of
//! `music_cache`.

use super::*;
use crate::cache::{delete_music_cache_file, find_music_cache_by_file_id};
use axum::extract::rejection::JsonRejection;
use music_gateway::{
    CacheInvalidateRequest, CacheLookupRequest, CachedTrack, Capabilities, Collection,
    CollectionRequest, ErrorBody, ErrorCode, JobState, Lyrics, LyricsRequest, Media, MediaJob,
    MediaLookup, MediaPollQuery, MediaRequest, PROTOCOL_HEADER, PROTOCOL_VERSION, Platform,
    PlatformCapability, Quality, ResolveRequest, SearchItem, SearchRequest, Track, protocol::paths,
};
use sha2::{Digest, Sha256};

const POLL_WAIT_MAX: Duration = Duration::from_secs(25);
const SEARCH_LIMIT_MAX: usize = 50;
/// The upload that follows a download, plus one retry from the local file.
const UPLOAD_ATTEMPTS_MAX: u8 = 2;
/// Before the gateway the bot re-wrapped Apple Music files itself and stored
/// them as `<quality>:tg-m4a-v2`. Those standard M4A uploads stay sendable,
/// so bot lookups keep using them instead of downloading the songs again; the
/// web player never reads them.
const LEGACY_BOT_APPLE_SUFFIX: &str = ":tg-m4a-v2";

#[derive(Clone)]
struct Gateway {
    state: AppState,
    token_digest: [u8; 32],
}

pub(super) fn router(state: AppState, token: &str) -> Router {
    let gateway = Gateway {
        state,
        token_digest: digest(token),
    };
    Router::new()
        .route(paths::CAPABILITIES, get(capabilities))
        .route(paths::SEARCH, post(search))
        .route(paths::COLLECTION, post(collection))
        .route(paths::LYRICS, post(lyrics))
        .route(paths::RESOLVE, post(resolve))
        .route(paths::MEDIA, post(start_media))
        .route(paths::MEDIA_LOOKUP, post(lookup_media))
        .route(&format!("{}/:id", paths::MEDIA), get(poll_media))
        .route(paths::CACHE_LOOKUP, post(lookup_cache))
        .route(paths::CACHE_INVALIDATE, post(invalidate_cache))
        .route_layer(middleware::from_fn_with_state(gateway.clone(), authorize))
        .fallback(|| async {
            Failure::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, "接口不存在")
        })
        .with_state(gateway)
        .layer(TraceLayer::new_for_http())
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Comparing digests rather than the secrets keeps the comparison time
/// independent of how much of a guessed token is right.
async fn authorize(State(gateway): State<Gateway>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| digest(token.trim()) == gateway.token_digest);
    if !authorized {
        return Failure::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
            "音乐网关令牌无效",
        )
        .into_response();
    }
    let protocol = headers
        .get(PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u32>().ok());
    if protocol != Some(PROTOCOL_VERSION) {
        let client = protocol.map_or_else(|| "未声明".to_string(), |version| version.to_string());
        return Failure::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::ProtocolMismatch,
            format!("音乐网关协议版本不一致：服务端 {PROTOCOL_VERSION}，客户端 {client}"),
        )
        .into_response();
    }
    next.run(request).await
}

async fn capabilities() -> Json<Capabilities> {
    Json(Capabilities {
        protocol: PROTOCOL_VERSION,
        server: concat!("mhub/", env!("CARGO_PKG_VERSION")).to_string(),
        qualities: Quality::ALL.to_vec(),
        platforms: Platform::online()
            .map(|platform| PlatformCapability {
                platform,
                label: platform.label().to_string(),
                lyrics: platform.supports_lyrics(),
                collections: platform.supports_collections(),
                account_configured: account_configured(platform),
            })
            .collect(),
    })
}

fn account_configured(platform: Platform) -> Option<bool> {
    let music = &SETTINGS.music;
    let set = |value: &str| !value.trim().is_empty();
    match platform {
        Platform::AppleMusic => {
            let apple = &music.applemusic;
            Some(
                apple.enabled
                    && (set(&apple.wrapper_lite_url)
                        || apple.wrapper_lite_urls.iter().any(|url| set(url))
                        || set(&apple.wrapper_host)
                        || apple.wrapper_hosts.iter().any(|host| set(host))
                        || set(&apple.wv_device)),
            )
        }
        Platform::Netease => Some(set(&music.netease.cookie)),
        Platform::Tencent => Some(set(&music.tencent.cookie)),
        Platform::Soda => Some(set(&music.soda.cookie)),
        Platform::Bilibili | Platform::Kugou | Platform::YouTubeMusic => None,
    }
}

async fn search(
    payload: Result<Json<SearchRequest>, JsonRejection>,
) -> Result<Json<Vec<SearchItem>>, Failure> {
    let Json(request) = payload?;
    music::ensure_online(request.platform)?;
    let keyword = required(&request.keyword, "keyword")?;
    let limit = request.limit.clamp(1, SEARCH_LIMIT_MAX);
    Ok(Json(music::search(request.platform, keyword, limit).await?))
}

async fn collection(
    payload: Result<Json<CollectionRequest>, JsonRejection>,
) -> Result<Json<Option<Collection>>, Failure> {
    let Json(request) = payload?;
    music::ensure_online(request.platform)?;
    let keyword = required(&request.keyword, "keyword")?;
    let limit = request.limit.clamp(1, music::MAX_COLLECTION_ITEMS);
    Ok(Json(
        music::provider_collection(request.platform, keyword, limit).await?,
    ))
}

async fn lyrics(
    payload: Result<Json<LyricsRequest>, JsonRejection>,
) -> Result<Json<Option<Lyrics>>, Failure> {
    let Json(request) = payload?;
    music::ensure_online(request.platform)?;
    let track_id = required(&request.track_id, "track_id")?;
    let lyrics = music::lyrics(request.platform, track_id).await?;
    Ok(Json(lyrics.map(|lyrics| Lyrics {
        plain: lyrics.plain,
        translation: lyrics.translation,
        roman: lyrics.roman,
        credits: lyrics.credits,
    })))
}

async fn resolve(
    payload: Result<Json<ResolveRequest>, JsonRejection>,
) -> Result<Json<Track>, Failure> {
    let Json(request) = payload?;
    music::ensure_online(request.platform)?;
    let keyword = required(&request.keyword, "keyword")?;
    let track = music::resolve(ResolveOptions {
        platform: request.platform,
        query: keyword.to_string(),
        selected_id: selected_id(request.selected_id),
        quality: Some(request.quality.as_str().to_string()),
    })
    .await?;
    Ok(Json(track.gateway_track()))
}

async fn lookup_media(
    State(gateway): State<Gateway>,
    payload: Result<Json<MediaRequest>, JsonRejection>,
) -> Result<Json<MediaLookup>, Failure> {
    let Json(request) = payload?;
    music::ensure_online(request.platform)?;
    let keyword = required(&request.keyword, "keyword")?;
    let quality = request.quality.as_str();
    let (track_id, resolved) = match selected_id(request.selected_id) {
        Some(id) => (id, None),
        None => {
            let track = music::resolve(ResolveOptions {
                platform: request.platform,
                query: keyword.to_string(),
                selected_id: None,
                quality: Some(quality.to_string()),
            })
            .await?;
            (track.id.clone(), Some(track.gateway_track()))
        }
    };
    let id = playback_id(request.platform.id(), &track_id, quality);
    let live = gateway.state.downloads.read().await.get(&id).cloned();
    let live = live.filter(|task| {
        task.status == DownloadStatus::Completed
            && task
                .file_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty())
    });
    let mut row = None;
    for key in cache_keys(request.platform, quality) {
        match find_music_cache(request.platform.id(), &track_id, &key).await {
            Ok(Some(cache)) if !cache.file_id.trim().is_empty() => {
                row = Some(cache);
                break;
            }
            Ok(_) => {}
            // Starting the job checks again; a lookup is only an optimization.
            Err(error) => {
                tracing::warn!(playback_id = %id, "gateway cache lookup failed: {error}");
                break;
            }
        }
    }
    // An upload from before lyrics were embedded gets them, before it is
    // sent when the client asks to wait and it does not take too long.
    if let Some(cache) = &row
        && let Some(updated) = lyric_upload::refresh_stale(cache, request.wait_for_lyrics).await
    {
        row = Some(updated);
    }
    let from_row = |cache: MusicCache| {
        let mut task = task_from_cache(&id, request.platform.id(), &track_id, quality, &cache);
        task.cover_url = resolved.as_ref().map(|track| track.cover.clone());
        task
    };
    // A live generation keeps its own file, unless the row has since moved
    // on to a copy with lyrics written in.
    let cached = match (live, row) {
        (Some(task), Some(cache))
            if lyric_upload::is_tagged(&cache)
                && task.file_id.as_deref() != Some(cache.file_id.as_str()) =>
        {
            Some(from_row(cache))
        }
        (Some(task), _) => Some(task),
        (None, Some(cache)) => Some(from_row(cache)),
        (None, None) => None,
    };
    let media = cached.and_then(|task| {
        let file_id = task.file_id.clone()?;
        task_media(&task, file_id, task.thumb_file_id.clone(), true)
    });
    Ok(Json(MediaLookup {
        track: media.as_ref().map(|media| media.track.clone()).or(resolved),
        media,
    }))
}

async fn start_media(
    State(gateway): State<Gateway>,
    payload: Result<Json<MediaRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<MediaJob>), Failure> {
    let Json(request) = payload?;
    music::ensure_online(request.platform)?;
    let keyword = required(&request.keyword, "keyword")?;
    let task = start_download(
        &gateway.state,
        DownloadRequest {
            platform: request.platform.id().to_string(),
            query: keyword.to_string(),
            selected_id: selected_id(request.selected_id),
            quality: Some(request.quality.as_str().to_string()),
            include_cover: request.include_cover,
            streaming_mode: None,
        },
    )
    .await?;
    // A file that is already in Telegram at this point predates the request.
    let cached = task.status == DownloadStatus::Completed && task.file_id.is_some();
    let state = job_state(&gateway.state, &task, cached).await;
    let status = if state.is_final() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((
        status,
        Json(MediaJob {
            track: task_track(&task),
            id: task.id,
            state,
        }),
    ))
}

async fn poll_media(
    State(gateway): State<Gateway>,
    Path(id): Path<String>,
    Query(query): Query<MediaPollQuery>,
) -> Result<Json<MediaJob>, Failure> {
    let wait = Duration::from_millis(query.wait_ms.unwrap_or(0)).min(POLL_WAIT_MAX);
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        // Register before reading so a transition in between is not missed.
        let changed = gateway.state.task_events.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let task = load_task(&gateway.state, &id).await?;
        let state = job_state(&gateway.state, &task, false).await;
        let now = tokio::time::Instant::now();
        if state.is_final() || query.since.as_deref() != Some(state.phase()) || now >= deadline {
            return Ok(Json(MediaJob {
                track: task_track(&task),
                id,
                state,
            }));
        }
        let _ = tokio::time::timeout((deadline - now).min(PLAYBACK_WAIT_RECHECK), changed).await;
    }
}

async fn lookup_cache(
    payload: Result<Json<CacheLookupRequest>, JsonRejection>,
) -> Result<Json<Option<CachedTrack>>, Failure> {
    let Json(request) = payload?;
    let file_id = required(&request.file_id, "file_id")?;
    let Some(cache) = find_music_cache_by_file_id(file_id).await? else {
        return Ok(Json(None));
    };
    let Some(platform) = Platform::from_alias(&cache.platform) else {
        return Ok(Json(None));
    };
    Ok(Json(Some(CachedTrack {
        track: Track {
            platform,
            id: cache.track_id,
            song: cache.song,
            singer: cache.singer,
            album: cache.album,
            cover: String::new(),
            link: cache.link,
            duration: cache.duration.map(|duration| duration.max(0) as u32),
            bitrate: (cache.bitrate > 0).then_some(cache.bitrate as u32),
            format: Some(cache.file_ext).filter(|ext| !ext.trim().is_empty()),
        },
        quality: cache.quality,
        byte_size: cache.music_size.max(0) as u64,
    })))
}

/// Called when Telegram rejects a cached `file_id`. Besides the row, the live
/// task forgets it, so the next request re-uploads the local copy or, when
/// that is gone too, downloads the song again.
async fn invalidate_cache(
    State(gateway): State<Gateway>,
    payload: Result<Json<CacheInvalidateRequest>, JsonRejection>,
) -> Result<Json<bool>, Failure> {
    let Json(request) = payload?;
    let track_id = required(&request.track_id, "track_id")?;
    let file_id = required(&request.file_id, "file_id")?;
    let platform = request.platform.id();
    let quality = request.quality.as_str();
    // Every quality and track ID sharing the upload loses it, or a new
    // download of the same audio would be matched back to the dead file.
    let deleted = delete_music_cache_file(platform, file_id).await?;
    let id = playback_id(platform, track_id, quality);
    if let Some(task) = gateway.state.downloads.write().await.get_mut(&id)
        && task.file_id.as_deref() == Some(file_id)
    {
        task.file_id = None;
        task.thumb_file_id = None;
        task.upload_attempts = 0;
        task.upload_error = None;
    }
    tracing::warn!(playback_id = %id, deleted, "gateway client reported an unusable Telegram file");
    Ok(Json(deleted))
}

/// `music_cache` quality keys a gateway client may have a file under, the
/// shared one first.
fn cache_keys(platform: Platform, quality: &str) -> Vec<String> {
    let mut keys = vec![quality.to_string()];
    if platform == Platform::AppleMusic {
        keys.push(format!("{quality}{LEGACY_BOT_APPLE_SUFFIX}"));
    }
    keys
}

async fn job_state(state: &AppState, task: &DownloadTask, cached: bool) -> JobState {
    match task.status {
        DownloadStatus::Queued => JobState::Queued,
        DownloadStatus::Processing | DownloadStatus::Streaming => {
            let (written, total) = download_progress(state, task).await;
            JobState::Downloading { written, total }
        }
        DownloadStatus::Failed => JobState::Failed {
            error: ErrorBody::new(
                ErrorCode::Upstream,
                task.error
                    .as_deref()
                    .map(redact_error_message)
                    .unwrap_or_else(|| "下载失败".to_string()),
            ),
        },
        DownloadStatus::Completed => completed_job_state(state, task, cached).await,
    }
}

async fn download_progress(state: &AppState, task: &DownloadTask) -> (Option<u64>, Option<u64>) {
    let (written, total) = task.transfer.snapshot();
    if written > 0 {
        return (Some(written), total);
    }
    // Streaming downloads write straight into a growing file.
    let Some(filename) = task.filename.as_deref() else {
        return (None, None);
    };
    let path = audio_work_dir(state, &task.work_id).join(filename);
    let written = fs::metadata(path).await.ok().map(|metadata| metadata.len());
    (written, task.stream_total)
}

async fn completed_job_state(state: &AppState, task: &DownloadTask, cached: bool) -> JobState {
    if let Some(file_id) = task.file_id.clone().filter(|id| !id.trim().is_empty()) {
        return ready(task, file_id, task.thumb_file_id.clone(), cached);
    }
    let Some(track_id) = task.track_id.as_deref() else {
        return failed(ErrorCode::Upstream, "无法确定平台歌曲 ID");
    };
    // The upload belongs to whichever generation downloaded the song, which
    // is not always this task (a joined inflight download, for example).
    match find_music_cache(&task.platform, track_id, &task.quality).await {
        Ok(Some(cache)) if !cache.file_id.trim().is_empty() => {
            let thumb = Some(cache.thumb_file_id).filter(|id| !id.trim().is_empty());
            return ready(task, cache.file_id, thumb, cached);
        }
        Ok(_) => {}
        // The task's own upload result still decides below.
        Err(error) => {
            tracing::warn!(playback_id = %task.id, "gateway cache lookup failed: {error}")
        }
    }
    if find_inflight(state, &task.platform, track_id, &task.quality)
        .await
        .is_some()
    {
        return JobState::Uploading;
    }
    if task.upload_attempts >= UPLOAD_ATTEMPTS_MAX {
        let reason = task.upload_error.as_deref().unwrap_or("未知错误");
        return failed(
            ErrorCode::Unavailable,
            format!("上传到 Telegram 失败：{reason}"),
        );
    }
    match retry_upload(state, task, track_id).await {
        Ok(()) => JobState::Uploading,
        Err(error) => JobState::Failed {
            error: Failure::from(error).body,
        },
    }
}

/// Uploads the generation's local file again after its upload failed or was
/// invalidated. Without a local file the task fails, so the next request
/// starts a fresh download.
async fn retry_upload(state: &AppState, task: &DownloadTask, track_id: &str) -> ApiResult<()> {
    let dir = audio_work_dir(state, &task.work_id);
    let local = match task.filename.as_deref() {
        Some(filename) if !filename.ends_with(".streaming") => fs::read(dir.join(filename))
            .await
            .ok()
            .map(|audio| (filename, audio)),
        _ => None,
    };
    let Some((filename, audio)) = local else {
        let message = "本地音频已清理，请重新请求";
        if let Some(current) = state.downloads.write().await.get_mut(&task.id)
            && current.work_id == task.work_id
        {
            current.status = DownloadStatus::Failed;
            current.error = Some(message.to_string());
        }
        notify_task_change(state);
        return Err(ApiError::Custom(message.to_string()));
    };
    let key = inflight_key(&task.platform, track_id, &task.quality);
    let (inflight, owner) = register_inflight(state, key.clone()).await;
    if !owner {
        return Ok(());
    }
    mark_inflight_ready(&inflight, task.clone()).await;
    let cover = match task.cover_filename.as_deref() {
        Some(cover) => fs::read(dir.join(cover)).await.unwrap_or_default(),
        None => Vec::new(),
    };
    let chat_id = cache_upload_chat_id();
    let file_ext = task
        .format
        .clone()
        .unwrap_or_else(|| filename.rsplit('.').next().unwrap_or("mp3").to_string());
    let cache = MusicCache {
        platform: task.platform.clone(),
        track_id: track_id.to_string(),
        quality: task.quality.clone(),
        song: task.title.clone().unwrap_or_default(),
        singer: task.artist.clone().unwrap_or_default(),
        album: task.album.clone().unwrap_or_default(),
        link: task.source_url.clone().unwrap_or_default(),
        file_ext,
        music_size: audio.len().min(i64::MAX as usize) as i64,
        bitrate: task
            .bitrate
            .or_else(|| estimate_bitrate(audio.len() as u64, task.duration))
            .unwrap_or_default()
            .min(i32::MAX as u32) as i32,
        duration: task
            .duration
            .map(|duration| duration.min(i32::MAX as u32) as i32),
        file_id: String::new(),
        thumb_file_id: String::new(),
        from_user_id: SETTINGS.bot.owner,
        from_chat_id: chat_id,
        lyrics_tag: 0,
    };
    record_upload_attempt(state, &task.id, &task.work_id).await;
    tracing::info!(playback_id = %task.id, "gateway retrying Telegram cache upload");
    tokio::spawn(upload_audio_to_telegram_cache(TelegramCacheUpload {
        state: state.clone(),
        task_id: task.id.clone(),
        work_id: task.work_id.clone(),
        inflight_key: key,
        chat_id,
        filename: filename.to_string(),
        content_type: content_type(&dir.join(filename)),
        audio,
        cover,
        title: cache.song.clone(),
        performer: cache.singer.clone(),
        duration: task.duration,
        cache,
    }));
    Ok(())
}

fn ready(
    task: &DownloadTask,
    file_id: String,
    thumb_file_id: Option<String>,
    cached: bool,
) -> JobState {
    match task_media(task, file_id, thumb_file_id, cached) {
        Some(media) => JobState::Ready {
            media: Box::new(media),
        },
        None => failed(ErrorCode::Upstream, "未知的音乐平台"),
    }
}

fn task_media(
    task: &DownloadTask,
    file_id: String,
    thumb_file_id: Option<String>,
    cached: bool,
) -> Option<Media> {
    Some(Media {
        track: task_track(task)?,
        quality: Quality::parse(&task.quality).unwrap_or(Quality::High),
        file_id,
        thumb_file_id,
        byte_size: task.byte_size.unwrap_or_default(),
        decrypt_elapsed_ms: task
            .decrypt_elapsed_seconds
            .map(|seconds| (seconds * 1000.0).round() as u64),
        cached,
    })
}

/// The task's song once it is known, that is, after it was resolved.
fn task_track(task: &DownloadTask) -> Option<Track> {
    let platform = Platform::from_alias(&task.platform)?;
    let song = task.title.clone()?;
    let format = task.format.clone().or_else(|| {
        task.filename
            .as_deref()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, ext)| ext.to_string())
    });
    Some(Track {
        platform,
        id: task.track_id.clone().unwrap_or_default(),
        song,
        singer: task.artist.clone().unwrap_or_default(),
        album: task.album.clone().unwrap_or_default(),
        cover: task.cover_url.clone().unwrap_or_default(),
        link: task.source_url.clone().unwrap_or_default(),
        duration: task.duration,
        bitrate: task.bitrate,
        format,
    })
}

fn failed(code: ErrorCode, message: impl Into<String>) -> JobState {
    JobState::Failed {
        error: ErrorBody::new(code, message),
    }
}

fn required<'a>(value: &'a str, field: &str) -> Result<&'a str, Failure> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Failure::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::BadRequest,
            format!("{field} 不能为空"),
        ));
    }
    Ok(value)
}

fn selected_id(value: Option<String>) -> Option<String> {
    value
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

/// A gateway error response: always an `ErrorBody`, never a signed URL.
#[derive(Debug)]
struct Failure {
    status: StatusCode,
    body: ErrorBody,
}

impl Failure {
    fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody::new(code, message),
        }
    }
}

impl From<ApiError> for Failure {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::RateLimited {
                message,
                retry_after,
            } => Self {
                status: StatusCode::TOO_MANY_REQUESTS,
                body: ErrorBody {
                    code: ErrorCode::RateLimited,
                    message: redact_error_message(&message),
                    retry_after_ms: retry_after.map(|wait| wait.as_millis() as u64),
                },
            },
            ApiError::Http { status, message } => {
                let code = match status {
                    StatusCode::NOT_FOUND => ErrorCode::NotFound,
                    status if status.is_client_error() => ErrorCode::BadRequest,
                    _ => ErrorCode::Upstream,
                };
                Self::new(status, code, redact_error_message(&message))
            }
            // Provider failures are written for users ("未找到歌曲" …).
            ApiError::Custom(message) | ApiError::Unplayable(message) => Self::new(
                StatusCode::BAD_GATEWAY,
                ErrorCode::Upstream,
                redact_error_message(&message),
            ),
            ApiError::Request(_) | ApiError::Retry(_) => {
                tracing::warn!(
                    "gateway upstream request failed: {}",
                    redact_error_message(&error)
                );
                Self::new(
                    StatusCode::BAD_GATEWAY,
                    ErrorCode::Upstream,
                    "音乐平台请求失败，请稍后再试",
                )
            }
            error => {
                tracing::warn!("gateway internal error: {}", redact_error_message(&error));
                Self::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    ErrorCode::Unavailable,
                    "音乐服务内部错误，请稍后再试",
                )
            }
        }
    }
}

impl From<JsonRejection> for Failure {
    fn from(rejection: JsonRejection) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::BadRequest,
            format!("请求格式错误：{}", rejection.body_text()),
        )
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CACHE_CONTROL, "no-store")],
            Json(self.body),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_gateway::{GatewayClient, GatewayError};

    const TOKEN: &str = "test-token-0123456789abcdef0123456789";

    fn test_state(directory: &FsPath) -> AppState {
        AppState {
            downloads: Arc::new(RwLock::new(HashMap::new())),
            task_events: Arc::new(Notify::new()),
            accounts: None,
            inflight: Arc::new(RwLock::new(HashMap::new())),
            data_dir: directory.to_path_buf(),
            media_base_url: None,
        }
    }

    async fn serve(state: AppState) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(state, TOKEN)).await.unwrap();
        });
        format!("http://{address}")
    }

    fn task(id: &str, status: DownloadStatus) -> DownloadTask {
        let cache = MusicCache {
            platform: "netease".into(),
            track_id: id.into(),
            quality: "lossless".into(),
            song: "晴天".into(),
            singer: "周杰伦".into(),
            album: "叶惠美".into(),
            link: "https://music.163.com/song?id=1".into(),
            file_ext: "flac".into(),
            music_size: 1024,
            bitrate: 900_000,
            duration: Some(269),
            file_id: "telegram-file".into(),
            thumb_file_id: "telegram-thumb".into(),
            from_user_id: 0,
            from_chat_id: 0,
            lyrics_tag: 0,
        };
        let playback = playback_id("netease", id, "lossless");
        let mut task = task_from_cache(&playback, "netease", id, "lossless", &cache);
        task.status = status;
        task.cover_url = Some("https://example.test/cover.jpg".into());
        task
    }

    async fn insert(state: &AppState, task: &DownloadTask) {
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());
    }

    #[tokio::test]
    async fn rejects_wrong_tokens_and_protocol_versions() {
        let directory = tempfile::tempdir().unwrap();
        let base = serve(test_state(directory.path())).await;

        let wrong = GatewayClient::new(&base, "not-the-token").unwrap();
        let error = wrong.capabilities().await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::Unauthorized);

        let response = reqwest::Client::new()
            .get(format!("{base}{}", paths::CAPABILITIES))
            .bearer_auth(TOKEN)
            .header(PROTOCOL_HEADER, "999")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: ErrorBody = response.json().await.unwrap();
        assert_eq!(body.code, ErrorCode::ProtocolMismatch);

        let client = GatewayClient::new(&base, TOKEN).unwrap();
        let capabilities = client.capabilities().await.unwrap();
        assert_eq!(capabilities.protocol, PROTOCOL_VERSION);
        assert_eq!(capabilities.platforms.len(), Platform::online().count());
        assert!(
            capabilities
                .platforms
                .iter()
                .all(|platform| !platform.platform.is_offline())
        );
        let bilibili = capabilities
            .platforms
            .iter()
            .find(|platform| platform.platform == Platform::Bilibili)
            .unwrap();
        assert!(!bilibili.lyrics && bilibili.account_configured.is_none());
    }

    #[tokio::test]
    async fn completed_tasks_are_ready_with_their_telegram_file() {
        let directory = tempfile::tempdir().unwrap();
        let state = test_state(directory.path());
        let done = task("gateway-ready", DownloadStatus::Completed);
        insert(&state, &done).await;
        let client = GatewayClient::new(&serve(state).await, TOKEN).unwrap();

        let job = client
            .poll_media(&done.id, None, Duration::ZERO)
            .await
            .unwrap();
        let JobState::Ready { media } = job.state else {
            panic!("expected ready, got {:?}", job.state);
        };
        assert_eq!(media.file_id, "telegram-file");
        assert_eq!(media.thumb_file_id.as_deref(), Some("telegram-thumb"));
        assert_eq!(media.quality, Quality::Lossless);
        assert_eq!(media.track.file_name(), "晴天 - 周杰伦.flac");
        assert_eq!(media.track.cover, "https://example.test/cover.jpg");
        assert_eq!(media.byte_size, 1024);
        assert!(!media.cached);
    }

    #[tokio::test]
    async fn downloads_report_bytes_and_long_polls_wait_for_a_change() {
        let directory = tempfile::tempdir().unwrap();
        let state = test_state(directory.path());
        let mut running = task("gateway-running", DownloadStatus::Processing);
        running.file_id = None;
        running.transfer.record(music::DownloadProgress {
            written: 300,
            total: Some(1000),
        });
        insert(&state, &running).await;
        let client = GatewayClient::new(&serve(state.clone()).await, TOKEN).unwrap();

        let started = tokio::time::Instant::now();
        let job = client
            .poll_media(&running.id, Some("downloading"), Duration::from_millis(300))
            .await
            .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert_eq!(
            job.state,
            JobState::Downloading {
                written: Some(300),
                total: Some(1000)
            }
        );

        // A status change wakes the poll before its deadline.
        let waker = state.clone();
        let id = running.id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(task) = waker.downloads.write().await.get_mut(&id) {
                task.status = DownloadStatus::Failed;
                task.error = Some("会员歌曲 https://cdn.example.test/a.flac?sig=secret".into());
            }
            notify_task_change(&waker);
        });
        let started = tokio::time::Instant::now();
        let job = client
            .poll_media(&running.id, Some("downloading"), Duration::from_secs(10))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        let JobState::Failed { error } = job.state else {
            panic!("expected failure");
        };
        assert!(error.message.starts_with("会员歌曲"));
        assert!(!error.message.contains("secret"));
    }

    #[tokio::test]
    async fn completed_downloads_wait_for_their_upload_then_retry_once_from_disk() {
        let directory = tempfile::tempdir().unwrap();
        let state = test_state(directory.path());
        let mut done = task("gateway-upload", DownloadStatus::Completed);
        done.file_id = None;
        done.thumb_file_id = None;
        done.filename = Some("audio.flac".into());
        done.upload_attempts = 1;
        insert(&state, &done).await;
        let key = inflight_key("netease", "gateway-upload", "lossless");
        let (_inflight, _) = register_inflight(&state, key.clone()).await;
        let client = GatewayClient::new(&serve(state.clone()).await, TOKEN).unwrap();

        let job = client
            .poll_media(&done.id, None, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(job.state, JobState::Uploading);

        // The upload ended without a file and the local copy is gone: the
        // task fails so that the next request downloads the song again.
        remove_inflight(&state, &key).await;
        let job = client
            .poll_media(&done.id, None, Duration::ZERO)
            .await
            .unwrap();
        let JobState::Failed { error } = job.state else {
            panic!("expected failure, got {:?}", job.state);
        };
        assert_eq!(error.message, "本地音频已清理，请重新请求");
        assert_eq!(
            state.downloads.read().await[&done.id].status,
            DownloadStatus::Failed
        );

        let mut exhausted = done.clone();
        exhausted.status = DownloadStatus::Completed;
        exhausted.upload_attempts = UPLOAD_ATTEMPTS_MAX;
        exhausted.upload_error = Some("Request Entity Too Large".into());
        insert(&state, &exhausted).await;
        let job = client
            .poll_media(&done.id, None, Duration::ZERO)
            .await
            .unwrap();
        let JobState::Failed { error } = job.state else {
            panic!("expected failure");
        };
        assert_eq!(
            error.message,
            "上传到 Telegram 失败：Request Entity Too Large"
        );
    }

    #[tokio::test]
    async fn malformed_requests_get_error_bodies() {
        let directory = tempfile::tempdir().unwrap();
        let base = serve(test_state(directory.path())).await;
        let response = reqwest::Client::new()
            .post(format!("{base}{}", paths::SEARCH))
            .bearer_auth(TOKEN)
            .header(PROTOCOL_HEADER, PROTOCOL_VERSION.to_string())
            .header(header::CONTENT_TYPE, "application/json")
            .body(r#"{"platform":"spotify","keyword":"x","limit":1}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: ErrorBody = response.json().await.unwrap();
        assert_eq!(body.code, ErrorCode::BadRequest);

        let client = GatewayClient::new(&base, TOKEN).unwrap();
        let error = client.search(Platform::Netease, "  ", 5).await.unwrap_err();
        assert!(matches!(error, GatewayError::Remote(_)));
        assert_eq!(error.to_string(), "keyword 不能为空");
        let error = client
            .poll_media("missing", None, Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::NotFound);
    }

    #[test]
    fn apple_lookups_also_accept_the_bots_legacy_cache_key() {
        assert_eq!(cache_keys(Platform::Netease, "lossless"), ["lossless"]);
        assert_eq!(
            cache_keys(Platform::AppleMusic, "lossless"),
            ["lossless", "lossless:tg-m4a-v2"]
        );
    }

    #[test]
    fn api_errors_map_to_codes_without_urls() {
        let limited = Failure::from(ApiError::RateLimited {
            message: "slow down".into(),
            retry_after: Some(Duration::from_millis(1500)),
        });
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limited.body.retry_after_ms, Some(1500));
        let custom = Failure::from(ApiError::Custom(
            "未找到歌曲 https://api.example.test/?cookie=MUSIC_U".into(),
        ));
        assert_eq!(custom.body.code, ErrorCode::Upstream);
        assert!(!custom.body.message.contains("MUSIC_U"));
        let io = Failure::from(ApiError::IOError(std::io::Error::other("/opt/private")));
        assert_eq!(io.body.code, ErrorCode::Unavailable);
        assert!(!io.body.message.contains("/opt"));
    }
}
