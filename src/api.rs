use crate::{
    ApiError, ApiResult,
    audio::{
        BrowserAudioService, PendingAudioFile, ensure_audio_file, find_mislabeled_aac,
        lock_audio_path, needs_browser_lossless_derivative, repair_mislabeled_aac,
        valid_audio_file,
    },
    audio_cache,
    cache::{MusicCache, find_music_cache, find_music_cache_by_source, upsert_music_cache},
    loudness::{self, LoudnessResponse},
    matching,
    music::{self, ResolveOptions},
    settings::SETTINGS,
    telegram_cache, telegram_segments, upstream_limit,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env,
    io::SeekFrom,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{Notify, RwLock},
};
use tokio_util::io::ReaderStream;
use tower_http::{
    compression::{
        CompressionLayer,
        predicate::{DefaultPredicate, Predicate},
    },
    cors::CorsLayer,
    services::ServeDir,
    trace::TraceLayer,
};

const AUDIO_STREAM_CHUNK_SIZE: usize = 64 * 1024;

fn audio_stream_identity(metadata: &std::fs::Metadata) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // dev/inode refer to the open object, not its path. An active growing
        // response pins this inode while a completed-file Range is negotiated.
        // Birth time, when available, also distinguishes later inode reuse.
        // Never use size/mtime: both legitimately change while audio grows.
        let created = metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| format!("{}:{}", duration.as_secs(), duration.subsec_nanos()))
            .unwrap_or_default();
        Some(format!(
            "{:x}",
            md5::compute(format!("{}:{}:{created}", metadata.dev(), metadata.ino()))
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        // Without a reliable immutable file identity, clients retain their
        // original stream rather than splice possibly different packet bytes.
        None
    }
}

#[derive(Clone)]
pub struct AppState {
    downloads: Arc<RwLock<HashMap<String, DownloadTask>>>,
    /// Wakes long-polling status requests after a task changes state.
    task_events: Arc<Notify>,
    /// Accounts and sync; `None` when `[auth]` is not configured.
    accounts: Option<Arc<crate::accounts::Accounts>>,
    inflight: Arc<RwLock<HashMap<String, Arc<InflightDownload>>>>,
    data_dir: PathBuf,
    media_base_url: Option<url::Url>,
}

#[derive(Clone, Debug)]
struct DownloadTask {
    id: String,
    work_id: String,
    /// Identifies one immutable source generation, independently of a file's
    /// pathname or lazily materialized browser/packet representations.
    media_revision: String,
    status: DownloadStatus,
    platform: String,
    query: String,
    track_id: Option<String>,
    selected_id: Option<String>,
    quality: String,
    include_cover: bool,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    source_url: Option<String>,
    filename: Option<String>,
    cover_filename: Option<String>,
    file_id: Option<String>,
    thumb_file_id: Option<String>,
    content_type: Option<String>,
    byte_size: Option<u64>,
    bitrate: Option<u32>,
    duration: Option<u32>,
    format: Option<String>,
    decrypt_elapsed_seconds: Option<f64>,
    error: Option<String>,
    /// `unplayable` when no source can play this song; clients skip it.
    error_code: Option<String>,
    /// Explicit transport requested by a capable client (for example,
    /// `wasm_alac`).  This is deliberately separate from quality so an
    /// ordinary browser never receives ALAC expecting native decoding.
    streaming_mode: Option<String>,
    /// Declared length of a progressive HTTP source while it is still being
    /// mirrored, so the growing response can answer Range requests.
    stream_total: Option<u64>,
    /// Provider-measured ReplayGain (centi-dB), see `MusicTrack`.
    replay_gain_centi_db: Option<i32>,
    /// Cover image on the platform, kept for gateway clients.
    cover_url: Option<String>,
    /// Bytes fetched by a complete (non-streaming) download so far.
    transfer: Arc<TransferProgress>,
    /// Telegram cache uploads started for this generation, and why the last
    /// one failed. Gateway clients wait for the upload; the web does not.
    upload_attempts: u8,
    upload_error: Option<String>,
}

#[derive(Debug, Default)]
struct TransferProgress {
    written: std::sync::atomic::AtomicU64,
    /// 0 while the length is unknown.
    total: std::sync::atomic::AtomicU64,
}

impl TransferProgress {
    fn record(&self, progress: music::DownloadProgress) {
        use std::sync::atomic::Ordering::Relaxed;
        self.written.store(progress.written, Relaxed);
        self.total.store(progress.total.unwrap_or(0), Relaxed);
    }

    fn snapshot(&self) -> (u64, Option<u64>) {
        use std::sync::atomic::Ordering::Relaxed;
        let total = self.total.load(Relaxed);
        (self.written.load(Relaxed), (total > 0).then_some(total))
    }
}

#[derive(Debug, Default)]
struct InflightDownload {
    inner: RwLock<InflightDownloadState>,
    notify: Notify,
}

#[derive(Clone, Debug, Default)]
struct InflightDownloadState {
    task: Option<DownloadTask>,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlaybackTask {
    pub id: String,
    pub media_revision: String,
    pub status: DownloadStatus,
    pub platform: String,
    pub query: String,
    pub track_id: Option<String>,
    pub selected_id: Option<String>,
    pub quality: String,
    pub include_cover: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub source_url: Option<String>,
    pub content_type: Option<String>,
    pub byte_size: Option<u64>,
    pub bitrate: Option<u32>,
    pub duration: Option<u32>,
    pub format: Option<String>,
    pub decrypt_elapsed_seconds: Option<f64>,
    pub error: Option<String>,
    /// Machine-readable failure kind. `unplayable`: no configured source
    /// can ever play this song, so players skip it instead of retrying.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Transport selected while the task was created.  `wasm_alac` means the
    /// dedicated streaming_url exposes decrypted SPLALAC1 packets for a
    /// WebAssembly decoder; it is absent for native AAC/full-download tasks.
    pub streaming_mode: Option<String>,
    pub streaming_codec: Option<String>,
    pub streaming_protocol: Option<String>,
    pub audio_url: String,
    pub streaming_url: Option<String>,
    /// The stored ALAC M4A of a completed Apple Music lossless task, exactly
    /// as Telegram keeps it: the WASM player demuxes it directly and
    /// downloads save it as is. Older clients keep using `streaming_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alac_source_url: Option<String>,
    /// Declared length of a growing progressive `/audio` stream. Present only
    /// while `streaming` and the stream answers Range requests, so native
    /// players (AVPlayer, ExoPlayer) can start before the task completes;
    /// absent for remuxed/decrypted streams that have no final length yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_length: Option<u64>,
    pub cover_url: String,
    pub lyrics_url: String,
    /// Loudness derived from the provider's own measurement, available before
    /// the file finishes so the first play can already be normalized. A later
    /// `/loudness` measurement of the actual file supersedes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) loudness_estimate: Option<crate::loudness::LoudnessResponse>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Queued,
    Processing,
    /// The first browser-playable fMP4 fragment is available while the
    /// worker continues downloading/decrypting the remaining segments.
    Streaming,
    Completed,
    Failed,
}

impl DownloadStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Processing => "processing",
            Self::Streaming => "streaming",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// Optional long-poll for `GET /api/v1/playbacks/:id`: with `since` set to
/// the status a client already knows, the response waits (up to `wait_ms`)
/// until the task leaves that status. Without it the request returns at once.
#[derive(Debug, Default, Deserialize)]
pub struct PlaybackStatusQuery {
    since: Option<String>,
    wait_ms: Option<u64>,
}

const PLAYBACK_WAIT_MAX: Duration = Duration::from_secs(15);
// Every production transition notifies waiters; the periodic re-check only
// bounds latency if a future status write forgets to.
const PLAYBACK_WAIT_RECHECK: Duration = Duration::from_millis(500);

#[derive(Debug, Deserialize)]
pub struct SearchParams {
    pub platform: String,
    pub q: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

#[derive(Debug, Deserialize)]
pub struct RecommendationParams {
    #[serde(default = "default_apple_platform")]
    pub platform: String,
    #[serde(default = "default_recommendation_limit")]
    pub limit: usize,
    #[serde(default)]
    pub refresh: bool,
    #[serde(default)]
    pub exclude: String,
}

#[derive(Debug, Deserialize)]
pub struct RecommendationContinuationParams {
    #[serde(default = "default_apple_platform")]
    pub platform: String,
    pub seed_id: String,
    #[serde(default)]
    pub exclude: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

#[derive(Debug, Deserialize)]
pub struct RecommendationContinuationRequest {
    #[serde(default = "default_apple_platform")]
    pub platform: String,
    pub seed_id: String,
    /// The queue's latest songs, oldest first, used to seed Apple AutoPlay.
    #[serde(default)]
    pub seed_ids: Vec<String>,
    /// AutoPlay station returned by an earlier batch for the same queue.
    #[serde(default)]
    pub station: Option<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

#[derive(Debug, Deserialize)]
pub struct CollectionParams {
    pub platform: String,
    pub url: String,
    /// Omit for complete import; an explicit limit requests a preview.
    pub limit: Option<usize>,
    /// Import what the platform listed even when it listed fewer songs than
    /// the collection claims (the user confirmed after an
    /// `incomplete_collection` error).
    #[serde(default)]
    pub allow_incomplete: bool,
}

#[derive(Debug, Deserialize)]
pub struct DownloadRequest {
    pub platform: String,
    pub query: String,
    pub selected_id: Option<String>,
    pub quality: Option<String>,
    #[serde(default = "default_true")]
    pub include_cover: bool,
    /// Optional explicit transport capability.  `wasm_alac` is the only
    /// accepted value today; omitting it preserves native AAC/full-download
    /// behavior for existing clients.
    #[serde(default)]
    pub streaming_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LyricsParams {
    pub platform: String,
    pub track_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub duration: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct MediaVersionQuery {
    rev: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchItem {
    pub platform: String,
    pub id: String,
    pub song: String,
    pub singer: String,
    pub cover: String,
}

#[derive(Debug, Serialize)]
pub struct CollectionResponse {
    pub platform: String,
    pub id: String,
    pub title: String,
    pub source_url: String,
    pub items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
pub struct MatchCollectionRequest {
    pub source_platform: String,
    pub target_platform: String,
    pub items: Vec<MatchSourceItem>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MatchSourceItem {
    pub id: String,
    pub song: String,
    pub singer: String,
    #[serde(default)]
    pub cover: String,
}

#[derive(Debug, Serialize)]
pub struct MatchCollectionResponse {
    pub source_platform: String,
    pub target_platform: String,
    pub matched_count: usize,
    pub review_count: usize,
    pub missing_count: usize,
    pub deferred_count: usize,
    /// Suggested wait before resending the deferred items.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    pub items: Vec<MatchCollectionItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MatchCollectionItem {
    pub source: MatchSourceItem,
    pub status: MatchStatus,
    pub confidence: u8,
    pub candidates: Vec<MatchCandidate>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MatchCandidate {
    #[serde(flatten)]
    pub item: SearchItem,
    /// 0..=100 similarity to the source track, for the review UI.
    pub score: u8,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus {
    Matched,
    Review,
    Missing,
    /// Not searched yet because the platform asked to slow down; resend later.
    Deferred,
}

#[derive(Debug, Serialize)]
pub struct CreateDownloadResponse {
    pub playback: PlaybackTask,
    pub status_url: String,
    pub audio_url: String,
    pub cover_url: String,
    pub lyrics_url: String,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: &'static str,
}

#[derive(Debug, Serialize)]
pub struct LyricsResponse {
    pub platform: String,
    pub track_id: String,
    pub available: bool,
    pub plain: String,
    pub translation: String,
    /// Word-timed lines (`{start, end, bg?, words: [[start, end, text]]}`, ms).
    pub words: Vec<music::WordLine>,
    /// Timed LRC romanization, empty when the source has none.
    pub roman: String,
    /// `role：names` credits lifted out of the lyric lines.
    pub credits: Vec<String>,
}

mod gateway;
mod lyric_upload;

pub fn app() -> Router {
    app_with_state(start_state(), serve_web())
}

/// The site and API together with the loopback gateway, sharing one task,
/// inflight and cache state so their requests for a song are deduplicated.
pub fn app_and_gateway(gateway_token: &str) -> (Router, Router) {
    let state = start_state();
    (
        app_with_state(state.clone(), serve_web()),
        gateway::router(state, gateway_token),
    )
}

fn start_state() -> AppState {
    let data_dir = env::var("MUSIC_API_DATA_DIR").unwrap_or_else(|_| "data".to_string());
    let state = AppState {
        downloads: Arc::new(RwLock::new(HashMap::new())),
        task_events: Arc::new(Notify::new()),
        accounts: crate::accounts::Accounts::from_settings(
            &SETTINGS.auth,
            Arc::new(crate::accounts::store::MySqlStore::new(SETTINGS.db.mysql.url.clone())),
        ),
        inflight: Arc::new(RwLock::new(HashMap::new())),
        data_dir: PathBuf::from(data_dir),
        media_base_url: media_base_url(env::var("MUSIC_API_MEDIA_BASE_URL").ok().as_deref())
            .expect("MUSIC_API_MEDIA_BASE_URL must be an HTTP(S) origin without credentials, path, query or fragment"),
    };
    spawn_audio_cache_sweeper(state.clone());
    music::spawn_netease_session_keeper(state.data_dir.clone());
    telegram_segments::warm_up();
    state
}

fn app_with_state(state: AppState, serve_web: bool) -> Router {
    let accounts = state.accounts.clone();
    let router = Router::new()
        .route("/health", get(health))
        .route("/api/v1/platforms", get(platforms))
        .route("/api/v1/search", get(search))
        .route("/api/v1/recommendations", get(recommendations))
        .route("/api/v1/recommendations/home", get(recommendation_home))
        .route(
            "/api/v1/recommendations/continue",
            get(recommendation_continuation).post(recommendation_continuation_post),
        )
        .route("/api/v1/collections/resolve", get(resolve_collection))
        .route("/api/v1/collections/match", post(match_collection))
        .route("/api/v1/lyrics", get(lyrics))
        .route("/api/v1/tracks/:platform/:id", get(track_info))
        .route("/api/v1/playbacks", post(create_download))
        .route("/api/v1/playbacks/:id", get(get_download))
        .route("/api/v1/playbacks/:id/audio", get(download_file))
        .route("/api/v1/playbacks/:id/audio/stream", get(download_stream))
        .route("/api/v1/playbacks/:id/audio/source", get(download_source))
        .route("/api/v1/playbacks/:id/cover", get(download_cover))
        .route("/api/v1/playbacks/:id/lyrics", get(download_lyrics))
        .route("/api/v1/playbacks/:id/loudness", get(playback_loudness))
        .with_state(state)
        .merge(crate::accounts::router(accounts))
        // Upstream work started by a request is metered against its client.
        .layer(middleware::from_fn(crate::upstream_limit::attach_principal));

    let router = if serve_web {
        // The frontend build writes `.br`/`.gz` siblings; serve them directly
        // instead of compressing static assets on every request.
        router.fallback_service(
            ServeDir::new("web")
                .append_index_html_on_directories(true)
                .precompressed_br()
                .precompressed_gzip(),
        )
    } else {
        router
    };
    // Apply after the fallback is installed, so extractor failures, method
    // rejections and missing static/API routes receive the same safe policy.
    router
        .layer(CorsLayer::permissive().expose_headers([
            header::CONTENT_TYPE,
            header::CONTENT_LENGTH,
            header::CONTENT_RANGE,
            header::CONTENT_ENCODING,
            header::CONTENT_DISPOSITION,
            header::ACCEPT_RANGES,
            header::CACHE_CONTROL,
            header::ETAG,
            header::HeaderName::from_static("x-audio-stream-id"),
        ]))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(response_cache_policy))
        // Search, recommendation and collection payloads are verbose JSON.
        // Media, packet streams and precompressed files are never touched.
        .layer(
            CompressionLayer::new().compress_when(DefaultPredicate::new().and(
                |_: StatusCode,
                 _: axum::http::Version,
                 headers: &HeaderMap,
                 _: &axum::http::Extensions| {
                    headers
                        .get(header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .is_some_and(|value| value.starts_with("application/json"))
                },
            )),
        )
}

fn media_base_url(value: Option<&str>) -> Result<Option<url::Url>, &'static str> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let url = url::Url::parse(value).map_err(|_| "invalid media origin")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("invalid media origin");
    }
    Ok(Some(url))
}

async fn response_cache_policy(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let method = request.method().clone();
    let credentialed = request.headers().contains_key(header::AUTHORIZATION)
        || request.headers().contains_key(header::COOKIE);
    let mut response = next.run(request).await;
    let policy = if credentialed || response.headers().contains_key(header::SET_COOKIE) {
        Some("no-store, no-transform")
    } else if (!response.status().is_success() && response.status() != StatusCode::NOT_MODIFIED)
        || !matches!(method, Method::GET | Method::HEAD)
    {
        Some("no-store")
    } else if response.headers().contains_key(header::CACHE_CONTROL) {
        None
    } else if path.starts_with("/api/") || path == "/health" {
        Some("no-store")
    } else if path.starts_with("/assets/") {
        Some("public, max-age=31536000, immutable")
    } else if path.starts_with("/docs/") {
        Some("public, max-age=300")
    } else if path == "/"
        || path.ends_with(".html")
        || path == "/sw.js"
        || path == "/manifest.webmanifest"
    {
        Some("no-cache")
    } else {
        Some("public, max-age=3600")
    };
    if let Some(policy) = policy {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, policy.parse().unwrap());
    }
    // Static files are negotiated between `.br`, `.gz` and identity; shared
    // caches must key them by the request encoding.
    if response.headers().contains_key(header::CONTENT_ENCODING) && !path.starts_with("/api/") {
        response.headers_mut().append(
            header::VARY,
            header::HeaderValue::from_static("accept-encoding"),
        );
    }
    response
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "music-api",
    })
}

async fn platforms() -> Json<Vec<music::PlatformInfo>> {
    Json(music::platforms())
}

async fn search(Query(params): Query<SearchParams>) -> ApiResult<Json<Vec<SearchItem>>> {
    let platform = music::parse_platform(&params.platform)?;
    let items = music::search(platform, &params.q, params.limit.clamp(1, 20)).await?;
    Ok(Json(
        items
            .into_iter()
            .map(|item| SearchItem {
                platform: item.platform.id().to_string(),
                id: item.id,
                song: item.song,
                singer: item.singer,
                cover: item.cover,
            })
            .collect(),
    ))
}

/// Catalog metadata for a shared song, looked up by platform and id only so a
/// share link cannot carry made-up titles or covers.
async fn track_info(
    Path((platform, id)): Path<(String, String)>,
) -> ApiResult<(
    [(header::HeaderName, &'static str); 1],
    Json<music::TrackInfo>,
)> {
    let platform = music::parse_platform(&platform)?;
    let info = upstream_limit::metered(
        upstream_limit::Kind::Metadata,
        music::track_info(platform, &id),
    )
    .await?;
    Ok((
        [(header::CACHE_CONTROL, "public, max-age=3600")],
        Json(info),
    ))
}

// Recommendation responses are newly sampled even when the upstream pool is
// cached. Browsers and proxies must not reuse an earlier sampled batch.
type RecommendationJson<T> = ([(header::HeaderName, &'static str); 1], Json<T>);
fn recommendation_json<T>(value: T) -> RecommendationJson<T> {
    ([(header::CACHE_CONTROL, "no-store")], Json(value))
}

async fn recommendations(
    Query(params): Query<RecommendationParams>,
) -> ApiResult<RecommendationJson<Vec<SearchItem>>> {
    let platform = music::parse_platform(&params.platform)?;
    let exclude = recommendation_excludes(params.exclude.split(','), 500);
    let items = music::recommendations(
        platform,
        params.limit.clamp(1, 50),
        params.refresh,
        &exclude,
    )
    .await?;
    Ok(recommendation_json(
        items
            .into_iter()
            .map(|item| SearchItem {
                platform: item.platform.id().to_string(),
                id: item.id,
                song: item.song,
                singer: item.singer,
                cover: item.cover,
            })
            .collect(),
    ))
}

async fn recommendation_home(
    Query(params): Query<RecommendationParams>,
) -> ApiResult<RecommendationJson<music::RecommendationHome>> {
    let platform = music::parse_platform(&params.platform)?;
    let exclude = recommendation_excludes(params.exclude.split(','), 500);
    let home = music::recommendation_home(
        platform,
        params.limit.clamp(1, 50),
        params.refresh,
        &exclude,
    )
    .await?;
    Ok(recommendation_json(home))
}

async fn recommendation_continuation(
    Query(params): Query<RecommendationContinuationParams>,
) -> ApiResult<RecommendationJson<music::RecommendationContinuation>> {
    let platform = music::parse_platform(&params.platform)?;
    let exclude = recommendation_excludes(params.exclude.split(','), 10_000);
    Ok(recommendation_json(
        music::recommendation_continuation(
            platform,
            &params.seed_id,
            &[],
            None,
            &exclude,
            params.limit.clamp(1, 50),
        )
        .await?,
    ))
}

async fn recommendation_continuation_post(
    Json(params): Json<RecommendationContinuationRequest>,
) -> ApiResult<RecommendationJson<music::RecommendationContinuation>> {
    let platform = music::parse_platform(&params.platform)?;
    let exclude = recommendation_excludes(params.exclude.iter().map(String::as_str), 10_000);
    let seeds = recommendation_excludes(params.seed_ids.iter().map(String::as_str), 50);
    Ok(recommendation_json(
        music::recommendation_continuation(
            platform,
            &params.seed_id,
            &seeds,
            params.station.as_deref(),
            &exclude,
            params.limit.clamp(1, 50),
        )
        .await?,
    ))
}

fn recommendation_excludes<'a>(
    ids: impl IntoIterator<Item = &'a str>,
    limit: usize,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .map(str::trim)
        .filter(|id| {
            (1..=20).contains(&id.len())
                && id.bytes().all(|byte| byte.is_ascii_digit())
                && id.bytes().any(|byte| byte != b'0')
        })
        .filter(|id| seen.insert((*id).to_string()))
        .take(limit)
        .map(str::to_string)
        .collect()
}

async fn resolve_collection(
    Query(params): Query<CollectionParams>,
) -> ApiResult<Json<CollectionResponse>> {
    let platform = music::parse_platform(&params.platform)?;
    let source_url = params.url.trim();
    if source_url.is_empty() {
        return Err(ApiError::Custom("url 不能为空".to_string()));
    }
    let limit = collection_request_limit(params.limit)?;
    let collection = upstream_limit::metered(
        upstream_limit::Kind::Metadata,
        music::collection(platform, source_url, limit, params.allow_incomplete),
    )
    .await?;
    if params.limit.is_none() && collection.items.len() > music::MAX_COLLECTION_ITEMS {
        return Err(ApiError::Custom(format!(
            "歌单超过 {} 首，已停止导入以避免截断；请先拆分歌单",
            music::MAX_COLLECTION_ITEMS
        )));
    }
    Ok(Json(CollectionResponse {
        platform: collection.platform,
        id: collection.id,
        title: collection.title,
        source_url: source_url.to_string(),
        items: collection
            .items
            .into_iter()
            .map(|item| SearchItem {
                platform: item.platform,
                id: item.id,
                song: item.song,
                singer: item.singer,
                cover: item.cover,
            })
            .collect(),
    }))
}

/// How long one match request may spend waiting for paced searches.
const MATCH_BUDGET: Duration = Duration::from_secs(20);
/// Resend hint for items deferred by timeouts or transient errors.
const DEFERRED_RETRY: Duration = Duration::from_secs(3);

async fn match_collection(
    Json(request): Json<MatchCollectionRequest>,
) -> ApiResult<Json<MatchCollectionResponse>> {
    // Matching only reads the source tracks' metadata, so playlists from an
    // offline platform can still move to one that is online.
    let source_platform = music::parse_any_platform(&request.source_platform)?;
    let target_platform = music::parse_platform(&request.target_platform)?;
    if request.items.is_empty() {
        return Err(ApiError::Custom("歌单没有可匹配的歌曲".to_string()));
    }
    if request.items.len() > 200 {
        return Err(ApiError::Custom("单次最多匹配 200 首歌曲".to_string()));
    }
    let target_id = target_platform.id().to_string();
    if source_platform == target_platform {
        let items = request
            .items
            .into_iter()
            .map(|source| direct_match_collection_item(source, &target_id))
            .collect::<Vec<_>>();
        return Ok(Json(match_collection_response(
            source_platform.id(),
            target_id,
            items,
        )));
    }
    // One request answers within this budget; whatever the paced search
    // could not reach comes back as `deferred` for the client to resend.
    let deadline = std::time::Instant::now() + MATCH_BUDGET;
    // Repeats of the same song in a playlist cost one search.
    let mut groups: Vec<(MatchSourceItem, Vec<(usize, MatchSourceItem)>)> = Vec::new();
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    for (index, source) in request.items.into_iter().enumerate() {
        let key = (
            source.song.trim().to_lowercase(),
            source.singer.trim().to_lowercase(),
        );
        match seen.get(&key) {
            Some(&group) => groups[group].1.push((index, source)),
            None => {
                seen.insert(key, groups.len());
                groups.push((source.clone(), vec![(index, source)]));
            }
        }
    }
    let sources = groups
        .iter()
        .map(|(source, _)| source.clone())
        .collect::<Vec<_>>();
    let outcomes = stream::iter(sources)
        .map(|source| async move {
            let search = |query: String| async move {
                music::search_bulk(target_platform, &query, 8, deadline).await
            };
            let candidates = search(format!("{} {}", source.song, source.singer)).await?;
            let mut item = match_collection_item(source.clone(), candidates.clone());
            // Long annotated titles or several artists can hide the song from
            // a platform's search; retry once with "title first-artist".
            if !matches!(item.status, MatchStatus::Matched)
                && let Some(query) = matching::fallback_query(&source.song, &source.singer)
            {
                let mut combined = candidates;
                combined.extend(search(query).await?);
                item = match_collection_item(source, combined);
            }
            Ok::<_, ApiError>(item)
        })
        .buffered(6)
        .collect::<Vec<_>>()
        .await;
    // An unfinished search is never reported as "missing": that would turn a
    // throttled platform into a list of songs it supposedly lacks.
    let mut retry_after: Option<Duration> = None;
    let mut slots: Vec<Option<MatchCollectionItem>> = Vec::new();
    slots.resize_with(groups.iter().map(|(_, rows)| rows.len()).sum(), || None);
    for ((source, rows), outcome) in groups.into_iter().zip(outcomes) {
        let item = outcome.unwrap_or_else(|err| {
            let wait = match err {
                ApiError::RateLimited {
                    retry_after: Some(wait),
                    ..
                }
                | ApiError::ClientRateLimited { retry_after: wait } => wait,
                _ => DEFERRED_RETRY,
            };
            retry_after = Some(retry_after.map_or(wait, |current| current.min(wait)));
            deferred_collection_item(source)
        });
        for (index, source) in rows {
            slots[index] = Some(MatchCollectionItem {
                source,
                ..item.clone()
            });
        }
    }
    let items = slots.into_iter().map(Option::unwrap).collect::<Vec<_>>();
    let mut response = match_collection_response(source_platform.id(), target_id, items);
    response.retry_after_ms = retry_after.map(|wait| wait.as_millis() as u64);
    Ok(Json(response))
}

fn deferred_collection_item(source: MatchSourceItem) -> MatchCollectionItem {
    MatchCollectionItem {
        source,
        status: MatchStatus::Deferred,
        confidence: 0,
        candidates: Vec::new(),
    }
}

fn direct_match_collection_item(source: MatchSourceItem, platform: &str) -> MatchCollectionItem {
    let candidate = SearchItem {
        platform: platform.to_string(),
        id: source.id.clone(),
        song: source.song.clone(),
        singer: source.singer.clone(),
        cover: source.cover.clone(),
    };
    MatchCollectionItem {
        source,
        status: MatchStatus::Matched,
        confidence: 100,
        candidates: vec![MatchCandidate {
            item: candidate,
            score: 100,
        }],
    }
}

fn match_collection_response(
    source_platform: &str,
    target_platform: String,
    items: Vec<MatchCollectionItem>,
) -> MatchCollectionResponse {
    let matched_count = items
        .iter()
        .filter(|item| matches!(item.status, MatchStatus::Matched))
        .count();
    let review_count = items
        .iter()
        .filter(|item| matches!(item.status, MatchStatus::Review))
        .count();
    let missing_count = items
        .iter()
        .filter(|item| matches!(item.status, MatchStatus::Missing))
        .count();
    let deferred_count = items
        .iter()
        .filter(|item| matches!(item.status, MatchStatus::Deferred))
        .count();
    MatchCollectionResponse {
        source_platform: source_platform.to_string(),
        target_platform,
        matched_count,
        review_count,
        missing_count,
        deferred_count,
        retry_after_ms: None,
        items,
    }
}

fn match_collection_item(
    source: MatchSourceItem,
    candidates: Vec<crate::music::MusicSearchItem>,
) -> MatchCollectionItem {
    let (decision, confidence, ranked) =
        matching::rank(&source.song, &source.singer, candidates, |candidate| {
            (candidate.song.as_str(), candidate.singer.as_str())
        });
    MatchCollectionItem {
        source,
        status: match decision {
            matching::Decision::Matched => MatchStatus::Matched,
            matching::Decision::Review => MatchStatus::Review,
            matching::Decision::Missing => MatchStatus::Missing,
        },
        confidence,
        candidates: ranked
            .into_iter()
            .take(5)
            .map(|ranked| MatchCandidate {
                score: ranked.score,
                item: SearchItem {
                    platform: ranked.item.platform.id().to_string(),
                    id: ranked.item.id,
                    song: ranked.item.song,
                    singer: ranked.item.singer,
                    cover: ranked.item.cover,
                },
            })
            .collect(),
    }
}

async fn lyrics(Query(params): Query<LyricsParams>) -> ApiResult<Json<LyricsResponse>> {
    let platform = music::parse_platform(&params.platform)?;
    let track_id = params.track_id.trim();
    if track_id.is_empty() {
        return Err(ApiError::Custom("track_id 不能为空".to_string()));
    }
    let lyrics = if params
        .title
        .as_deref()
        .is_some_and(|title| !title.trim().is_empty())
    {
        upstream_limit::metered(
            upstream_limit::Kind::Lyrics,
            music::lyrics_for_track(
                platform,
                track_id,
                params.title.as_deref().unwrap_or_default(),
                params.artist.as_deref().unwrap_or_default(),
                params.album.as_deref().unwrap_or_default(),
                params.duration,
            ),
        )
        .await?
    } else {
        upstream_limit::metered(
            upstream_limit::Kind::Lyrics,
            music::lyrics(platform, track_id),
        )
        .await?
    };
    Ok(Json(lyrics_response(platform.id(), track_id, lyrics)))
}

/// `error_code` / response `code` for a song no source can play.
const UNPLAYABLE_CODE: &str = "unplayable";
const INCOMPLETE_COLLECTION_CODE: &str = "incomplete_collection";

fn normalize_streaming_mode(mode: Option<String>) -> Option<String> {
    mode.and_then(|value| {
        let normalized = value.trim().to_ascii_lowercase();
        (normalized == "wasm_alac").then_some(normalized)
    })
}

fn upgrade_completed_alac_transport(task: &mut DownloadTask, requested_mode: Option<&str>) {
    // Apple's lossless downloads are ALAC containers (there is no AAC quality
    // fallback). Other providers can return FLAC, so do not infer their codec
    // from the quality label. The packet builder still validates the cookie.
    if requested_mode == Some("wasm_alac")
        && task.status == DownloadStatus::Completed
        && task.platform == "applemusic"
        && needs_browser_lossless_derivative(
            &task.quality,
            task.format.as_deref(),
            task.content_type.as_deref(),
            task.filename.as_deref(),
        )
    {
        task.streaming_mode = Some("wasm_alac".to_string());
    }
}

fn task_for_client(task: &DownloadTask, requested_mode: Option<&str>) -> DownloadTask {
    let mut task = task.clone();
    if requested_mode != Some("wasm_alac") {
        task.streaming_mode = None;
    }
    task
}

async fn create_download(
    State(state): State<AppState>,
    Json(request): Json<DownloadRequest>,
) -> ApiResult<(StatusCode, Json<CreateDownloadResponse>)> {
    let task = start_download(&state, request).await?;
    let status = if task.status == DownloadStatus::Completed {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((status, Json(create_download_response(&state, &task))))
}

/// Joins the live task for this song and quality, completes a new one from
/// the Telegram cache, or starts a download worker. Shared by the web API and
/// the gateway so both always reuse the same generation.
async fn start_download(state: &AppState, request: DownloadRequest) -> ApiResult<DownloadTask> {
    let platform = music::parse_platform(&request.platform)?;
    if request.query.trim().is_empty() {
        return Err(ApiError::Custom("query 不能为空".to_string()));
    }

    let quality = request.quality.unwrap_or_else(|| "high".to_string());
    let streaming_mode = normalize_streaming_mode(request.streaming_mode);
    let query = request.query.trim().to_string();
    let selected_id = request.selected_id.filter(|id| !id.trim().is_empty());
    let resolved_track = if selected_id.is_none() {
        // Without an id the platform is searched for the query.
        Some(
            upstream_limit::metered(
                upstream_limit::Kind::Search,
                music::resolve(ResolveOptions {
                    platform,
                    query: query.clone(),
                    selected_id: None,
                    quality: Some(quality.clone()),
                }),
            )
            .await?,
        )
    } else {
        None
    };
    let track_id = selected_id
        .as_deref()
        .or(resolved_track.as_ref().map(|track| track.id.as_str()))
        .ok_or_else(|| ApiError::Custom("无法确定平台歌曲 ID".to_string()))?
        .to_string();
    let id = playback_id(platform.id(), &track_id, &quality);

    let existing = {
        let mut downloads = state.downloads.write().await;
        downloads
            .get_mut(&id)
            .filter(|task| task.status != DownloadStatus::Failed)
            .map(|task| {
                upgrade_completed_alac_transport(task, streaming_mode.as_deref());
                task_for_client(task, streaming_mode.as_deref())
            })
    };
    if let Some(existing) = existing {
        return Ok(existing);
    }

    // Resolve an already uploaded Telegram item before creating an async
    // worker. This turns the common warm-cache path into one request instead
    // of create -> poll -> complete, which is especially noticeable when the
    // browser is starting a song over a high-latency connection.
    let cached = find_music_cache(platform.id(), &track_id, &quality).await?;
    let cache_hit = cached
        .as_ref()
        .is_some_and(|cache| !cache.file_id.trim().is_empty());
    // Only a song that is neither live nor cached reaches the platform, so
    // only that costs the client a download. Charged before the task is
    // published, so a refused request leaves nothing behind.
    let charge = if cache_hit {
        None
    } else {
        upstream_limit::charge(upstream_limit::Kind::Download).await?
    };

    let media_revision = uuid::Uuid::new_v4().simple().to_string();
    let task = DownloadTask {
        id: id.clone(),
        work_id: generation_work_id(&id, &media_revision),
        media_revision,
        status: DownloadStatus::Queued,
        platform: platform.id().to_string(),
        query,
        track_id: Some(track_id.clone()),
        selected_id: Some(track_id.clone()),
        quality,
        include_cover: request.include_cover,
        title: resolved_track.as_ref().map(|track| track.song.clone()),
        artist: resolved_track.as_ref().map(|track| track.singer.clone()),
        album: resolved_track.as_ref().map(|track| track.album.clone()),
        source_url: resolved_track.as_ref().map(|track| track.link.clone()),
        filename: None,
        cover_filename: None,
        file_id: None,
        thumb_file_id: None,
        content_type: None,
        byte_size: None,
        bitrate: None,
        duration: None,
        format: None,
        decrypt_elapsed_seconds: None,
        error: None,
        error_code: None,
        streaming_mode,
        stream_total: None,
        replay_gain_centi_db: None,
        cover_url: resolved_track
            .as_ref()
            .map(|track| track.cover.clone())
            .filter(|cover| !cover.trim().is_empty()),
        transfer: Arc::default(),
        upload_attempts: 0,
        upload_error: None,
    };

    // Resolving and checking Telegram may have yielded to another POST for
    // this song. Only one generation may own the shared public task ID;
    // otherwise a worker could publish filenames into another generation's
    // directory. Recheck atomically before publishing or starting work.
    if let Some(existing) = register_download_task(state, &task).await {
        if let Some(charge) = charge {
            charge.refund();
        }
        return Ok(existing);
    }

    if let Some(cache) = cached.filter(|cache| !cache.file_id.trim().is_empty()) {
        complete_task_from_cache(state, &id, &track_id, &cache).await?;
        return state
            .downloads
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| ApiError::Custom("Download not found".to_string()));
    }

    tokio::spawn(run_download(state.clone(), id, charge));
    Ok(task)
}

async fn register_download_task(state: &AppState, task: &DownloadTask) -> Option<DownloadTask> {
    let mut downloads = state.downloads.write().await;
    if let Some(existing) = downloads
        .get_mut(&task.id)
        .filter(|existing| existing.status != DownloadStatus::Failed)
    {
        upgrade_completed_alac_transport(existing, task.streaming_mode.as_deref());
        audio_cache::touch(&existing.work_id);
        return Some(task_for_client(existing, task.streaming_mode.as_deref()));
    }
    audio_cache::touch(&task.work_id);
    downloads.insert(task.id.clone(), task.clone());
    None
}

async fn get_download(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(wait): Query<PlaybackStatusQuery>,
) -> ApiResult<Json<PlaybackTask>> {
    let wait_for = Duration::from_millis(wait.wait_ms.unwrap_or(0)).min(PLAYBACK_WAIT_MAX);
    let deadline = tokio::time::Instant::now() + wait_for;
    loop {
        // Register before reading so a transition between the read and the
        // wait cannot be missed.
        let changed = state.task_events.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let task = load_task(&state, &id).await?;
        let unchanged = wait
            .since
            .as_deref()
            .is_some_and(|since| since.eq_ignore_ascii_case(task.status.as_str()));
        let now = tokio::time::Instant::now();
        if !unchanged || now >= deadline {
            return Ok(Json(public_task(&state, &task)));
        }
        let _ = tokio::time::timeout((deadline - now).min(PLAYBACK_WAIT_RECHECK), changed).await;
    }
}

fn notify_task_change(state: &AppState) {
    state.task_events.notify_waiters();
}

async fn playback_loudness(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<RecommendationJson<LoudnessResponse>> {
    let task = load_task(&state, &id).await?;
    let response = match task.status {
        DownloadStatus::Queued | DownloadStatus::Processing | DownloadStatus::Streaming => {
            LoudnessResponse::pending()
        }
        DownloadStatus::Failed => LoudnessResponse::unavailable(),
        DownloadStatus::Completed => completed_task_loudness(&state, &task).await,
    };
    // A pending / unavailable response must not hide a later measurement.
    Ok(recommendation_json(response))
}

async fn completed_task_loudness(state: &AppState, task: &DownloadTask) -> LoudnessResponse {
    let Some(filename) = task.filename.as_deref() else {
        return LoudnessResponse::unavailable();
    };
    let downloads = state.data_dir.join("downloads");
    let path = downloads.join(&task.work_id).join(filename);
    let (Ok(root), Ok(path)) = (
        fs::canonicalize(&downloads).await,
        fs::canonicalize(path).await,
    ) else {
        return LoudnessResponse::unavailable();
    };
    if !path.starts_with(root) {
        return LoudnessResponse::unavailable();
    }
    // Inspect only the completed audio object already on disk. In particular,
    // do not call ensure_local_audio_file here: a Telegram-only cache entry
    // must never trigger a network download just to measure loudness. Packet
    // sidecars are not an audio-container input for FFmpeg either.
    let identity = format!(
        "{}\0{}\0{}\0{}\0{}",
        task.platform,
        task.track_id
            .as_deref()
            .or(task.selected_id.as_deref())
            .unwrap_or(&task.id),
        task.quality,
        task.format.as_deref().unwrap_or_default(),
        task.content_type.as_deref().unwrap_or_default(),
    );
    loudness::request_completed_file(
        &path,
        task.byte_size,
        &identity,
        &state.data_dir.join("loudness").join("v1"),
    )
    .await
}

async fn download_lyrics(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<LyricsResponse>> {
    let task = load_task(&state, &id).await?;
    let platform = music::parse_platform(&task.platform)?;
    let track_id = task
        .track_id
        .as_deref()
        .or(task.selected_id.as_deref())
        .unwrap_or(task.query.as_str())
        .trim();
    if track_id.is_empty() {
        return Err(ApiError::Custom("track_id 不能为空".to_string()));
    }
    Ok(Json(lyrics_response(
        platform.id(),
        track_id,
        upstream_limit::metered(
            upstream_limit::Kind::Lyrics,
            music::lyrics_for_track(
                platform,
                track_id,
                task.title.as_deref().unwrap_or_default(),
                task.artist.as_deref().unwrap_or_default(),
                task.album.as_deref().unwrap_or_default(),
                task.duration,
            ),
        )
        .await?,
    )))
}

async fn download_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(version): Query<MediaVersionQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    serve_audio_file(state, id, &headers, version.rev.as_deref()).await
}

/// Serve a completed song's stored ALAC M4A as is, for the WASM player to
/// demux. It is read straight from the Telegram cache (or the finished local
/// download), so replays need neither packet conversion nor a local copy.
async fn download_source(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(version): Query<MediaVersionQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let task = load_task(&state, &id).await?;
    if version
        .rev
        .as_deref()
        .is_some_and(|rev| rev != source_media_revision(&task))
    {
        return Err(ApiError::Http {
            status: StatusCode::CONFLICT,
            message: "音频版本已更新，请重新获取播放任务地址".to_string(),
        });
    }
    if !serves_alac_source(&task) {
        return Err(ApiError::Http {
            status: StatusCode::CONFLICT,
            message: "该播放任务没有可直接读取的 ALAC 原文件".to_string(),
        });
    }
    let filename = task.filename.as_deref().unwrap_or("audio.m4a");
    let local = state
        .data_dir
        .join("downloads")
        .join(&task.work_id)
        .join(filename);
    if fs::metadata(&local)
        .await
        .is_ok_and(|meta| meta.is_file() && meta.len() > 0)
    {
        return stream_local_file(&local, filename, "audio/mp4", &headers).await;
    }
    let Some(file_id) = task.file_id.as_deref().filter(|id| !id.trim().is_empty()) else {
        return Err(ApiError::Http {
            status: StatusCode::NOT_FOUND,
            message: "音频文件不存在".to_string(),
        });
    };
    stream_telegram_file(
        file_id,
        filename,
        Some("audio/mp4".to_string()),
        task.byte_size,
        &headers,
    )
    .await
}

/// Serve SPLALAC1 packets to a capable WebAssembly decoder. The regular
/// `/audio` endpoint keeps its browser-compatible Lossless/Hi-Res derivative.
async fn download_stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(version): Query<MediaVersionQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let task = load_task(&state, &id).await?;
    validate_media_revision(&task, true, version.rev.as_deref())?;
    if task.streaming_mode.as_deref() != Some("wasm_alac") {
        return Err(ApiError::Http {
            status: StatusCode::NOT_ACCEPTABLE,
            message: "该播放任务未请求 WASM ALAC 流式传输".to_string(),
        });
    }
    if task.status == DownloadStatus::Streaming {
        return stream_growing_audio_file(state, &task, true, &headers).await;
    }
    if task.status != DownloadStatus::Completed {
        return Err(ApiError::Http {
            status: StatusCode::CONFLICT,
            message: format!("下载尚未完成，当前状态为 {:?}", task.status),
        });
    }
    // Never return raw M4A from this endpoint: its client has already chosen
    // the packet decoder. Recover an evicted sidecar from completed audio.
    let raw_path = task.filename.as_deref().map(|filename| {
        state
            .data_dir
            .join("downloads")
            .join(&task.work_id)
            .join(filename)
    });
    if let Some(raw_path) = raw_path {
        // During the worker's final remux the packet stream keeps the
        // `.streaming` filename while DownloadTask switches back to the
        // completed audio filename. Try both names so a reload that lands
        // after completion still receives SPLALAC1 instead of raw M4A.
        let filename = task.filename.as_deref().unwrap_or("audio.m4a");
        let candidates = [
            music::wasm_alac_stream_path(&raw_path),
            raw_path.with_file_name(format!("{filename}.streaming.alacstream")),
        ];
        for packet_path in candidates {
            if fs::metadata(&packet_path)
                .await
                .is_ok_and(|meta| meta.len() > 24)
            {
                return stream_local_file(
                    &packet_path,
                    &format!("{}.alacstream", ascii_filename(filename)),
                    "application/x-splayer-alac",
                    &headers,
                )
                .await;
            }
        }
    }
    let audio_path = ensure_local_audio_file(&state, &task).await?;
    let packet_path = music::wasm_alac_stream_path(&audio_path);
    let _packet_guard = lock_audio_path(&packet_path).await;
    if fs::metadata(&packet_path)
        .await
        .is_ok_and(|meta| meta.is_file() && meta.len() > 24)
    {
        return stream_local_file(
            &packet_path,
            "audio.alacstream",
            "application/x-splayer-alac",
            &headers,
        )
        .await;
    }
    let audio = fs::read(&audio_path).await?;
    let packets =
        tokio::task::spawn_blocking(move || music::alac_transport_from_progressive(&audio))
            .await??;
    // Concurrent restores may finish together. Publish only a complete file;
    // readers must never mistake a partly written sidecar for a valid cache.
    let temporary = PendingAudioFile::new(&packet_path)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .open(temporary.path())
        .await?;
    output.write_all(&packets).await?;
    output.flush().await?;
    drop(output);
    fs::rename(temporary.path(), &packet_path).await?;
    stream_local_file(
        &packet_path,
        "audio.alacstream",
        "application/x-splayer-alac",
        &headers,
    )
    .await
}

async fn download_cover(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    serve_task_file(state, id, FileKind::Cover, &headers).await
}

fn media_revision(task: &DownloadTask, packets: bool) -> String {
    // This is a generation/version token, not a content hash. Bump the
    // representation schema when an encoder's byte format changes. Native
    // files and SPLALAC packet files must never share a CDN cache entry.
    let representation = if packets {
        "splayer-alac-v2"
    } else if task.platform == "applemusic" {
        // v2 relabelled AAC sample entries `alac` -> `mp4a`; v3 also serves
        // m4a as audio/mp4. Safari needs both; the CDN keeps headers too.
        "native-v3"
    } else {
        // v2: m4a is served as audio/mp4 instead of audio/m4a.
        "native-v2"
    };
    format!(
        "{:x}",
        md5::compute(format!("{}\0{representation}", task.media_revision))
    )
}

fn media_url(state: &AppState, task: &DownloadTask, packets: bool) -> String {
    let suffix = if packets { "/audio/stream" } else { "/audio" };
    public_media_url(state, task, suffix, &media_revision(task, packets))
}

/// Revision of the stored ALAC M4A served by `/audio/source`.
fn source_media_revision(task: &DownloadTask) -> String {
    format!(
        "{:x}",
        md5::compute(format!("{}\0alac-source-v1", task.media_revision))
    )
}

fn source_media_url(state: &AppState, task: &DownloadTask) -> String {
    public_media_url(state, task, "/audio/source", &source_media_revision(task))
}

/// Completed Apple Music ALAC kept as a progressive M4A: the file Telegram
/// stores, which the WASM player reads and demuxes without conversion.
fn serves_alac_source(task: &DownloadTask) -> bool {
    task.status == DownloadStatus::Completed
        && task.platform == "applemusic"
        && task.filename.as_deref().is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name.ends_with(".m4a") || name.ends_with(".mp4")
        })
        && needs_browser_lossless_derivative(
            &task.quality,
            task.format.as_deref(),
            task.content_type.as_deref(),
            task.filename.as_deref(),
        )
}

fn public_media_url(state: &AppState, task: &DownloadTask, suffix: &str, revision: &str) -> String {
    let path = format!("/api/v1/playbacks/{}{suffix}?rev={revision}", task.id);
    if task.status == DownloadStatus::Completed
        && let Some(base) = &state.media_base_url
    {
        // Config accepts origins only, so joining cannot retain credentials,
        // hidden query parameters, or a surprising deployment path prefix.
        return base
            .join(&path)
            .expect("generated media path is valid")
            .to_string();
    }
    path
}

fn validate_media_revision(
    task: &DownloadTask,
    packets: bool,
    requested: Option<&str>,
) -> ApiResult<()> {
    let expected = media_revision(task, packets);
    // Previously deployed clients appended `?v=task.id` even when a URL
    // already had a query. Axum's query extractor has decoded that suffix by
    // this point. Accept only this exact legacy spelling for this exact task
    // and current representation; never strip arbitrary trailing input.
    let legacy = format!("{expected}?v={}", task.id);
    if requested.is_some_and(|value| value != expected && value != legacy) {
        return Err(ApiError::Http {
            status: StatusCode::CONFLICT,
            message: "音频版本已更新，请重新获取播放任务地址".to_string(),
        });
    }
    Ok(())
}

fn cache_media_revision(cache: &MusicCache) -> String {
    // Telegram file_id identifies immutable uploaded bytes. A replacement
    // upload and legacy FLAC/new ALAC cache entries get different generations.
    // Only its opaque digest is public; never expose the upstream identifier.
    format!(
        "{:x}",
        md5::compute(format!(
            "media-generation-v1\0{}\0{}\0{}",
            cache.file_id, cache.file_ext, cache.music_size
        ))
    )
}

fn generation_work_id(id: &str, revision: &str) -> String {
    format!("{id}--{revision}")
}

fn public_task(state: &AppState, task: &DownloadTask) -> PlaybackTask {
    PlaybackTask {
        id: task.id.clone(),
        media_revision: task.media_revision.clone(),
        status: task.status,
        platform: task.platform.clone(),
        query: task.query.clone(),
        track_id: task.track_id.clone(),
        selected_id: task.selected_id.clone(),
        quality: task.quality.clone(),
        include_cover: task.include_cover,
        title: task.title.clone(),
        artist: task.artist.clone(),
        album: task.album.clone(),
        source_url: task.source_url.clone(),
        content_type: task.content_type.clone(),
        byte_size: task.byte_size,
        bitrate: task.bitrate,
        duration: task.duration,
        format: task.format.clone(),
        decrypt_elapsed_seconds: task.decrypt_elapsed_seconds,
        error: task.error.clone(),
        error_code: task.error_code.clone(),
        streaming_mode: task.streaming_mode.clone(),
        streaming_codec: task
            .streaming_mode
            .as_deref()
            .map(|mode| match mode {
                "wasm_alac" => "alac".to_string(),
                _ => "".to_string(),
            })
            .filter(|codec| !codec.is_empty()),
        streaming_protocol: (task.streaming_mode.as_deref() == Some("wasm_alac"))
            .then_some("splayer-alac-v2".to_string()),
        audio_url: media_url(state, task, false),
        streaming_url: task
            .streaming_mode
            .as_deref()
            .map(|_| media_url(state, task, true)),
        alac_source_url: serves_alac_source(task).then(|| source_media_url(state, task)),
        stream_length: task
            .stream_total
            .filter(|_| task.status == DownloadStatus::Streaming),
        cover_url: format!("/api/v1/playbacks/{}/cover", task.id),
        lyrics_url: format!("/api/v1/playbacks/{}/lyrics", task.id),
        loudness_estimate: task
            .replay_gain_centi_db
            .and_then(crate::loudness::LoudnessResponse::from_replay_gain),
    }
}

fn create_download_response(state: &AppState, task: &DownloadTask) -> CreateDownloadResponse {
    CreateDownloadResponse {
        playback: public_task(state, task),
        status_url: format!("/api/v1/playbacks/{}", task.id),
        audio_url: media_url(state, task, false),
        cover_url: format!("/api/v1/playbacks/{}/cover", task.id),
        lyrics_url: format!("/api/v1/playbacks/{}/lyrics", task.id),
    }
}

async fn run_download(state: AppState, id: String, mut charge: Option<upstream_limit::Charge>) {
    let result = upstream_limit::within(
        upstream_limit::Kind::Download,
        run_download_inner(state.clone(), id.clone(), &mut charge),
    )
    .await;
    if let Err(err) = result {
        let mut downloads = state.downloads.write().await;
        if let Some(task) = downloads.get_mut(&id) {
            task.status = DownloadStatus::Failed;
            task.error = Some(err.to_string());
            task.error_code =
                matches!(err, ApiError::Unplayable(_)).then(|| UNPLAYABLE_CODE.into());
        }
        drop(downloads);
        notify_task_change(&state);
    }
}

/// `charge` is the client's download unit; it is refunded when the song turns
/// out to be live or cached after all and the platform is not needed.
async fn run_download_inner(
    state: AppState,
    id: String,
    charge: &mut Option<upstream_limit::Charge>,
) -> ApiResult<()> {
    let refund = |charge: &mut Option<upstream_limit::Charge>| {
        if let Some(charge) = charge.take() {
            charge.refund();
        }
    };
    let task = {
        let mut downloads = state.downloads.write().await;
        let task = downloads
            .get_mut(&id)
            .ok_or_else(|| ApiError::Custom("Download not found".to_string()))?;
        task.status = DownloadStatus::Processing;
        task.clone()
    };
    notify_task_change(&state);

    let platform = music::parse_platform(&task.platform)?;
    // create_download looked up the Telegram cache for this exact
    // platform/track/quality immediately before spawning this worker, so a
    // repeated lookup here would only add a database round trip.
    let checked_track_id = task
        .selected_id
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if let Some(selected_id) = task
        .selected_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        && let Some(inflight) =
            find_inflight(&state, platform.id(), selected_id, &task.quality).await
    {
        refund(charge);
        complete_task_from_inflight(&state, &id, &inflight).await?;
        return Ok(());
    }

    let track = music::resolve(ResolveOptions {
        platform,
        query: task.query.clone(),
        selected_id: task.selected_id.clone(),
        quality: Some(task.quality.clone()),
    })
    .await?;
    let mut task = task;
    let substituted = adopt_served_quality(&mut task, &track);
    if substituted {
        if let Some(current) = state.downloads.write().await.get_mut(&id) {
            adopt_served_quality(current, &track);
        }
        notify_task_change(&state);
    }
    if (substituted || track.id != checked_track_id)
        && let Some(cache) = find_music_cache(track.platform.id(), &track.id, &task.quality).await?
        && !cache.file_id.trim().is_empty()
    {
        refund(charge);
        complete_task_from_cache(&state, &id, &track.id, &cache).await?;
        return Ok(());
    }
    publish_resolved_track(&state, &id, &track).await;
    let inflight_key = inflight_key(track.platform.id(), &track.id, &task.quality);
    let (inflight, owns_inflight) = register_inflight(&state, inflight_key.clone()).await;
    if !owns_inflight {
        refund(charge);
        complete_task_from_inflight(&state, &id, &inflight).await?;
        return Ok(());
    }

    let progressive = music::can_stream_progressive_http(&track);
    let can_stream =
        progressive || music::can_stream_track_with_mode(&track, task.streaming_mode.as_deref());
    if (progressive || !can_stream) && task.streaming_mode.is_some() {
        // Invalid capability/quality combinations must return the ordinary
        // completed-file contract instead of advertising a packet endpoint
        // that contains raw M4A bytes. A plain MP3/FLAC file is always the
        // native contract, whatever packet transport the client offered.
        task.streaming_mode = None;
        if let Some(current) = state.downloads.write().await.get_mut(&id) {
            current.streaming_mode = None;
        }
    }
    let result = if can_stream {
        download_provider_track_streaming(
            state.clone(),
            id.clone(),
            task,
            track,
            inflight_key.clone(),
            inflight.clone(),
        )
        .await
    } else {
        download_provider_track(
            state.clone(),
            id.clone(),
            task,
            track,
            inflight_key.clone(),
            inflight.clone(),
        )
        .await
    };
    if let Err(err) = &result {
        mark_inflight_failed(&state, &inflight_key, &inflight, err.to_string()).await;
    }
    result
}

/// Lets status readers name the song while it is still downloading.
/// Relabels a task with the quality the provider will actually serve (an
/// Apple song with no lossless asset plays as High AAC). The cache row,
/// inflight key and transport all follow, so AAC is never offered to the
/// browser as ALAC packets or stored as a lossless file.
fn adopt_served_quality(task: &mut DownloadTask, track: &music::MusicTrack) -> bool {
    let Some(served) = track
        .served_quality
        .as_deref()
        .filter(|served| !served.eq_ignore_ascii_case(&task.quality))
    else {
        return false;
    };
    task.quality = served.to_string();
    task.streaming_mode = None;
    true
}

async fn publish_resolved_track(state: &AppState, id: &str, track: &music::MusicTrack) {
    if let Some(task) = state.downloads.write().await.get_mut(id) {
        task.track_id = Some(track.id.clone());
        task.title.get_or_insert_with(|| track.song.clone());
        task.artist.get_or_insert_with(|| track.singer.clone());
        task.album.get_or_insert_with(|| track.album.clone());
        task.source_url.get_or_insert_with(|| track.link.clone());
        if !track.cover.trim().is_empty() {
            task.cover_url.get_or_insert_with(|| track.cover.clone());
        }
    }
}

async fn download_provider_track(
    state: AppState,
    id: String,
    task: DownloadTask,
    track: music::MusicTrack,
    inflight_key: String,
    inflight: Arc<InflightDownload>,
) -> ApiResult<()> {
    let transfer = task.transfer.clone();
    let mut progress = |progress| transfer.record(progress);
    // The Telegram upload is shared by every client, so it always carries the
    // cover whatever this request asked for.
    let media = music::download(&track, true, &mut progress).await?;

    let dir = audio_work_dir(&state, &task.work_id);
    fs::create_dir_all(&dir).await?;
    let audio_filename = sanitize_filename(&track.file_name());
    let audio_path = dir.join(&audio_filename);
    fs::write(&audio_path, &media.audio).await?;
    let audio_content_type = content_type(&audio_path);

    let cover_bytes = media.cover;
    let cover_filename = if cover_bytes.is_empty() {
        None
    } else {
        let cover_name = "cover.jpg".to_string();
        fs::write(dir.join(&cover_name), &cover_bytes).await?;
        Some(cover_name)
    };
    let audio_len = media.audio.len();
    let audio_bytes = media.audio;
    let upload_chat_id = cache_upload_chat_id();
    let cache = MusicCache {
        platform: track.platform.id().to_string(),
        track_id: track.id.clone(),
        quality: task.quality.clone(),
        song: track.song.clone(),
        singer: track.singer.clone(),
        album: track.album.clone(),
        link: track.link.clone(),
        file_ext: track.file_extension(),
        music_size: audio_len.min(i64::MAX as usize) as i64,
        bitrate: track
            .bitrate
            .or_else(|| estimate_bitrate(audio_len as u64, track.duration))
            .unwrap_or_default()
            .min(i32::MAX as u32) as i32,
        duration: track
            .duration
            .map(|duration| duration.min(i32::MAX as u32) as i32),
        file_id: String::new(),
        thumb_file_id: String::new(),
        from_user_id: SETTINGS.bot.owner,
        from_chat_id: upload_chat_id,
        lyrics_tag: 0,
    };
    record_upload_attempt(&state, &id, &task.work_id).await;
    tokio::spawn(upload_audio_to_telegram_cache(TelegramCacheUpload {
        state: state.clone(),
        task_id: id.clone(),
        work_id: task.work_id.clone(),
        inflight_key,
        chat_id: upload_chat_id,
        filename: audio_filename.clone(),
        content_type: audio_content_type.clone(),
        audio: audio_bytes,
        cover: cover_bytes,
        title: track.song.clone(),
        performer: track.singer.clone(),
        duration: track.duration,
        cache,
    }));

    let mut downloads = state.downloads.write().await;
    let task = downloads
        .get_mut(&id)
        .ok_or_else(|| ApiError::Custom("Download not found".to_string()))?;
    task.status = DownloadStatus::Completed;
    task.track_id = Some(track.id.clone());
    task.title = Some(track.song);
    task.artist = Some(track.singer);
    task.album = Some(track.album);
    task.source_url = Some(track.link);
    task.cover_url = Some(track.cover).filter(|cover| !cover.trim().is_empty());
    task.filename = Some(audio_filename);
    task.cover_filename = cover_filename;
    task.file_id = None;
    task.thumb_file_id = None;
    task.content_type = Some(audio_content_type);
    task.byte_size = Some(audio_len as u64);
    task.bitrate = track.bitrate;
    task.duration = track.duration;
    task.format = track.format;
    task.replay_gain_centi_db = track.replay_gain_centi_db;
    task.decrypt_elapsed_seconds = media.decrypt_elapsed.map(duration_seconds);
    task.error = None;
    task.error_code = None;
    let completed_task = task.clone();
    drop(downloads);
    notify_task_change(&state);
    mark_inflight_ready(&inflight, completed_task).await;
    Ok(())
}

async fn download_provider_track_streaming(
    state: AppState,
    id: String,
    task: DownloadTask,
    track: music::MusicTrack,
    inflight_key: String,
    inflight: Arc<InflightDownload>,
) -> ApiResult<()> {
    let dir = audio_work_dir(&state, &task.work_id);
    fs::create_dir_all(&dir).await?;
    let audio_filename = sanitize_filename(&track.file_name());
    let streaming_filename = format!("{audio_filename}.streaming");
    let streaming_path = dir.join(&streaming_filename);
    let audio_path = dir.join(&audio_filename);
    let progressive = music::can_stream_progressive_http(&track);
    let (audio_content_type, streaming_format) = if progressive {
        let ext = track.file_extension();
        (content_type_from_ext(&ext), ext)
    } else {
        ("audio/mp4".to_string(), "m4a".to_string())
    };

    // Publish metadata before the first fragment arrives so the client can
    // immediately resolve the growing endpoint once status becomes streaming.
    {
        let mut downloads = state.downloads.write().await;
        let current = downloads
            .get_mut(&id)
            .ok_or_else(|| ApiError::Custom("Download not found".to_string()))?;
        current.status = DownloadStatus::Processing;
        current.track_id = Some(track.id.clone());
        current.title = Some(track.song.clone());
        current.artist = Some(track.singer.clone());
        current.album = Some(track.album.clone());
        current.source_url = Some(track.link.clone());
        current.cover_url = Some(track.cover.clone()).filter(|cover| !cover.trim().is_empty());
        current.filename = Some(streaming_filename.clone());
        current.content_type = Some(audio_content_type.clone());
        current.bitrate = track.bitrate;
        current.duration = track.duration;
        current.replay_gain_centi_db = track.replay_gain_centi_db;
        current.format = Some(streaming_format);
        current.stream_total = None;
        current.error = None;
        current.error_code = None;
    }
    notify_task_change(&state);

    // The shared Telegram upload always carries the cover. Fetch it while the
    // audio streams, so completion does not wait for another request after
    // the last audio byte.
    let cover_track = track.clone();
    let cover_download = tokio::spawn(async move { music::download_cover(&cover_track).await });
    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::unbounded_channel::<Option<u64>>();
    let ready_state = state.clone();
    let ready_id = id.clone();
    let ready_listener = tokio::spawn(async move {
        if let Some(total) = ready_rx.recv().await
            && let Some(task) = ready_state.downloads.write().await.get_mut(&ready_id)
        {
            task.status = DownloadStatus::Streaming;
            task.stream_total = total;
        }
        notify_task_change(&ready_state);
    });
    let streamed = if progressive {
        let mut on_ready = |total| {
            let _ = ready_tx.send(total);
        };
        music::stream_http_track_to_path(&track, &streaming_path, &mut on_ready)
            .await
            .map(|bytes| (bytes, None))
    } else {
        let mut on_ready = || {
            let _ = ready_tx.send(None);
        };
        music::stream_track_to_path_with_mode(
            &track,
            &streaming_path,
            task.streaming_mode.as_deref(),
            &mut on_ready,
        )
        .await
    };
    drop(ready_tx);
    let _ = ready_listener.await;
    let (audio_bytes, decrypt_elapsed) = match streamed {
        Ok(result) => result,
        Err(error) => {
            let started = state
                .downloads
                .read()
                .await
                .get(&id)
                .is_some_and(|current| current.status == DownloadStatus::Streaming);
            if !started {
                tracing::warn!(
                    playback_id = %id,
                    error = %streaming_fallback_reason(&error),
                    "Incremental audio unavailable before first fragment; using complete download"
                );
                let _ = fs::remove_file(&streaming_path).await;
                let _ = fs::remove_file(format!("{}.assembled", streaming_path.display())).await;
                let _ = fs::remove_file(music::wasm_alac_stream_path(&streaming_path)).await;
                // Preserve the old complete-download behavior when the
                // incremental parser cannot handle a provider variant before
                // any browser bytes have been exposed. Clear the capability
                // marker so clients do not mistake the completed M4A for a
                // packet stream.
                if let Some(current) = state.downloads.write().await.get_mut(&id) {
                    current.streaming_mode = None;
                }
                cover_download.abort();
                return download_provider_track(state, id, task, track, inflight_key, inflight)
                    .await;
            }
            cover_download.abort();
            return Err(error);
        }
    };

    fs::write(&audio_path, &audio_bytes).await?;
    // Existing chunked readers may still be draining the final fragment when
    // the progressive file becomes available. Keep the growing file around
    // briefly, then reclaim it without truncating those readers.
    let cleanup_streaming_path = streaming_path.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30 * 60)).await;
        let _ = fs::remove_file(cleanup_streaming_path).await;
        // Keep `.alacstream` for completed WASM tasks. A later replay may
        // still use the packet endpoint, and deleting it would silently
        // expose raw M4A bytes to the packet parser.
    });
    let audio_content_type = content_type(&audio_path);
    let cover_bytes = match cover_download.await {
        Ok(cover) => cover?,
        Err(_) => Vec::new(),
    };
    let cover_filename = if cover_bytes.is_empty() {
        None
    } else {
        let cover_name = "cover.jpg".to_string();
        fs::write(dir.join(&cover_name), &cover_bytes).await?;
        Some(cover_name)
    };
    let audio_len = audio_bytes.len();
    let upload_chat_id = cache_upload_chat_id();
    let cache = MusicCache {
        platform: track.platform.id().to_string(),
        track_id: track.id.clone(),
        quality: task.quality.clone(),
        song: track.song.clone(),
        singer: track.singer.clone(),
        album: track.album.clone(),
        link: track.link.clone(),
        file_ext: track.file_extension(),
        music_size: audio_len.min(i64::MAX as usize) as i64,
        bitrate: track
            .bitrate
            .or_else(|| estimate_bitrate(audio_len as u64, track.duration))
            .unwrap_or_default()
            .min(i32::MAX as u32) as i32,
        duration: track
            .duration
            .map(|duration| duration.min(i32::MAX as u32) as i32),
        file_id: String::new(),
        thumb_file_id: String::new(),
        from_user_id: SETTINGS.bot.owner,
        from_chat_id: upload_chat_id,
        lyrics_tag: 0,
    };
    record_upload_attempt(&state, &id, &task.work_id).await;
    tokio::spawn(upload_audio_to_telegram_cache(TelegramCacheUpload {
        state: state.clone(),
        task_id: id.clone(),
        work_id: task.work_id.clone(),
        inflight_key,
        chat_id: upload_chat_id,
        filename: audio_filename.clone(),
        content_type: audio_content_type.clone(),
        audio: audio_bytes,
        cover: cover_bytes,
        title: track.song.clone(),
        performer: track.singer.clone(),
        duration: track.duration,
        cache,
    }));

    let mut downloads = state.downloads.write().await;
    let current = downloads
        .get_mut(&id)
        .ok_or_else(|| ApiError::Custom("Download not found".to_string()))?;
    current.status = DownloadStatus::Completed;
    current.filename = Some(audio_filename);
    current.cover_filename = cover_filename;
    current.file_id = None;
    current.thumb_file_id = None;
    current.content_type = Some(audio_content_type);
    current.byte_size = Some(audio_len as u64);
    current.decrypt_elapsed_seconds = decrypt_elapsed.map(duration_seconds);
    current.stream_total = None;
    current.error = None;
    current.error_code = None;
    let completed_task = current.clone();
    drop(downloads);
    notify_task_change(&state);
    mark_inflight_ready(&inflight, completed_task).await;
    Ok(())
}

fn streaming_fallback_reason(error: &ApiError) -> String {
    // reqwest's Display includes the request URL, potentially with a signed
    // CDN query. Keep actionable local parser errors without exposing URLs.
    if matches!(error, ApiError::Request(_) | ApiError::Retry(_)) {
        return "provider HTTP request failed before the first audio fragment".to_string();
    }
    redact_error_message(error)
}

/// An error's text without URLs, which may carry signed queries or tokens.
fn redact_error_message(error: &(impl std::fmt::Display + ?Sized)) -> String {
    lazy_static::lazy_static! {
        static ref URLS: regex::Regex =
            regex::Regex::new(r#"(?i)(?:https?|skd)://[^\s\"'<>]+"#).unwrap();
    }
    URLS.replace_all(&error.to_string(), "[redacted URL]")
        .chars()
        .take(500)
        .collect()
}

async fn complete_task_from_cache(
    state: &AppState,
    id: &str,
    track_id: &str,
    cache: &MusicCache,
) -> ApiResult<()> {
    let mut downloads = state.downloads.write().await;
    let task = downloads
        .get_mut(id)
        .ok_or_else(|| ApiError::Custom("Download not found".to_string()))?;
    let requested_mode = task.streaming_mode.clone();
    task.status = DownloadStatus::Completed;
    task.media_revision = cache_media_revision(cache);
    task.work_id = generation_work_id(id, &task.media_revision);
    task.track_id = Some(track_id.to_string());
    task.title = Some(cache.song.clone());
    task.artist = Some(cache.singer.clone());
    task.album = Some(cache.album.clone());
    task.source_url = Some(cache.link.clone());
    task.filename = Some(cache_audio_filename(cache));
    task.cover_filename = None;
    task.file_id = Some(cache.file_id.clone());
    task.thumb_file_id =
        (!cache.thumb_file_id.trim().is_empty()).then_some(cache.thumb_file_id.clone());
    task.content_type = Some(content_type_from_ext(&cache.file_ext));
    task.byte_size = Some(cache.music_size.max(0) as u64);
    task.bitrate = (cache.bitrate > 0).then_some(cache.bitrate as u32);
    task.duration = cache.duration.map(|duration| duration.max(0) as u32);
    task.format = (!cache.file_ext.trim().is_empty()).then_some(cache.file_ext.clone());
    task.decrypt_elapsed_seconds = None;
    task.error = None;
    task.error_code = None;
    // A capable client can reconstruct packets from the completed ALAC source,
    // avoiding a full FLAC transcode. Native clients retain their file endpoint.
    task.streaming_mode = None;
    upgrade_completed_alac_transport(task, requested_mode.as_deref());
    drop(downloads);
    notify_task_change(state);
    Ok(())
}

async fn complete_task_from_inflight(
    state: &AppState,
    id: &str,
    inflight: &Arc<InflightDownload>,
) -> ApiResult<()> {
    loop {
        let notified = inflight.notify.notified();
        {
            let inner = inflight.inner.read().await;
            if let Some(source) = &inner.task {
                let mut task = source.clone();
                task.id = id.to_string();
                task.status = DownloadStatus::Completed;
                task.error = None;
                task.error_code = None;
                // Inflight work is shared by platform/track/quality, while
                // `streaming_mode` is a client capability. A native client
                // joining a WASM request must never inherit the packet URL.
                let requested_wasm =
                    state.downloads.read().await.get(id).is_some_and(|current| {
                        current.streaming_mode.as_deref() == Some("wasm_alac")
                    });
                if !requested_wasm {
                    task.streaming_mode = None;
                } else {
                    upgrade_completed_alac_transport(&mut task, Some("wasm_alac"));
                }
                state.downloads.write().await.insert(id.to_string(), task);
                notify_task_change(state);
                return Ok(());
            }
            if let Some(error) = &inner.error {
                return Err(ApiError::Custom(format!(
                    "同曲目下载任务失败，无法复用：{error}"
                )));
            }
        }
        notified.await;
    }
}

async fn find_inflight(
    state: &AppState,
    platform: &str,
    track_id: &str,
    quality: &str,
) -> Option<Arc<InflightDownload>> {
    state
        .inflight
        .read()
        .await
        .get(&inflight_key(platform, track_id, quality))
        .cloned()
}

async fn register_inflight(state: &AppState, key: String) -> (Arc<InflightDownload>, bool) {
    let mut inflight = state.inflight.write().await;
    if let Some(existing) = inflight.get(&key) {
        return (existing.clone(), false);
    }
    let entry = Arc::new(InflightDownload::default());
    inflight.insert(key, entry.clone());
    (entry, true)
}

async fn mark_inflight_ready(inflight: &Arc<InflightDownload>, task: DownloadTask) {
    let mut inner = inflight.inner.write().await;
    inner.task = Some(task);
    inner.error = None;
    drop(inner);
    inflight.notify.notify_waiters();
}

async fn mark_inflight_failed(
    state: &AppState,
    key: &str,
    inflight: &Arc<InflightDownload>,
    error: String,
) {
    let mut inner = inflight.inner.write().await;
    inner.error = Some(error);
    drop(inner);
    inflight.notify.notify_waiters();
    state.inflight.write().await.remove(key);
}

async fn remove_inflight(state: &AppState, key: &str) {
    state.inflight.write().await.remove(key);
}

struct TelegramCacheUpload {
    state: AppState,
    task_id: String,
    work_id: String,
    inflight_key: String,
    chat_id: i64,
    filename: String,
    content_type: String,
    audio: Vec<u8>,
    cover: Vec<u8>,
    title: String,
    performer: String,
    duration: Option<u32>,
    cache: MusicCache,
}

lazy_static::lazy_static! {
    /// One upload at a time per downloaded file, so two qualities that got
    /// the same bytes concurrently upload them once.
    static ref UPLOADS_BY_SOURCE: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> =
        std::sync::Mutex::new(HashMap::new());
}

fn audio_sha256(audio: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(audio))
}

/// `cache` pointed at `existing`'s upload, keeping its own key and metadata.
fn reuse_upload(mut cache: MusicCache, existing: &MusicCache) -> MusicCache {
    cache.file_id = existing.file_id.clone();
    cache.thumb_file_id = existing.thumb_file_id.clone();
    cache.music_size = existing.music_size;
    cache.lyrics_tag = existing.lyrics_tag;
    cache.from_chat_id = existing.from_chat_id;
    // The upload may be a re-encode in another format (`telegram_audio`).
    if !existing.file_ext.trim().is_empty() && existing.file_ext != cache.file_ext {
        cache.file_ext = existing.file_ext.clone();
        cache.bitrate = existing.bitrate;
    }
    cache
}

/// Telegram keeps only MP3, M4A and FLAC as songs; anything else sent with
/// sendAudio comes back as a voice message or a document, which the bot
/// cannot send as a song. Re-encodes such a file to AAC M4A and points the
/// job and its row at the new format. Returns whether it did.
async fn telegram_audio(job: &mut TelegramCacheUpload) -> ApiResult<bool> {
    if crate::lyric_tags::Container::sniff(&job.audio).is_some() {
        return Ok(false);
    }
    let source_bps = u32::try_from(job.cache.bitrate).ok();
    let kbps = crate::audio::aac_kbps_for(source_bps);
    let started = std::time::Instant::now();
    let m4a = crate::audio::transcode_to_m4a(&job.audio, kbps).await?;
    tracing::info!(
        platform = %job.cache.platform,
        track_id = %job.cache.track_id,
        from = %job.cache.file_ext,
        kbps,
        ms = started.elapsed().as_millis() as u64,
        "re-encoded audio to M4A for the Telegram cache"
    );
    job.audio = m4a;
    job.cache.file_ext = "m4a".to_string();
    job.cache.bitrate = (kbps * 1000) as i32;
    job.content_type = "audio/mp4".to_string();
    let stem = job
        .filename
        .rsplit_once('.')
        .map_or(job.filename.as_str(), |(stem, _)| stem);
    job.filename = format!("{stem}.m4a");
    Ok(true)
}

async fn upload_audio_to_telegram_cache(mut job: TelegramCacheUpload) {
    let audio = std::mem::take(&mut job.audio);
    let (source, audio) =
        match tokio::task::spawn_blocking(move || (audio_sha256(&audio), audio)).await {
            Ok(hashed) => hashed,
            Err(err) => {
                tracing::warn!("failed to hash audio for the telegram cache: {err}");
                return finish_telegram_upload(job, Err(ApiError::Custom(err.to_string()))).await;
            }
        };
    job.audio = audio;
    let gate = UPLOADS_BY_SOURCE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .entry(source.clone())
        .or_default()
        .clone();
    let guard = gate.lock().await;
    let result = upload_or_reuse(&mut job, &source).await;
    drop(guard);
    {
        let mut gates = UPLOADS_BY_SOURCE
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // Ours and the map's: nobody else is waiting on it.
        if Arc::strong_count(&gate) == 2 {
            gates.remove(&source);
        }
    }
    finish_telegram_upload(job, result).await;
}

/// The row to save and whether its file holds exactly the local bytes.
async fn upload_or_reuse(
    job: &mut TelegramCacheUpload,
    source: &str,
) -> ApiResult<(MusicCache, bool)> {
    match find_music_cache_by_source(&job.cache.platform, source).await {
        Ok(Some(existing)) => {
            tracing::info!(
                platform = %job.cache.platform,
                track_id = %job.cache.track_id,
                quality = %job.cache.quality,
                reused_track_id = %existing.track_id,
                reused_quality = %existing.quality,
                "reused a telegram upload of the same audio"
            );
            let cache = reuse_upload(job.cache.clone(), &existing);
            upsert_music_cache(&cache, Some(source)).await?;
            // The reused file may carry lyrics the local copy lacks.
            return Ok((cache, false));
        }
        Ok(None) => {}
        // Uploading again only costs a duplicate.
        Err(err) => tracing::warn!("telegram cache lookup by audio failed: {err}"),
    }

    let reencoded = telegram_audio(job).await?;
    let cover = std::mem::take(&mut job.cover);
    let thumbnail = async move {
        if cover.is_empty() {
            return None;
        }
        match tokio::task::spawn_blocking(move || telegram_cache::audio_thumbnail(&cover)).await {
            Ok(thumbnail) => thumbnail,
            Err(err) => {
                tracing::warn!("failed to prepare telegram audio thumbnail: {err}");
                None
            }
        }
    };
    let audio = std::mem::take(&mut job.audio);
    let (thumbnail, prepared) = tokio::join!(thumbnail, lyric_upload::prepare(&job.cache, audio));
    let uploaded_len = prepared.audio.len();
    let uploaded = telegram_cache::upload_audio(
        job.chat_id,
        &job.filename,
        &job.content_type,
        prepared.audio,
        thumbnail,
        &job.title,
        &job.performer,
        job.duration,
    )
    .await?;

    let mut cache = job.cache.clone();
    cache.file_id = uploaded.file_id;
    cache.thumb_file_id = uploaded.thumb_file_id.unwrap_or_default();
    cache.music_size = uploaded_len.min(i64::MAX as usize) as i64;
    cache.lyrics_tag = prepared.lyrics_tag;
    // The upload exists either way; the next lookup by audio just misses.
    if let Err(err) = upsert_music_cache(&cache, Some(source)).await {
        tracing::warn!("failed to save music cache: {err}");
    }
    Ok((cache, !prepared.embedded && !reencoded))
}

async fn finish_telegram_upload(job: TelegramCacheUpload, result: ApiResult<(MusicCache, bool)>) {
    let (cache, same_bytes) = match result {
        Ok(saved) => saved,
        Err(err) => {
            tracing::warn!("failed to upload audio to telegram cache: {err}");
            if let Some(task) = job.state.downloads.write().await.get_mut(&job.task_id)
                && task.work_id == job.work_id
            {
                task.upload_error = Some(redact_error_message(&err));
            }
            remove_inflight(&job.state, &job.inflight_key).await;
            notify_task_change(&job.state);
            return;
        }
    };
    if let Some(task) = job.state.downloads.write().await.get_mut(&job.task_id)
        && task.work_id == job.work_id
    {
        task.upload_error = None;
        // Let this generation fall back to Telegram once its local files are
        // evicted, keeping its media revision and URLs valid for open
        // clients. Only when Telegram holds the same bytes: a copy with
        // lyrics written in would splice two files under one revision. Such
        // a task is found ready through its cache row, and is dropped once
        // its local files are evicted.
        if same_bytes && task.file_id.is_none() {
            task.file_id = Some(cache.file_id.clone());
            task.thumb_file_id =
                (!cache.thumb_file_id.is_empty()).then(|| cache.thumb_file_id.clone());
        }
    }
    remove_inflight(&job.state, &job.inflight_key).await;
    notify_task_change(&job.state);
}

async fn record_upload_attempt(state: &AppState, id: &str, work_id: &str) {
    if let Some(task) = state.downloads.write().await.get_mut(id)
        && task.work_id == work_id
    {
        task.upload_attempts = task.upload_attempts.saturating_add(1);
        task.upload_error = None;
    }
}

#[derive(Clone, Copy)]
enum FileKind {
    Audio,
    Cover,
}

async fn repair_cached_aac(path: &FsPath) {
    match repair_mislabeled_aac(path).await {
        Ok(true) => tracing::info!("relabelled AAC sample entry in {}", path.display()),
        Ok(false) => {}
        Err(err) => tracing::warn!(
            "failed to check AAC sample entry in {}: {err}",
            path.display()
        ),
    }
}

async fn serve_task_file(
    state: AppState,
    id: String,
    kind: FileKind,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let task = load_task(&state, &id).await?;
    if task.status != DownloadStatus::Completed {
        return Err(ApiError::Http {
            status: StatusCode::CONFLICT,
            message: format!("下载尚未完成，当前状态为 {:?}", task.status),
        });
    }

    let filename = match kind {
        FileKind::Audio => task.filename.clone(),
        FileKind::Cover => task.cover_filename.clone(),
    };
    let mp4_audio = matches!(kind, FileKind::Audio)
        && filename.as_deref().is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name.ends_with(".m4a") || name.ends_with(".mp4")
        });

    if let Some(filename) = filename.as_deref() {
        let path = state
            .data_dir
            .join("downloads")
            .join(&task.work_id)
            .join(filename);
        // A completed task may have both a local download and a Telegram
        // file_id. Prefer the local file so playback does not take a needless
        // network hop; it also preserves the efficient local Range path for
        // seeking/buffering.
        if fs::metadata(&path)
            .await
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        {
            if mp4_audio {
                repair_cached_aac(&path).await;
            }
            let content_type = content_type(&path);
            return stream_local_file(&path, filename, &content_type, headers).await;
        }
        // Apple Music AAC cached on Telegram before the sample-entry fix is
        // mislabelled `alac`, which Safari refuses. Patch those 4 bytes while
        // streaming, so the Telegram copy (sent as is by the bot) stays
        // unchanged and nothing is copied to disk.
        if mp4_audio
            && task.platform == "applemusic"
            && let Some(file_id) = task.file_id.as_deref().filter(|id| !id.trim().is_empty())
        {
            if telegram_cache::cached_local_path(file_id).is_none()
                && let Some(size) = task.byte_size.filter(|size| *size > 0)
                && let Some(patch) = telegram_aac_patch(file_id, size).await
                && let Some(response) = stream_telegram_segments(
                    file_id,
                    filename,
                    "audio/mp4",
                    Some(size),
                    patch,
                    headers,
                )
                .await?
            {
                return Ok(response);
            }
            let file =
                telegram_cache::locate_file(file_id)
                    .await
                    .map_err(|err| ApiError::Http {
                        status: StatusCode::BAD_GATEWAY,
                        message: format!("Telegram 文件信息获取失败：{err}"),
                    })?;
            if let Some(local) = telegram_cache::local_file_path(&file.file_path) {
                let patch = find_mislabeled_aac(&local).await.unwrap_or_else(|err| {
                    tracing::warn!(
                        "failed to check AAC sample entry in {}: {err}",
                        local.display()
                    );
                    None
                });
                return stream_local_file_patched(&local, filename, "audio/mp4", headers, patch)
                    .await;
            }
        }
        // A remote Bot API cannot be patched in flight: restore locally.
        if mp4_audio && task.platform == "applemusic" {
            let path = ensure_local_audio_file(&state, &task).await?;
            repair_cached_aac(&path).await;
            let content_type = content_type(&path);
            return stream_local_file(&path, filename, &content_type, headers).await;
        }
    }

    match kind {
        FileKind::Audio
            if task
                .file_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty()) =>
        {
            let file_id = task.file_id.as_deref().unwrap();
            let filename = filename.as_deref().unwrap_or("telegram-audio.bin");
            stream_telegram_file(
                file_id,
                filename,
                task.content_type,
                task.byte_size,
                headers,
            )
            .await
        }
        FileKind::Cover
            if task
                .thumb_file_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty()) =>
        {
            let file_id = task.thumb_file_id.as_deref().unwrap();
            stream_telegram_file(
                file_id,
                filename.as_deref().unwrap_or("cover.jpg"),
                Some("image/jpeg".to_string()),
                None,
                headers,
            )
            .await
        }
        _ => Err(ApiError::Http {
            status: StatusCode::NOT_FOUND,
            message: "文件不存在".to_string(),
        }),
    }
}

async fn serve_audio_file(
    state: AppState,
    id: String,
    headers: &HeaderMap,
    revision: Option<&str>,
) -> ApiResult<Response> {
    let task = load_task(&state, &id).await?;
    validate_media_revision(&task, false, revision)?;
    if task.status == DownloadStatus::Streaming {
        // Keep the public `/audio` contract browser-compatible. A capable
        // WASM decoder client opts into the packet transport through the
        // separate `/audio/stream` endpoint.
        if task.streaming_mode.as_deref() == Some("wasm_alac") {
            return Err(ApiError::Http {
                status: StatusCode::CONFLICT,
                message: "无损音频正在准备原生浏览器兼容版本，请稍后重试".to_string(),
            });
        }
        return stream_growing_audio_file(state, &task, false, headers).await;
    }
    if task.status != DownloadStatus::Completed {
        return Err(ApiError::Http {
            status: StatusCode::CONFLICT,
            message: format!("下载尚未完成，当前状态为 {:?}", task.status),
        });
    }
    if !needs_browser_lossless_derivative(
        &task.quality,
        task.format.as_deref(),
        task.content_type.as_deref(),
        task.filename.as_deref(),
    ) {
        return serve_task_file(state, id, FileKind::Audio, headers).await;
    }

    let source = ensure_local_audio_file(&state, &task).await?;
    let dir = audio_work_dir(&state, &task.work_id);
    let service = BrowserAudioService::default();
    let playable = service.ensure_playable_lossless(&source, &dir).await?;
    stream_local_file(
        &playable.path,
        &playable.filename,
        playable.content_type,
        headers,
    )
    .await
}

async fn stream_growing_audio_file(
    state: AppState,
    task: &DownloadTask,
    packet_stream: bool,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let filename = task.filename.clone().ok_or_else(|| ApiError::Http {
        status: StatusCode::NOT_FOUND,
        message: "增量音频文件不存在".to_string(),
    })?;
    let base_path = state
        .data_dir
        .join("downloads")
        .join(&task.work_id)
        .join(&filename);
    let path = if packet_stream {
        music::wasm_alac_stream_path(&base_path)
    } else {
        base_path
    };
    // A progressive source mirrors a CDN object of known length, so the
    // response can promise that length and serve Range requests, waiting for
    // bytes that have not arrived yet. Remuxed/decrypted streams have no
    // final length until they finish and stay unranged.
    let total = task.stream_total.filter(|_| !packet_stream);
    let range = match total {
        Some(total) => parse_range(headers, total)?,
        None => None,
    };
    // Keep one reader and its cursor for this response. Reopening, seeking and
    // stat'ing for every 64 KiB multiplies blocking filesystem work and makes
    // an active reader unnecessarily dependent on the pathname staying around.
    let mut file = fs::File::open(&path).await?;
    let stream_id = audio_stream_identity(&file.metadata().await?);
    let (start, remaining) = match (range, total) {
        (Some((start, end)), _) => (start, Some(end - start + 1)),
        (None, Some(total)) => (0, Some(total)),
        (None, None) => (0, None),
    };
    if start > 0 {
        file.seek(SeekFrom::Start(start)).await?;
    }
    let downloads = state.downloads.clone();
    let id = task.id.clone();
    let generation = task.media_revision.clone();
    let stream = stream::unfold(
        (file, downloads, id, generation, remaining),
        |(mut file, downloads, id, generation, mut remaining)| async move {
            if remaining == Some(0) {
                return None;
            }
            let mut producer_completed = false;
            loop {
                let want = remaining.map_or(AUDIO_STREAM_CHUNK_SIZE, |left| {
                    left.min(AUDIO_STREAM_CHUNK_SIZE as u64) as usize
                });
                let mut bytes = vec![0u8; want];
                match file.read(&mut bytes).await {
                    Ok(0) if producer_completed && remaining.is_none() => return None,
                    Ok(0) if producer_completed => {
                        return Some((
                            Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "音频文件短于声明长度，请重试播放",
                            )),
                            (file, downloads, id, generation, remaining),
                        ));
                    }
                    // A Range far ahead of the download waits here until the
                    // mirror reaches it.
                    Ok(0) => {}
                    Ok(read) => {
                        bytes.truncate(read);
                        remaining = remaining.map(|left| left - read as u64);
                        return Some((Ok(bytes), (file, downloads, id, generation, remaining)));
                    }
                    Err(error) => {
                        return Some((Err(error), (file, downloads, id, generation, remaining)));
                    }
                }

                let status = downloads
                    .read()
                    .await
                    .get(&id)
                    .filter(|task| task.media_revision == generation)
                    .map(|task| task.status);
                if matches!(status, Some(DownloadStatus::Completed)) {
                    // The last append can race the EOF read above. Completed
                    // is published after the producer flushes, so one final
                    // read must follow observing it before emitting clean EOF.
                    producer_completed = true;
                    continue;
                }
                if matches!(status, Some(DownloadStatus::Failed) | None) {
                    // A failed producer can stop exactly on a packet boundary.
                    // A normal EOF would make the browser accept a truncated
                    // song as complete and persist it in its full-track cache.
                    return Some((
                        Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "音频流生成中断，请重试播放",
                        )),
                        (file, downloads, id, generation, remaining),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        },
    );
    let content_type = if packet_stream {
        "application/x-splayer-alac".to_string()
    } else {
        task.content_type
            .clone()
            .unwrap_or_else(|| "audio/mp4".to_string())
    };
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, browser_audio_type(&content_type))
        .header(header::CACHE_CONTROL, "no-store, no-transform")
        .header("X-Accel-Buffering", "no")
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{}\"", ascii_filename(&filename)),
        );
    response = match (range, total) {
        (Some((start, end)), Some(total)) => response
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total}"),
            )
            .header(header::CONTENT_LENGTH, (end - start + 1).to_string()),
        (None, Some(total)) => response
            .status(StatusCode::OK)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, total.to_string()),
        _ => response
            .status(StatusCode::OK)
            .header(header::ACCEPT_RANGES, "none"),
    };
    if let Some(identity) = stream_id {
        response = response.header("X-Audio-Stream-Id", identity);
    }
    let response = response
        .body(Body::from_stream(stream))
        .map_err(|error| ApiError::Custom(format!("构造增量音频响应失败：{error}")))?;
    Ok(response)
}

async fn load_task(state: &AppState, id: &str) -> ApiResult<DownloadTask> {
    if let Some(task) = state.downloads.read().await.get(id).cloned() {
        audio_cache::touch(&task.work_id);
        return Ok(task);
    }

    let Some((platform, track_id, quality)) = parse_playback_id(id) else {
        return Err(ApiError::Http {
            status: StatusCode::NOT_FOUND,
            message: "任务不存在".to_string(),
        });
    };
    let Some(cache) = find_music_cache(&platform, &track_id, &quality).await? else {
        return Err(ApiError::Http {
            status: StatusCode::NOT_FOUND,
            message: "任务不存在".to_string(),
        });
    };
    if cache.file_id.trim().is_empty() {
        return Err(ApiError::Http {
            status: StatusCode::NOT_FOUND,
            message: "任务尚未完成".to_string(),
        });
    }
    let task = task_from_cache(id, &platform, &track_id, &quality, &cache);
    // A creation request may have registered its generation while the cache
    // lookup awaited the database. Never replace that live generation with
    // a separately reconstructed cache row.
    let mut downloads = state.downloads.write().await;
    let task = downloads.entry(id.to_string()).or_insert(task).clone();
    audio_cache::touch(&task.work_id);
    Ok(task)
}

fn task_from_cache(
    id: &str,
    platform: &str,
    track_id: &str,
    quality: &str,
    cache: &MusicCache,
) -> DownloadTask {
    let media_revision = cache_media_revision(cache);
    DownloadTask {
        id: id.to_string(),
        work_id: generation_work_id(id, &media_revision),
        media_revision,
        status: DownloadStatus::Completed,
        platform: platform.to_string(),
        query: cache.song.clone(),
        track_id: Some(track_id.to_string()),
        selected_id: Some(track_id.to_string()),
        quality: quality.to_string(),
        include_cover: true,
        title: Some(cache.song.clone()),
        artist: Some(cache.singer.clone()),
        album: Some(cache.album.clone()),
        source_url: Some(cache.link.clone()),
        filename: Some(cache_audio_filename(cache)),
        cover_filename: None,
        file_id: Some(cache.file_id.clone()),
        thumb_file_id: (!cache.thumb_file_id.trim().is_empty())
            .then_some(cache.thumb_file_id.clone()),
        content_type: Some(content_type_from_ext(&cache.file_ext)),
        byte_size: Some(cache.music_size.max(0) as u64),
        bitrate: (cache.bitrate > 0).then_some(cache.bitrate as u32),
        duration: cache.duration.map(|duration| duration.max(0) as u32),
        format: (!cache.file_ext.trim().is_empty()).then_some(cache.file_ext.clone()),
        decrypt_elapsed_seconds: None,
        error: None,
        error_code: None,
        streaming_mode: None,
        stream_total: None,
        replay_gain_centi_db: None,
        cover_url: None,
        transfer: Arc::default(),
        upload_attempts: 0,
        upload_error: None,
    }
}

async fn ensure_local_audio_file(state: &AppState, task: &DownloadTask) -> ApiResult<PathBuf> {
    let filename = task.filename.as_deref().ok_or_else(|| ApiError::Http {
        status: StatusCode::NOT_FOUND,
        message: "音频文件不存在".to_string(),
    })?;
    let dir = state.data_dir.join("downloads").join(&task.work_id);
    let path = dir.join(filename);
    ensure_audio_file(&path, task.byte_size, |temporary| async move {
        let Some(file_id) = task.file_id.as_deref().filter(|id| !id.trim().is_empty()) else {
            return Err(ApiError::Http {
                status: StatusCode::NOT_FOUND,
                message: "音频文件不存在".to_string(),
            });
        };
        // The reserved temporary file is opened without create: a cancelled
        // Tokio file operation cannot recreate it after its cleanup guard ran.
        let mut output = fs::OpenOptions::new().write(true).open(&temporary).await?;
        // Converting needs the whole file. Read it over MTProto when possible,
        // so the Bot API server does not fetch and keep another copy.
        if let Some(size) = task.byte_size.filter(|size| *size > 0)
            && let Some(mut chunks) = telegram_segments::stream(file_id, size, 0, size - 1).await
        {
            use futures::StreamExt;
            let mut written = 0u64;
            let mut failed = None;
            while let Some(chunk) = chunks.next().await {
                match chunk {
                    Ok(chunk) => {
                        output.write_all(&chunk).await?;
                        written += chunk.len() as u64;
                    }
                    Err(err) => {
                        failed = Some(err);
                        break;
                    }
                }
            }
            match failed {
                None if written == size => {
                    output.flush().await?;
                    return Ok(());
                }
                failure => {
                    tracing::warn!(
                        "Telegram direct read of a whole file failed, using getFile: {}",
                        failure.map_or_else(|| "short".to_string(), |err| err.to_string())
                    );
                    output.set_len(0).await?;
                    output.seek(SeekFrom::Start(0)).await?;
                }
            }
        }
        let file = telegram_cache::locate_file(file_id)
            .await
            .map_err(|err| ApiError::Http {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Telegram 文件信息获取失败：{err}"),
            })?;
        if let Some(local_path) = telegram_cache::local_file_path(&file.file_path) {
            let mut source = fs::File::open(local_path).await?;
            tokio::io::copy(&mut source, &mut output).await?;
        } else {
            let mut response = telegram_cache::download_file(&file.file_path)
                .await
                .map_err(|err| ApiError::Http {
                    status: StatusCode::BAD_GATEWAY,
                    message: format!("Telegram 文件下载失败：{err}"),
                })?;
            while let Some(chunk) = response.chunk().await? {
                output.write_all(&chunk).await?;
            }
        }
        output.flush().await?;
        drop(output);
        if !valid_audio_file(&temporary, file.file_size).await {
            return Err(ApiError::Custom(
                "Telegram 音频文件不完整，请重试播放".to_string(),
            ));
        }
        Ok(())
    })
    .await
}

async fn stream_local_file(
    path: &FsPath,
    filename: &str,
    content_type: &str,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    stream_local_file_patched(path, filename, content_type, headers, None).await
}

/// Overwrite the 4 bytes at `fourcc_at` with `mp4a` in a streamed range,
/// leaving the file itself untouched.
fn patch_fourcc(
    stream: impl futures::Stream<Item = std::io::Result<axum::body::Bytes>>,
    start: u64,
    fourcc_at: u64,
) -> impl futures::Stream<Item = std::io::Result<axum::body::Bytes>> {
    use futures::StreamExt;
    let mut position = start;
    stream.map(move |chunk| {
        let chunk = chunk?;
        let chunk_start = position;
        position += chunk.len() as u64;
        let (from, to) = (fourcc_at.max(chunk_start), (fourcc_at + 4).min(position));
        if from >= to {
            return Ok(chunk);
        }
        let mut patched = chunk.to_vec();
        for at in from..to {
            patched[(at - chunk_start) as usize] = b"mp4a"[(at - fourcc_at) as usize];
        }
        Ok(axum::body::Bytes::from(patched))
    })
}

async fn stream_local_file_patched(
    path: &FsPath,
    filename: &str,
    content_type: &str,
    headers: &HeaderMap,
    fourcc_patch: Option<u64>,
) -> ApiResult<Response> {
    let mut file = fs::File::open(path).await.map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ApiError::Http {
                status: StatusCode::NOT_FOUND,
                message: "文件不存在".to_string(),
            }
        } else {
            ApiError::from(err)
        }
    })?;
    let metadata = file.metadata().await?;
    let size = metadata.len();
    let stream_id = audio_stream_identity(&metadata);
    let disposition = format!("inline; filename=\"{}\"", ascii_filename(filename));
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, browser_audio_type(&content_type))
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "public, max-age=86400, no-transform");
    if let Some(identity) = stream_id {
        response = response.header("X-Audio-Stream-Id", identity);
    }

    if let Some((start, end)) = parse_range(headers, size)? {
        file.seek(SeekFrom::Start(start)).await?;
        let chunk_size = end - start + 1;
        let reader = ReaderStream::with_capacity(file.take(chunk_size), AUDIO_STREAM_CHUNK_SIZE);
        let body = match fourcc_patch {
            Some(at) => Body::from_stream(patch_fourcc(reader, start, at)),
            None => Body::from_stream(reader),
        };
        return response
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"))
            .header(header::CONTENT_LENGTH, chunk_size.to_string())
            .body(body)
            .map_err(|err| ApiError::Custom(format!("failed to build response: {err}")));
    }

    let reader = ReaderStream::with_capacity(file, AUDIO_STREAM_CHUNK_SIZE);
    let body = match fourcc_patch {
        Some(at) => Body::from_stream(patch_fourcc(reader, 0, at)),
        None => Body::from_stream(reader),
    };
    response
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, size.to_string())
        .body(body)
        .map_err(|err| ApiError::Custom(format!("failed to build response: {err}")))
}

fn parse_range(headers: &HeaderMap, size: u64) -> ApiResult<Option<(u64, u64)>> {
    let Some(range) = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(None);
    };
    let Some(value) = range.strip_prefix("bytes=") else {
        return Ok(None);
    };
    if size == 0 {
        return Err(ApiError::Http {
            status: StatusCode::RANGE_NOT_SATISFIABLE,
            message: "文件为空，无法处理 Range 请求".to_string(),
        });
    }
    if value.contains(',') {
        return Err(ApiError::Http {
            status: StatusCode::RANGE_NOT_SATISFIABLE,
            message: "暂不支持多段 Range 请求".to_string(),
        });
    }

    let (start, end) = value.split_once('-').ok_or_else(|| ApiError::Http {
        status: StatusCode::RANGE_NOT_SATISFIABLE,
        message: "Range 请求格式无效".to_string(),
    })?;
    let (start, end) = if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ApiError::Http {
            status: StatusCode::RANGE_NOT_SATISFIABLE,
            message: "Range 请求格式无效".to_string(),
        })?;
        if suffix == 0 {
            return Err(ApiError::Http {
                status: StatusCode::RANGE_NOT_SATISFIABLE,
                message: "Range 请求格式无效".to_string(),
            });
        }
        (size.saturating_sub(suffix), size - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| ApiError::Http {
            status: StatusCode::RANGE_NOT_SATISFIABLE,
            message: "Range 请求格式无效".to_string(),
        })?;
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>().map_err(|_| ApiError::Http {
                status: StatusCode::RANGE_NOT_SATISFIABLE,
                message: "Range 请求格式无效".to_string(),
            })?
        };
        (start, end.min(size - 1))
    };

    if start >= size || start > end {
        return Err(ApiError::Http {
            status: StatusCode::RANGE_NOT_SATISFIABLE,
            message: "Range 超出文件范围".to_string(),
        });
    }
    Ok(Some((start, end)))
}

fn lyrics_response(
    platform: &str,
    track_id: &str,
    lyrics: Option<music::Lyrics>,
) -> LyricsResponse {
    let Some(lyrics) = lyrics else {
        return LyricsResponse {
            platform: platform.to_string(),
            track_id: track_id.to_string(),
            available: false,
            plain: String::new(),
            translation: String::new(),
            words: Vec::new(),
            roman: String::new(),
            credits: Vec::new(),
        };
    };
    LyricsResponse {
        platform: platform.to_string(),
        track_id: track_id.to_string(),
        available: !lyrics.plain.trim().is_empty()
            || !lyrics.translation.trim().is_empty()
            || !lyrics.words.is_empty(),
        plain: lyrics.plain,
        translation: lyrics.translation,
        words: lyrics.words,
        roman: lyrics.roman,
        credits: lyrics.credits,
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            ApiError::Http { status, .. } => status,
            ApiError::RateLimited { .. } | ApiError::ClientRateLimited { .. } => {
                StatusCode::TOO_MANY_REQUESTS
            }
            ApiError::Custom(_) => StatusCode::BAD_REQUEST,
            ApiError::Unplayable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::IncompleteCollection(_) => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let retry_after = match &self {
            ApiError::RateLimited {
                retry_after: Some(wait),
                ..
            }
            | ApiError::ClientRateLimited { retry_after: wait } => {
                Some(wait.as_secs_f64().ceil().max(1.0) as u64)
            }
            _ => None,
        };
        let body = match &self {
            ApiError::Unplayable(message) => {
                Json(serde_json::json!({ "detail": message, "code": UNPLAYABLE_CODE }))
            }
            ApiError::IncompleteCollection(message) => {
                Json(serde_json::json!({ "detail": message, "code": INCOMPLETE_COLLECTION_CODE }))
            }
            _ => Json(serde_json::json!({ "detail": self.to_string() })),
        };
        let mut response = (status, [(header::CACHE_CONTROL, "no-store")], body).into_response();
        if let Some(seconds) = retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, header::HeaderValue::from(seconds));
        }
        response
    }
}

fn sanitize_filename(value: &str) -> String {
    let mut cleaned = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch == '/' || ch == '\\' || ch.is_control() {
            cleaned.push('_');
        } else {
            cleaned.push(ch);
        }
    }
    let cleaned = cleaned.trim_matches([' ', '.']).to_string();
    if cleaned.is_empty() {
        "track.mp3".to_string()
    } else {
        cleaned
    }
}

fn ascii_filename(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_graphic() && ch != '"' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// `audio/m4a` (mime_guess's answer for `.m4a`) is not a type WebKit knows:
/// Safari refuses an `/audio` URL served with it, as it cannot fall back to
/// the file extension. Send the registered MP4 audio type instead.
fn browser_audio_type(content_type: &str) -> String {
    match content_type.trim().to_ascii_lowercase().as_str() {
        "audio/m4a" | "audio/x-m4a" | "audio/m4a-latm" => "audio/mp4".to_string(),
        _ => content_type.to_string(),
    }
}

fn content_type(path: &FsPath) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
}

async fn stream_telegram_file(
    file_id: &str,
    filename: &str,
    content_type: Option<String>,
    size: Option<u64>,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let content_type = content_type.unwrap_or_else(|| {
        mime_guess::from_path(filename)
            .first_or_octet_stream()
            .essence_str()
            .to_string()
    });
    if let Some(local_path) = telegram_cache::cached_local_path(file_id) {
        return stream_local_file(&local_path, filename, &content_type, headers).await;
    }
    if let Some(response) =
        stream_telegram_segments(file_id, filename, &content_type, size, None, headers).await?
    {
        return Ok(response);
    }
    stream_telegram_file_via_bot_api(file_id, filename, content_type, headers).await
}

/// Where a Telegram-cached Apple AAC file needs its sample entry relabelled
/// (`Some(None)`: it does not), read from the file's leading boxes over
/// MTProto. `None` when that cannot be read directly. Remembered per file:
/// every Range request of a song asks again.
async fn telegram_aac_patch(file_id: &str, size: u64) -> Option<Option<u64>> {
    use futures::StreamExt;
    lazy_static::lazy_static! {
        static ref PATCHES: std::sync::Mutex<HashMap<String, Option<u64>>> =
            std::sync::Mutex::new(HashMap::new());
    }
    if let Some(patch) = PATCHES.lock().unwrap().get(file_id) {
        return Some(*patch);
    }
    const LIMIT: u64 = 4 * 1024 * 1024;
    let mut chunks = telegram_segments::stream(file_id, size, 0, size.min(LIMIT) - 1).await?;
    let mut header = Vec::new();
    loop {
        match movie_header_end(&header) {
            HeaderEnd::Complete => break,
            HeaderEnd::NeedMore => {}
        }
        match chunks.next().await {
            Some(Ok(bytes)) => header.extend_from_slice(&bytes),
            Some(Err(err)) => {
                tracing::warn!("failed to read the movie header over MTProto: {err}");
                return None;
            }
            // The first 4 MiB hold no complete moov: nothing to relabel.
            None => break,
        }
    }
    drop(chunks);
    let patch = crate::audio::mislabeled_aac_entry_offset(&header).map(|at| at as u64);
    let mut patches = PATCHES.lock().unwrap();
    if patches.len() >= 4096 {
        patches.clear();
    }
    patches.insert(file_id.to_string(), patch);
    Some(patch)
}

enum HeaderEnd {
    Complete,
    NeedMore,
}

/// Whether `data` (a file's first bytes) holds every top-level box up to and
/// including `moov`, or shows there is none before the media data.
fn movie_header_end(data: &[u8]) -> HeaderEnd {
    let mut pos = 0usize;
    loop {
        let Some(head) = data.get(pos..pos + 8) else {
            return HeaderEnd::NeedMore;
        };
        let size32 = u32::from_be_bytes(head[..4].try_into().unwrap()) as u64;
        let size = match size32 {
            0 => return HeaderEnd::Complete,
            1 => match data.get(pos + 8..pos + 16) {
                Some(large) => u64::from_be_bytes(large.try_into().unwrap()),
                None => return HeaderEnd::NeedMore,
            },
            size => size,
        };
        match &head[4..8] {
            b"mdat" => return HeaderEnd::Complete,
            _ if size < 8 => return HeaderEnd::Complete,
            b"moov" if pos as u64 + size <= data.len() as u64 => return HeaderEnd::Complete,
            b"moov" => return HeaderEnd::NeedMore,
            _ => pos = pos.saturating_add(size as usize),
        }
    }
}

/// Serve a Telegram file by byte range over MTProto, straight from Telegram
/// to the listener: the first bytes leave as soon as Telegram returns them
/// and nothing is kept on disk. `fourcc_patch` relabels a mislabelled AAC
/// sample entry in flight. `None` when this route is unavailable for the
/// file; nothing has been sent then and the caller uses `getFile`.
async fn stream_telegram_segments(
    file_id: &str,
    filename: &str,
    content_type: &str,
    size: Option<u64>,
    fourcc_patch: Option<u64>,
    headers: &HeaderMap,
) -> ApiResult<Option<Response>> {
    let Some(size) = size.filter(|size| *size > 0) else {
        return Ok(None);
    };
    if !telegram_segments::enabled() {
        return Ok(None);
    }
    let range = parse_range(headers, size)?;
    let (start, end) = range.unwrap_or((0, size - 1));
    let Some(body) = telegram_segments::stream(file_id, size, start, end).await else {
        return Ok(None);
    };
    let body = match fourcc_patch {
        Some(at) => Body::from_stream(patch_fourcc(body, start, at)),
        None => Body::from_stream(body),
    };
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, browser_audio_type(content_type))
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{}\"", ascii_filename(filename)),
        )
        .header(header::ACCEPT_RANGES, "bytes")
        // The same immutable bytes as the Bot API route; see there.
        .header(header::CACHE_CONTROL, "public, max-age=86400, no-transform")
        .header(header::CONTENT_LENGTH, (end - start + 1).to_string());
    response = if range.is_some() {
        response
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"))
    } else {
        response.status(StatusCode::OK)
    };
    response
        .body(body)
        .map(Some)
        .map_err(|err| ApiError::Custom(format!("failed to build response: {err}")))
}

async fn stream_telegram_file_via_bot_api(
    file_id: &str,
    filename: &str,
    content_type: String,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let file = telegram_cache::locate_file(file_id)
        .await
        .map_err(|err| ApiError::Http {
            status: StatusCode::BAD_GATEWAY,
            message: format!("Telegram 文件信息获取失败：{err}"),
        })?;
    if let Some(local_path) = telegram_cache::local_file_path(&file.file_path) {
        return stream_local_file(&local_path, filename, &content_type, headers).await;
    }
    // Keep the browser's single Range request intact when Telegram's file
    // endpoint supports it (the local Bot API does). The previous code always
    // opened a full 200 response, which disabled normal media buffering and
    // made seeking or recovering from a slow connection much more likely to
    // stall.
    let requested_range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| value.starts_with("bytes=") && !value.contains(','));
    let response = telegram_cache::download_file_with_range(&file.file_path, requested_range)
        .await
        .map_err(|err| ApiError::Http {
            status: StatusCode::BAD_GATEWAY,
            message: format!("Telegram 文件下载失败：{err}"),
        })?;

    let upstream_status = response.status();
    let upstream_headers = response.headers();
    let upstream_length = upstream_headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let upstream_range = upstream_headers
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let is_partial = upstream_status == StatusCode::PARTIAL_CONTENT;
    let mut builder = Response::builder()
        .status(if is_partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, browser_audio_type(&content_type))
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{}\"", ascii_filename(filename)),
        )
        // Completed generation URLs may be cached for one day. Preserve bytes
        // exactly for native decoding and Range offsets; no unbounded TTL.
        .header(header::CACHE_CONTROL, "public, max-age=86400, no-transform");
    if is_partial {
        builder = builder.header(header::ACCEPT_RANGES, "bytes");
        if let Some(range) = upstream_range {
            builder = builder.header(header::CONTENT_RANGE, range);
        }
    }
    if let Some(length) = upstream_length {
        builder = builder.header(header::CONTENT_LENGTH, length);
    } else if !is_partial && let Some(size) = file.file_size {
        builder = builder.header(header::CONTENT_LENGTH, size.to_string());
    }
    builder
        .body(Body::from_stream(response.bytes_stream()))
        .map_err(|err| ApiError::Custom(format!("failed to build response: {err}")))
}

fn cache_audio_filename(cache: &MusicCache) -> String {
    let ext = if cache.file_ext.trim().is_empty() {
        "mp3"
    } else {
        cache.file_ext.trim().trim_start_matches('.')
    };
    sanitize_filename(&format!("{} - {}.{ext}", cache.song, cache.singer))
}

fn content_type_from_ext(ext: &str) -> String {
    let ext = ext.trim().trim_start_matches('.');
    if ext.is_empty() {
        return "application/octet-stream".to_string();
    }
    mime_guess::from_ext(ext)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
}

fn cache_upload_chat_id() -> i64 {
    if SETTINGS.music.inline_upload_chat_id != 0 {
        SETTINGS.music.inline_upload_chat_id
    } else {
        SETTINGS.bot.owner
    }
}

const AUDIO_CACHE_SWEEP_INTERVAL: Duration = Duration::from_secs(15 * 60);

fn spawn_audio_cache_sweeper(state: AppState) {
    let policy = audio_cache::Policy::from_settings(&SETTINGS.audio_cache);
    tokio::spawn(async move {
        // The first tick fires at once, so a restart also tidies up.
        let mut interval = tokio::time::interval(AUDIO_CACHE_SWEEP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if let Err(error) = sweep_audio_cache(&state, policy, SystemTime::now()).await {
                tracing::warn!("audio cache sweep failed: {error}");
            }
        }
    });
}

/// Remove idle or excess song directories, then forget in-memory tasks that
/// lost their only copy or have been idle as long; Telegram-backed tasks are
/// rebuilt from MySQL on their next request.
async fn sweep_audio_cache(
    state: &AppState,
    policy: audio_cache::Policy,
    now: SystemTime,
) -> std::io::Result<()> {
    let root = state.data_dir.join("downloads");
    let mut entries = audio_cache::scan(&root).await?;
    let protected: std::collections::HashSet<String> = state
        .downloads
        .read()
        .await
        .values()
        .filter(|task| {
            !matches!(
                task.status,
                DownloadStatus::Completed | DownloadStatus::Failed
            )
        })
        .map(|task| task.work_id.clone())
        .collect();
    for entry in &mut entries {
        if let Some(accessed) = audio_cache::last_access(&entry.name) {
            entry.last_used = entry.last_used.max(accessed);
        }
    }
    // Telegram keeps these songs and serves every replay straight from the
    // Bot API, so their local files only matter while they are in use.
    let uploaded: std::collections::HashSet<String> = state
        .downloads
        .read()
        .await
        .values()
        .filter(|task| {
            task.status == DownloadStatus::Completed
                && task
                    .file_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty())
        })
        .map(|task| task.work_id.clone())
        .collect();
    let scanned = entries.len();
    let bytes: u64 = entries.iter().map(|entry| entry.bytes).sum();
    let mut freed = 0;
    let mut evicted = std::collections::HashSet::new();
    for name in audio_cache::select(entries.clone(), now, policy, &protected, &uploaded) {
        match fs::remove_dir_all(root.join(&name)).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(directory = %name, "audio cache eviction failed: {error}");
                continue;
            }
        }
        freed += entries
            .iter()
            .find(|entry| entry.name == name)
            .map_or(0, |entry| entry.bytes);
        evicted.insert(name);
    }
    for entry in &entries {
        if !evicted.contains(&entry.name) && !protected.contains(&entry.name) {
            freed += audio_cache::remove_stream_leftovers(&root.join(&entry.name), now).await;
        }
    }

    let idle = |work_id: &str| {
        audio_cache::last_access(work_id)
            .is_none_or(|accessed| now.duration_since(accessed).unwrap_or_default() >= policy.idle)
    };
    let mut forgotten = Vec::new();
    state.downloads.write().await.retain(|_, task| {
        if !matches!(
            task.status,
            DownloadStatus::Completed | DownloadStatus::Failed
        ) {
            return true;
        }
        let telegram = task
            .file_id
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty());
        let keep = (telegram || !evicted.contains(&task.work_id)) && !idle(&task.work_id);
        if !keep {
            forgotten.push(task.work_id.clone());
        }
        keep
    });
    for work_id in evicted.iter().chain(&forgotten) {
        audio_cache::forget(work_id);
    }
    if !evicted.is_empty() || !forgotten.is_empty() {
        tracing::info!(
            scanned,
            evicted = evicted.len(),
            freed_mib = freed / (1024 * 1024),
            remaining_mib = (bytes - freed) / (1024 * 1024),
            forgotten_tasks = forgotten.len(),
            "audio cache sweep"
        );
    }
    Ok(())
}

fn audio_work_dir(state: &AppState, task_id: &str) -> PathBuf {
    state.data_dir.join("downloads").join(task_id)
}

fn inflight_key(platform: &str, track_id: &str, quality: &str) -> String {
    format!(
        "{}\x1f{}\x1f{}",
        platform.trim().to_ascii_lowercase(),
        track_id.trim(),
        quality.trim().to_ascii_lowercase()
    )
}

fn playback_id(platform: &str, track_id: &str, quality: &str) -> String {
    format!(
        "{}--{}--{}",
        id_component(platform),
        id_component(track_id),
        id_component(quality)
    )
}

fn parse_playback_id(id: &str) -> Option<(String, String, String)> {
    let (platform, rest) = id.split_once("--")?;
    let (track_id, quality) = rest.rsplit_once("--")?;
    let platform = platform.to_string();
    let track_id = track_id.to_string();
    let quality = quality.to_string();
    if platform.is_empty() || track_id.is_empty() || quality.is_empty() {
        return None;
    }
    Some((platform, track_id, quality))
}

fn id_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.trim().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
            out.push(byte as char);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

fn estimate_bitrate(bytes: u64, duration: Option<u32>) -> Option<u32> {
    let duration = duration.filter(|duration| *duration > 0)? as u64;
    Some((bytes.saturating_mul(8) / duration).min(u32::MAX as u64) as u32)
}

fn duration_seconds(duration: Duration) -> f64 {
    (duration.as_millis() as f64) / 1000.0
}

fn default_search_limit() -> usize {
    10
}

fn default_recommendation_limit() -> usize {
    20
}

fn default_apple_platform() -> String {
    "applemusic".to_string()
}

fn collection_request_limit(limit: Option<usize>) -> ApiResult<usize> {
    match limit {
        Some(limit) if (1..=music::MAX_COLLECTION_ITEMS).contains(&limit) => Ok(limit),
        Some(_) => Err(ApiError::Custom(format!(
            "limit 必须在 1 到 {} 之间",
            music::MAX_COLLECTION_ITEMS
        ))),
        // Ask for one extra item so full imports fail explicitly at the cap.
        None => Ok(music::MAX_COLLECTION_ITEMS + 1),
    }
}

fn default_true() -> bool {
    true
}

fn serve_web() -> bool {
    env::var("MUSIC_API_SERVE_WEB")
        .map(|value| {
            !matches!(
                value.as_str(),
                "0" | "false" | "FALSE" | "off" | "OFF" | "no" | "NO"
            )
        })
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m4a_audio_is_served_as_registered_mp4_type() {
        assert_eq!(content_type(FsPath::new("a.m4a")), "audio/m4a");
        assert_eq!(browser_audio_type("audio/m4a"), "audio/mp4");
        assert_eq!(browser_audio_type("audio/x-m4a"), "audio/mp4");
        assert_eq!(browser_audio_type("audio/flac"), "audio/flac");
        assert_eq!(browser_audio_type("audio/mp4"), "audio/mp4");
    }

    #[test]
    fn recommendation_exclusions_are_bounded_unique_catalog_ids() {
        let ids = recommendation_excludes(
            [" 123456 ", "../me", "123456", "654321", "111111", "abc123"],
            2,
        );
        assert_eq!(ids, ["123456", "654321"]);
    }

    #[test]
    fn continuation_post_preserves_large_queue_exclusions() {
        let ids = (100000..105001)
            .map(|id| id.to_string())
            .collect::<Vec<_>>();
        let request: RecommendationContinuationRequest =
            serde_json::from_value(serde_json::json!({
                "seed_id": "123456", "exclude": ids,
            }))
            .unwrap();
        assert_eq!(request.platform, "applemusic");
        assert_eq!(request.limit, 10);
        assert_eq!(
            recommendation_excludes(request.exclude.iter().map(String::as_str), 10_000).len(),
            5001
        );
    }

    fn test_alac_cache() -> MusicCache {
        MusicCache {
            platform: "applemusic".into(),
            track_id: "100000".into(),
            quality: "hires".into(),
            song: "Public test song".into(),
            singer: "Test artist".into(),
            album: String::new(),
            link: String::new(),
            file_ext: "m4a".into(),
            music_size: 8,
            bitrate: 0,
            duration: Some(30),
            file_id: "test-file-id".into(),
            thumb_file_id: String::new(),
            from_user_id: 0,
            from_chat_id: 0,
            lyrics_tag: 0,
        }
    }

    #[tokio::test]
    async fn completed_alac_source_is_the_stored_m4a_without_conversion() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.id = "alac-source-test".into();
        task.work_id = "alac-source-test--1".into();
        task.streaming_mode = Some("wasm_alac".into());
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());

        let public = public_task(&state, &task);
        let source = public
            .alac_source_url
            .clone()
            .expect("completed ALAC source");
        assert!(source.starts_with(&format!("/api/v1/playbacks/{}/audio/source?rev=", task.id)));
        assert_ne!(Some(source.clone()), public.streaming_url);
        // Native clients may download the original file as well.
        let mut native = task.clone();
        native.streaming_mode = None;
        assert_eq!(
            public_task(&state, &native).alac_source_url,
            Some(source.clone())
        );
        // While the song is still growing, only the packet stream exists.
        let mut growing = task.clone();
        growing.status = DownloadStatus::Streaming;
        assert_eq!(public_task(&state, &growing).alac_source_url, None);

        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        let m4a = b"\0\0\0\x10ftypM4A \0\0\0\0stored";
        fs::write(folder.join(task.filename.as_deref().unwrap()), m4a)
            .await
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=4-7".parse().unwrap());
        let rev = source.split("rev=").nth(1).unwrap().to_string();
        let response = download_source(
            State(state.clone()),
            Path(task.id.clone()),
            Query(MediaVersionQuery { rev: Some(rev) }),
            headers,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/mp4");
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap(),
            "ftyp"
        );
        // Serving the source never writes a packet sidecar.
        let packet_path =
            music::wasm_alac_stream_path(&folder.join(task.filename.as_deref().unwrap()));
        assert!(fs::metadata(packet_path).await.is_err());

        let stale = download_source(
            State(state.clone()),
            Path(task.id.clone()),
            Query(MediaVersionQuery {
                rev: Some("0".repeat(32)),
            }),
            HeaderMap::new(),
        )
        .await;
        assert!(matches!(
            stale,
            Err(ApiError::Http {
                status: StatusCode::CONFLICT,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn fourcc_patch_applies_to_any_range_without_changing_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telegram.m4a");
        let original = b"0123456789alacABCDEF".to_vec();
        fs::write(&path, &original).await.unwrap();
        for (range, expected) in [
            (None, "0123456789mp4aABCDEF"),
            (Some("bytes=11-15"), "p4aAB"),
            (Some("bytes=0-11"), "0123456789mp"),
            (Some("bytes=15-"), "BCDEF"),
        ] {
            let mut headers = HeaderMap::new();
            if let Some(range) = range {
                headers.insert(header::RANGE, range.parse().unwrap());
            }
            let response =
                stream_local_file_patched(&path, "a.m4a", "audio/mp4", &headers, Some(10))
                    .await
                    .unwrap();
            let body = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(body, expected, "{range:?}");
        }
        assert_eq!(fs::read(&path).await.unwrap(), original);
    }

    #[tokio::test]
    async fn sweep_removes_stream_leftovers_of_finished_downloads() {
        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path().join("song--1");
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "a.m4a",
            "a.m4a.streaming",
            "a.m4a.streaming.assembled",
            "a.m4a.streaming.alacstream",
        ] {
            std::fs::write(dir.join(name), vec![0; 10]).unwrap();
        }
        let fresh = audio_cache::remove_stream_leftovers(&dir, SystemTime::now()).await;
        assert_eq!(
            fresh, 0,
            "a download that just finished keeps its stream files"
        );
        let later = SystemTime::now() + Duration::from_secs(3600);
        assert_eq!(audio_cache::remove_stream_leftovers(&dir, later).await, 20);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["a.m4a", "a.m4a.streaming.alacstream"]);
    }

    #[test]
    fn audio_hash_is_hex_sha256() {
        assert_eq!(
            audio_sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn reused_upload_keeps_its_own_key() {
        let existing = MusicCache {
            quality: "lossless".into(),
            file_id: "shared-file".into(),
            thumb_file_id: "shared-thumb".into(),
            music_size: 99,
            lyrics_tag: 1,
            from_chat_id: -100,
            ..test_alac_cache()
        };
        let request = MusicCache {
            track_id: "200000".into(),
            file_id: String::new(),
            ..test_alac_cache()
        };
        let reused = reuse_upload(request, &existing);
        assert_eq!(
            (reused.track_id.as_str(), reused.quality.as_str()),
            ("200000", "hires")
        );
        assert_eq!(
            (reused.file_id.as_str(), reused.thumb_file_id.as_str()),
            ("shared-file", "shared-thumb")
        );
        assert_eq!(
            (reused.music_size, reused.lyrics_tag, reused.from_chat_id),
            (99, 1, -100)
        );
    }

    fn test_cache_state(directory: &FsPath) -> (AppState, DownloadTask) {
        let cache = test_alac_cache();
        let id = playback_id("applemusic", &cache.track_id, "hires");
        let task = task_from_cache(&id, "applemusic", &cache.track_id, "hires", &cache);
        let state = AppState {
            downloads: Arc::new(RwLock::new(HashMap::new())),
            task_events: Arc::new(Notify::new()),
            accounts: None,
            inflight: Arc::new(RwLock::new(HashMap::new())),
            data_dir: directory.to_path_buf(),
            media_base_url: None,
        };
        (state, task)
    }

    #[tokio::test]
    async fn audio_cache_sweep_evicts_idle_copies_but_never_running_tasks() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut cached) = test_cache_state(directory.path());
        // Access times are process-wide; keep these generations unique.
        cached.id = "sweep-telegram".into();
        cached.work_id = "sweep-telegram--1".into();
        let days_ago = SystemTime::now() - Duration::from_secs(4 * 24 * 3600);
        let make_dir = |name: &str, old: bool| {
            let dir = directory.path().join("downloads").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("audio.m4a"), vec![0; 64]).unwrap();
            if old {
                for path in [dir.join("audio.m4a"), dir.clone()] {
                    std::fs::File::open(path)
                        .unwrap()
                        .set_modified(days_ago)
                        .unwrap();
                }
            }
            dir
        };
        let mut running = cached.clone();
        running.id = "sweep-running".into();
        running.work_id = "sweep-running--1".into();
        running.status = DownloadStatus::Processing;
        let mut local_only = cached.clone();
        local_only.id = "sweep-local".into();
        local_only.work_id = "sweep-local--1".into();
        local_only.file_id = None;
        let running_dir = make_dir(&running.work_id, true);
        let local_dir = make_dir(&local_only.work_id, true);
        let telegram_dir = make_dir(&cached.work_id, true);
        let fresh_dir = make_dir("sweep-fresh--1", false);
        {
            let mut downloads = state.downloads.write().await;
            for task in [&running, &local_only, &cached] {
                downloads.insert(task.id.clone(), task.clone());
            }
        }

        let policy = audio_cache::Policy {
            idle: Duration::from_secs(72 * 3600),
            max_bytes: 1 << 30,
        };
        sweep_audio_cache(&state, policy, SystemTime::now())
            .await
            .unwrap();

        assert!(running_dir.exists(), "a running task keeps its files");
        assert!(fresh_dir.exists(), "recently written files stay");
        assert!(!local_dir.exists() && !telegram_dir.exists());
        let downloads = state.downloads.read().await;
        assert!(downloads.contains_key("sweep-running"));
        assert!(
            !downloads.contains_key("sweep-local") && !downloads.contains_key(&cached.id),
            "idle tasks are forgotten and rebuilt from MySQL on demand"
        );
    }

    #[test]
    fn media_origin_is_optional_and_accepts_only_uncredentialed_http_origins() {
        for value in [None, Some(""), Some("  ")] {
            assert!(media_base_url(value).unwrap().is_none());
        }
        for value in ["https://media.example.test", "http://localhost:8443/"] {
            assert!(media_base_url(Some(value)).unwrap().is_some());
        }
        for value in [
            "//media.example.test",
            "ftp://media.example.test",
            "https://user:secret@media.example.test",
            "https://media.example.test/audio",
            "https://media.example.test/?token=secret",
            "https://media.example.test/#fragment",
        ] {
            assert!(media_base_url(Some(value)).is_err(), "{value}");
        }
    }

    #[test]
    fn only_completed_audio_moves_to_media_origin_and_revision_survives_completion() {
        let directory = tempfile::tempdir().unwrap();
        let (mut state, mut task) = test_cache_state(directory.path());
        state.media_base_url = media_base_url(Some("https://media.example.test")).unwrap();
        task.streaming_mode = Some("wasm_alac".into());
        let expected_native_revision = media_revision(&task, false);
        let expected_packet_revision = media_revision(&task, true);
        assert_ne!(expected_native_revision, expected_packet_revision);
        for status in [
            DownloadStatus::Queued,
            DownloadStatus::Processing,
            DownloadStatus::Streaming,
            DownloadStatus::Failed,
            DownloadStatus::Completed,
        ] {
            task.status = status;
            let response = create_download_response(&state, &task);
            let expected_prefix = if status == DownloadStatus::Completed {
                "https://media.example.test/api/"
            } else {
                "/api/"
            };
            assert!(response.audio_url.starts_with(expected_prefix));
            assert_eq!(response.audio_url, response.playback.audio_url);
            assert!(
                response
                    .audio_url
                    .ends_with(&format!("?rev={expected_native_revision}"))
            );
            assert!(
                response
                    .playback
                    .streaming_url
                    .as_deref()
                    .unwrap()
                    .ends_with(&format!("?rev={expected_packet_revision}"))
            );
            assert!(
                response
                    .playback
                    .streaming_url
                    .as_deref()
                    .unwrap()
                    .starts_with(expected_prefix)
            );
            for control in [
                &response.status_url,
                &response.cover_url,
                &response.lyrics_url,
            ] {
                assert!(control.starts_with("/api/"));
                assert!(!control.contains("rev="));
            }
        }
        state.media_base_url = None;
        assert!(public_task(&state, &task).audio_url.starts_with("/api/"));
        assert!(
            !directory.path().join("downloads").exists(),
            "URLs never fetch or transcode media"
        );
    }

    #[test]
    fn same_origin_urls_keep_both_representation_revisions_across_completion() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.streaming_mode = Some("wasm_alac".into());
        let native = media_url(&state, &task, false);
        let packets = media_url(&state, &task, true);
        assert!(native.starts_with("/api/v1/playbacks/"));
        assert!(packets.starts_with("/api/v1/playbacks/"));
        assert_ne!(media_revision(&task, false), media_revision(&task, true));
        for status in [
            DownloadStatus::Queued,
            DownloadStatus::Processing,
            DownloadStatus::Streaming,
            DownloadStatus::Completed,
        ] {
            task.status = status;
            let response = create_download_response(&state, &task);
            assert_eq!(response.audio_url, native);
            assert_eq!(response.playback.audio_url, native);
            assert_eq!(
                response.playback.streaming_url.as_deref(),
                Some(packets.as_str())
            );
            assert_eq!(response.playback.media_revision, task.media_revision);
            for control in [
                &response.status_url,
                &response.cover_url,
                &response.lyrics_url,
            ] {
                assert!(control.starts_with("/api/"));
                assert!(!control.contains("rev="));
            }
        }
    }

    #[tokio::test]
    async fn same_origin_service_keeps_legacy_host_media_bytes_and_range_identity() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.format = Some("flac".into());
        task.filename = Some("audio.flac".into());
        task.content_type = Some("audio/flac".into());
        task.streaming_mode = Some("wasm_alac".into());
        let native_bytes = b"fLaC public synthetic audio fixture";
        let packet_bytes = b"SPLALAC1\0\0\0\x02 public synthetic packet fixture";
        task.byte_size = Some(native_bytes.len() as u64);
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        let audio_path = folder.join("audio.flac");
        fs::write(&audio_path, native_bytes).await.unwrap();
        fs::write(music::wasm_alac_stream_path(&audio_path), packet_bytes)
            .await
            .unwrap();
        let urls = [
            (media_url(&state, &task, false), native_bytes.as_slice()),
            (media_url(&state, &task, true), packet_bytes.as_slice()),
        ];
        state.downloads.write().await.insert(task.id.clone(), task);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app_with_state(state, false))
                .await
                .unwrap()
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        for (path, bytes) in urls {
            let mut identity = None;
            for host in ["app.example.test", "media.example.test"] {
                for range in [None, Some("bytes=4-12")] {
                    let mut request = client
                        .get(format!("{base}{path}"))
                        .header(header::HOST, host);
                    if let Some(range) = range {
                        request = request.header(header::RANGE, range);
                    }
                    let response = request.send().await.unwrap();
                    let expected = if range.is_some() {
                        &bytes[4..13]
                    } else {
                        bytes
                    };
                    assert_eq!(
                        response.status(),
                        if range.is_some() {
                            StatusCode::PARTIAL_CONTENT
                        } else {
                            StatusCode::OK
                        }
                    );
                    assert!(!response.headers().contains_key(header::LOCATION));
                    assert_eq!(
                        response.headers()[header::CACHE_CONTROL],
                        "public, max-age=86400, no-transform"
                    );
                    assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
                    assert_eq!(
                        response.headers()[header::CONTENT_LENGTH],
                        expected.len().to_string()
                    );
                    if range.is_some() {
                        assert_eq!(
                            response.headers()[header::CONTENT_RANGE],
                            format!("bytes 4-12/{}", bytes.len())
                        );
                    }
                    let actual_identity = response.headers()["x-audio-stream-id"].clone();
                    if let Some(identity) = &identity {
                        assert_eq!(&actual_identity, identity);
                    } else {
                        identity = Some(actual_identity);
                    }
                    assert_eq!(response.bytes().await.unwrap().as_ref(), expected);
                }
            }
        }
        server.abort();
    }

    #[test]
    fn generation_and_representation_versions_isolate_source_and_derived_caches() {
        let mut cache = test_alac_cache();
        let original = task_from_cache("same-task", "applemusic", "100000", "hires", &cache);
        let restored = task_from_cache("same-task", "applemusic", "100000", "hires", &cache);
        assert_eq!(original.media_revision, restored.media_revision);
        assert_eq!(original.work_id, restored.work_id);
        for change in 0..3 {
            match change {
                0 => cache.file_id = "replacement-file-id".into(),
                1 => cache.file_ext = "flac".into(),
                _ => cache.music_size += 1,
            }
            let replaced = task_from_cache("same-task", "applemusic", "100000", "hires", &cache);
            assert_ne!(original.work_id, replaced.work_id);
            assert_ne!(
                media_revision(&original, false),
                media_revision(&replaced, false)
            );
            assert_ne!(
                media_revision(&original, true),
                media_revision(&replaced, true)
            );
            assert!(
                validate_media_revision(&replaced, true, Some(&media_revision(&original, true)))
                    .is_err()
            );
        }
        assert!(!original.media_revision.contains(&cache.file_id));
        assert!(
            validate_media_revision(&original, false, None).is_ok(),
            "legacy clients remain supported"
        );
        assert!(
            validate_media_revision(&original, true, Some(&media_revision(&original, false)))
                .is_err()
        );
    }

    #[test]
    fn old_client_second_question_mark_is_accepted_only_for_current_exact_task_revision() {
        let directory = tempfile::tempdir().unwrap();
        let (_, task) = test_cache_state(directory.path());
        for packets in [false, true] {
            let revision = media_revision(&task, packets);
            let legacy = format!("{revision}?v={}", task.id);
            assert!(validate_media_revision(&task, packets, Some(&legacy)).is_ok());
            for invalid in [
                format!("{revision}?v=another-task"),
                format!("stale?v={}", task.id),
                format!("{}?v={}", media_revision(&task, !packets), task.id),
                format!("{legacy}?extra=ignored"),
                format!("{legacy}&extra=ignored"),
                format!("{revision}?v={}%20", task.id),
            ] {
                assert!(
                    validate_media_revision(&task, packets, Some(&invalid)).is_err(),
                    "{invalid}"
                );
            }
        }
    }

    #[tokio::test]
    async fn conditional_static_responses_keep_their_path_specific_cache_policy() {
        let app = Router::new()
            .route(
                "/assets/test-hashed.js",
                get(|| async { StatusCode::NOT_MODIFIED }),
            )
            .route("/index.html", get(|| async { StatusCode::NOT_MODIFIED }))
            .route("/api/v1/test", get(|| async { StatusCode::NOT_MODIFIED }))
            .layer(middleware::from_fn(response_cache_policy));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for (path, policy) in [
            (
                "/assets/test-hashed.js",
                "public, max-age=31536000, immutable",
            ),
            ("/index.html", "no-cache"),
            ("/api/v1/test", "no-store"),
        ] {
            let response = reqwest::get(format!("{base}{path}")).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
            assert_eq!(response.headers()[header::CACHE_CONTROL], policy);
        }
        server.abort();
    }

    #[tokio::test]
    async fn concurrent_task_creators_publish_only_one_source_generation() {
        let directory = tempfile::tempdir().unwrap();
        let (state, task) = test_cache_state(directory.path());
        let gate = Arc::new(tokio::sync::Barrier::new(16));
        let attempts = (0..16).map(|_| {
            let state = state.clone();
            let gate = gate.clone();
            let mut candidate = task.clone();
            candidate.status = DownloadStatus::Queued;
            candidate.media_revision = uuid::Uuid::new_v4().simple().to_string();
            candidate.work_id = generation_work_id(&candidate.id, &candidate.media_revision);
            async move {
                gate.wait().await;
                let existing = register_download_task(&state, &candidate).await;
                (candidate, existing)
            }
        });
        let attempts = futures::future::join_all(attempts).await;
        let winners = attempts
            .iter()
            .filter(|(_, existing)| existing.is_none())
            .collect::<Vec<_>>();
        assert_eq!(winners.len(), 1);
        let winner = &winners[0].0;
        let published = state.downloads.read().await[&task.id].clone();
        assert_eq!(published.work_id, winner.work_id);
        for (_, existing) in attempts.iter().filter(|(_, existing)| existing.is_some()) {
            assert_eq!(
                existing.as_ref().unwrap().media_revision,
                winner.media_revision
            );
            assert_eq!(existing.as_ref().unwrap().work_id, winner.work_id);
        }
    }

    #[tokio::test]
    async fn status_long_poll_returns_on_transition_and_never_waits_for_a_changed_status() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.status = DownloadStatus::Processing;
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());
        let wait = |since: &str| PlaybackStatusQuery {
            since: Some(since.to_string()),
            wait_ms: Some(10_000),
        };

        let started = tokio::time::Instant::now();
        let stale = get_download(
            State(state.clone()),
            Path(task.id.clone()),
            Query(wait("queued")),
        )
        .await
        .unwrap();
        assert_eq!(stale.0.status, DownloadStatus::Processing);
        assert!(started.elapsed() < Duration::from_millis(200));

        let poll = tokio::spawn(get_download(
            State(state.clone()),
            Path(task.id.clone()),
            Query(wait("processing")),
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!poll.is_finished(), "an unchanged status must wait");
        let started = tokio::time::Instant::now();
        state
            .downloads
            .write()
            .await
            .get_mut(&task.id)
            .unwrap()
            .status = DownloadStatus::Streaming;
        notify_task_change(&state);
        let changed = tokio::time::timeout(Duration::from_secs(1), poll)
            .await
            .expect("transition must wake the long poll")
            .unwrap()
            .unwrap();
        assert_eq!(changed.0.status, DownloadStatus::Streaming);
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn json_responses_are_compressed_for_clients_that_accept_it() {
        let directory = tempfile::tempdir().unwrap();
        let (state, _) = test_cache_state(directory.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app_with_state(state, false))
                .await
                .unwrap()
        });
        let client = reqwest::Client::new();
        let compressed = client
            .get(format!("{base}/api/v1/platforms"))
            .header(header::ACCEPT_ENCODING, "br")
            .send()
            .await
            .unwrap();
        assert_eq!(compressed.headers()[header::CONTENT_ENCODING], "br");
        let plain = client
            .get(format!("{base}/api/v1/platforms"))
            .send()
            .await
            .unwrap();
        assert!(plain.headers().get(header::CONTENT_ENCODING).is_none());
        assert!(plain.json::<serde_json::Value>().await.unwrap().is_array());
        server.abort();
    }

    #[tokio::test]
    async fn http_cache_policy_covers_dynamic_errors_ranges_and_anonymous_cors() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.format = Some("flac".into());
        task.filename = Some("audio.flac".into());
        task.content_type = Some("audio/flac".into());
        let bytes = b"fLaC public synthetic audio fixture";
        task.byte_size = Some(bytes.len() as u64);
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        fs::write(folder.join("audio.flac"), bytes).await.unwrap();
        let mut queued = task.clone();
        queued.id = "queued".into();
        queued.status = DownloadStatus::Queued;
        state
            .downloads
            .write()
            .await
            .extend([(task.id.clone(), task.clone()), (queued.id.clone(), queued)]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app_with_state(state, false))
                .await
                .unwrap()
        });
        let client = reqwest::Client::new();
        for (path, status) in [
            ("/health".to_string(), StatusCode::OK),
            ("/api/v1/playbacks/queued".to_string(), StatusCode::OK),
            (
                "/api/v1/playbacks/queued/audio".to_string(),
                StatusCode::CONFLICT,
            ),
            ("/api/v1/absent".to_string(), StatusCode::NOT_FOUND),
            ("/not-a-page".to_string(), StatusCode::NOT_FOUND),
            (
                format!("/api/v1/playbacks/{}/audio?rev=stale", task.id),
                StatusCode::CONFLICT,
            ),
            ("/api/v1/search".to_string(), StatusCode::BAD_REQUEST),
        ] {
            let response = client.get(format!("{base}{path}")).send().await.unwrap();
            assert_eq!(response.status(), status, "{path}");
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "no-store",
                "{path}"
            );
        }
        for (method, path) in [
            (Method::POST, "/health"),
            (Method::POST, "/api/v1/playbacks"),
        ] {
            let response = client
                .request(method, format!("{base}{path}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body("{")
                .send()
                .await
                .unwrap();
            assert!(response.status().is_client_error());
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
        let path = format!(
            "{base}/api/v1/playbacks/{}/audio?rev={}",
            task.id,
            media_revision(&task, false)
        );
        let old_client_url = format!("{path}?v={}", urlencoding::encode(&task.id));
        let old_client_response = client.get(old_client_url).send().await.unwrap();
        assert_eq!(old_client_response.status(), StatusCode::OK);
        assert_eq!(old_client_response.bytes().await.unwrap().as_ref(), bytes);
        let wrong_old_client_url = format!("{path}?v=another-task");
        let wrong_old_client_response = client.get(wrong_old_client_url).send().await.unwrap();
        assert_eq!(wrong_old_client_response.status(), StatusCode::CONFLICT);
        assert_eq!(
            wrong_old_client_response.headers()[header::CACHE_CONTROL],
            "no-store"
        );
        for (range, expected, expected_range) in [
            (None, bytes.as_slice(), None),
            (
                Some("bytes=4-"),
                &bytes[4..],
                Some(format!("bytes 4-{}/{}", bytes.len() - 1, bytes.len())),
            ),
            (
                Some("bytes=-7"),
                &bytes[bytes.len() - 7..],
                Some(format!(
                    "bytes {}-{}/{}",
                    bytes.len() - 7,
                    bytes.len() - 1,
                    bytes.len()
                )),
            ),
        ] {
            let mut request = client
                .get(&path)
                .header(header::ORIGIN, "https://app.example.test");
            if let Some(range) = range {
                request = request.header(header::RANGE, range);
            }
            let response = request.send().await.unwrap();
            assert_eq!(
                response.status(),
                if range.is_some() {
                    StatusCode::PARTIAL_CONTENT
                } else {
                    StatusCode::OK
                }
            );
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "public, max-age=86400, no-transform"
            );
            assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/flac");
            assert_eq!(
                response.headers()[header::CONTENT_LENGTH],
                expected.len().to_string()
            );
            assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
            assert!(
                response.headers()[header::ACCESS_CONTROL_EXPOSE_HEADERS]
                    .to_str()
                    .unwrap()
                    .contains("x-audio-stream-id")
            );
            assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
            if let Some(value) = expected_range {
                assert_eq!(response.headers()[header::CONTENT_RANGE], value);
            }
            assert_eq!(response.bytes().await.unwrap().as_ref(), expected);
        }
        let response = client
            .get(&path)
            .header(header::RANGE, "bytes=999-")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        for credential in [header::AUTHORIZATION, header::COOKIE] {
            let response = client
                .get(&path)
                .header(credential, "synthetic-test-value")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "no-store, no-transform"
            );
        }
        let preflight = client
            .request(Method::OPTIONS, &path)
            .header(header::ORIGIN, "https://app.example.test")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "range,if-range")
            .send()
            .await
            .unwrap();
        assert!(preflight.status().is_success());
        assert_eq!(
            preflight.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "*"
        );
        assert_eq!(preflight.headers()[header::CACHE_CONTROL], "no-store");
        server.abort();
    }

    #[tokio::test]
    async fn replacement_generation_cannot_turn_an_old_partial_stream_into_clean_eof() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.status = DownloadStatus::Streaming;
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        fs::write(
            folder.join(task.filename.as_deref().unwrap()),
            b"partial old source",
        )
        .await
        .unwrap();
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());
        let growing = stream_growing_audio_file(state.clone(), &task, false, &HeaderMap::new())
            .await
            .unwrap();
        let mut replacement = task.clone();
        replacement.status = DownloadStatus::Completed;
        replacement.media_revision = uuid::Uuid::new_v4().simple().to_string();
        replacement.work_id = generation_work_id(&replacement.id, &replacement.media_revision);
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), replacement);
        assert!(
            axum::body::to_bytes(growing.into_body(), 1024)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn loudness_endpoint_never_downloads_missing_telegram_audio() {
        use crate::loudness::LoudnessState;

        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        for (status, expected) in [
            (DownloadStatus::Queued, LoudnessState::Pending),
            (DownloadStatus::Processing, LoudnessState::Pending),
            (DownloadStatus::Streaming, LoudnessState::Pending),
            (DownloadStatus::Failed, LoudnessState::Unavailable),
            (DownloadStatus::Completed, LoudnessState::Unavailable),
        ] {
            task.status = status;
            state
                .downloads
                .write()
                .await
                .insert(task.id.clone(), task.clone());
            let (headers, Json(response)) =
                playback_loudness(State(state.clone()), Path(task.id.clone()))
                    .await
                    .unwrap();
            assert_eq!(response.state, expected);
            assert_eq!(response.version, 1);
            assert_eq!(headers, [(header::CACHE_CONTROL, "no-store")]);
            // The task has a Telegram file ID, but measurement must not even
            // create a download directory or attempt a cache restoration.
            assert!(!directory.path().join("downloads").exists());
        }
    }

    #[tokio::test]
    async fn loudness_rejects_packet_sidecars_and_sources_outside_downloads() {
        use crate::loudness::LoudnessState;

        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        task.streaming_mode = Some("wasm_alac".into());
        task.byte_size = None;
        task.filename = Some("audio.m4a.alacstream".into());
        fs::write(
            folder.join(task.filename.as_deref().unwrap()),
            b"SPLALAC1\0\0test",
        )
        .await
        .unwrap();
        assert_eq!(
            completed_task_loudness(&state, &task).await.state,
            LoudnessState::Unavailable
        );
        // A complete .streaming.alacstream sidecar alone also must not be
        // mistaken for the normal remuxed M4A when the latter was evicted.
        task.filename = Some("audio.m4a".into());
        assert_eq!(
            completed_task_loudness(&state, &task).await.state,
            LoudnessState::Unavailable
        );
        let outside = directory.path().join("outside.wav");
        fs::write(&outside, b"not an audio input").await.unwrap();
        task.filename = Some(outside.to_string_lossy().into_owned());
        assert_eq!(
            completed_task_loudness(&state, &task).await.state,
            LoudnessState::Unavailable
        );
        assert!(!directory.path().join("loudness").exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "local HTTP throughput diagnostic; allocates a temporary 128 MiB sparse file"]
    async fn benchmark_growing_and_completed_http_streams() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.status = DownloadStatus::Streaming;
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        let path = folder.join(task.filename.as_deref().unwrap());
        let file = fs::File::create(&path).await.unwrap();
        let size = 128 * 1024 * 1024u64;
        file.set_len(size).await.unwrap();
        drop(file);
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());
        let growing_state = state.clone();
        let growing_id = task.id.clone();
        let router = Router::new()
            .route(
                "/growing",
                get(move || {
                    let state = growing_state.clone();
                    let id = growing_id.clone();
                    async move {
                        let task = state.downloads.read().await[&id].clone();
                        stream_growing_audio_file(state, &task, false, &HeaderMap::new()).await
                    }
                }),
            )
            .route(
                "/completed",
                get(move || {
                    let path = path.clone();
                    async move {
                        stream_local_file(&path, "audio.m4a", "audio/mp4", &HeaderMap::new()).await
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::builder().http1_only().build().unwrap();
        for _ in 0..3 {
            for route in ["growing", "completed"] {
                state
                    .downloads
                    .write()
                    .await
                    .get_mut(&task.id)
                    .unwrap()
                    .status = DownloadStatus::Streaming;
                let started = std::time::Instant::now();
                let mut response = client
                    .get(format!("http://{address}/{route}"))
                    .send()
                    .await
                    .unwrap();
                let mut bytes = 0u64;
                let mut first_byte_ms = 0;
                let mut transfer_ms = 0;
                while let Some(chunk) = response.chunk().await.unwrap() {
                    if bytes == 0 {
                        first_byte_ms = started.elapsed().as_millis();
                    }
                    bytes += chunk.len() as u64;
                    if bytes >= size {
                        transfer_ms = started.elapsed().as_millis();
                        state
                            .downloads
                            .write()
                            .await
                            .get_mut(&task.id)
                            .unwrap()
                            .status = DownloadStatus::Completed;
                    }
                }
                assert_eq!(bytes, size);
                println!(
                    "HTTP_STREAM_BENCH route={route} bytes={bytes} first_byte_ms={first_byte_ms} transfer_ms={transfer_ms} mib_per_sec={:.2}",
                    size as f64 / 1024.0 / 1024.0 / (transfer_ms.max(1) as f64 / 1000.0)
                );
            }
        }
        server.abort();
    }

    #[tokio::test]
    async fn completed_alac_upgrade_preserves_native_endpoint_and_client_contract() {
        let directory = tempfile::tempdir().unwrap();
        let (state, task) = test_cache_state(directory.path());
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());
        let make_request = |mode: Option<&str>| DownloadRequest {
            platform: "applemusic".into(),
            query: "Public test song".into(),
            selected_id: Some("100000".into()),
            quality: Some("hires".into()),
            include_cover: false,
            streaming_mode: mode.map(str::to_string),
        };
        let (_, wasm) =
            create_download(State(state.clone()), Json(make_request(Some("wasm_alac"))))
                .await
                .unwrap();
        assert_eq!(wasm.0.playback.streaming_mode.as_deref(), Some("wasm_alac"));
        assert!(wasm.0.playback.streaming_url.is_some());
        let (_, native) = create_download(State(state.clone()), Json(make_request(None)))
            .await
            .unwrap();
        assert_eq!(native.0.playback.streaming_mode, None);
        assert_eq!(native.0.playback.streaming_url, None);
        assert_eq!(
            state.downloads.read().await[&task.id]
                .streaming_mode
                .as_deref(),
            Some("wasm_alac")
        );

        // The ordinary endpoint remains FLAC even after another client opts in
        // to packets. This uses an existing derivative, without invoking FFmpeg.
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        fs::write(folder.join(task.filename.as_deref().unwrap()), b"complete")
            .await
            .unwrap();
        fs::write(folder.join("browser-lossless.flac"), b"native audio")
            .await
            .unwrap();
        // A completed ALAC task may have both representations. Their URLs
        // must remain independent even when native/WASM callers alternate.
        // These sentinel bytes are deliberately not decodable source audio:
        // successfully serving them proves neither path reruns a converter.
        let packets = b"SPLALAC1\0\0\0\x02cached packet representation";
        let packet_path =
            music::wasm_alac_stream_path(&folder.join(task.filename.as_deref().unwrap()));
        fs::write(&packet_path, packets).await.unwrap();
        let mut packet_headers = HeaderMap::new();
        packet_headers.insert(header::RANGE, "bytes=0-7".parse().unwrap());
        let packet_response = download_stream(
            State(state.clone()),
            Path(task.id.clone()),
            Query(MediaVersionQuery::default()),
            packet_headers,
        )
        .await
        .unwrap();
        assert_eq!(packet_response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            packet_response.headers()[header::CONTENT_TYPE],
            "application/x-splayer-alac"
        );
        assert_eq!(
            packet_response.headers()[header::CONTENT_RANGE],
            format!("bytes 0-7/{}", packets.len())
        );
        assert_eq!(
            axum::body::to_bytes(packet_response.into_body(), 1024)
                .await
                .unwrap(),
            "SPLALAC1"
        );
        let response = serve_audio_file(state.clone(), task.id.clone(), &HeaderMap::new(), None)
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/flac");
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap(),
            "native audio"
        );
        assert_eq!(fs::read(packet_path).await.unwrap(), packets);
        assert_eq!(
            fs::read(folder.join(task.filename.as_deref().unwrap()))
                .await
                .unwrap(),
            b"complete"
        );
    }

    #[tokio::test]
    async fn legacy_telegram_flac_stays_native_and_reuses_exact_bytes_for_wasm_requests() {
        // Model a restored Telegram cache row and its already retrieved file.
        // No Telegram token, database, external decoder or real song is used.
        let original = b"fLaC\x80\0\0\x22synthetic legacy FLAC cache payload";
        for quality in ["lossless", "hires"] {
            let directory = tempfile::tempdir().unwrap();
            let (state, _) = test_cache_state(directory.path());
            let mut cache = test_alac_cache();
            cache.quality = quality.to_string();
            cache.file_ext = "flac".into();
            cache.music_size = original.len() as i64;
            let id = playback_id(&cache.platform, &cache.track_id, &cache.quality);
            let mut task = task_from_cache(
                &id,
                &cache.platform,
                &cache.track_id,
                &cache.quality,
                &cache,
            );
            task.status = DownloadStatus::Queued;
            task.streaming_mode = Some("wasm_alac".into());
            state.downloads.write().await.insert(id.clone(), task);
            complete_task_from_cache(&state, &id, &cache.track_id, &cache)
                .await
                .unwrap();
            let restored = state.downloads.read().await[&id].clone();
            let public = public_task(&state, &restored);
            assert_eq!(restored.file_id.as_deref(), Some(cache.file_id.as_str()));
            assert_eq!(public.status, DownloadStatus::Completed);
            assert_eq!(public.quality, quality);
            assert_eq!(public.format.as_deref(), Some("flac"));
            assert_eq!(public.streaming_mode, None);
            assert_eq!(public.streaming_url, None);
            assert_eq!(public.streaming_codec, None);
            assert_eq!(public.streaming_protocol, None);
            assert_eq!(public.audio_url, media_url(&state, &restored, false));

            let folder = audio_work_dir(&state, &restored.work_id);
            fs::create_dir_all(&folder).await.unwrap();
            let source = folder.join(restored.filename.as_deref().unwrap());
            fs::write(&source, original).await.unwrap();
            let mut range = HeaderMap::new();
            range.insert(header::RANGE, "bytes=4-15".parse().unwrap());
            let response = serve_audio_file(state.clone(), id.clone(), &range, None)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/flac");
            assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
            assert_eq!(response.headers()[header::CONTENT_LENGTH], "12");
            assert_eq!(
                response.headers()[header::CONTENT_RANGE],
                format!("bytes 4-15/{}", original.len())
            );
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap()
                    .as_ref(),
                &original[4..16]
            );

            // Even an explicit request to the packet URL must reject FLAC;
            // returning these bytes there would poison the ALAC decoder.
            let packet_error = download_stream(
                State(state.clone()),
                Path(id.clone()),
                Query(MediaVersionQuery::default()),
                HeaderMap::new(),
            )
            .await
            .unwrap_err();
            assert!(matches!(
                packet_error,
                ApiError::Http {
                    status: StatusCode::NOT_ACCEPTABLE,
                    ..
                }
            ));
            // Repeat negotiation after the shared task has been restored.
            // A subsequent WASM preference still selects the cached FLAC.
            let (_, negotiated) = create_download(
                State(state.clone()),
                Json(DownloadRequest {
                    platform: cache.platform.clone(),
                    query: cache.song.clone(),
                    selected_id: Some(cache.track_id.clone()),
                    quality: Some(quality.into()),
                    include_cover: false,
                    streaming_mode: Some("wasm_alac".into()),
                }),
            )
            .await
            .unwrap();
            assert_eq!(negotiated.playback.streaming_mode, None);
            assert_eq!(negotiated.playback.streaming_url, None);
            let whole = serve_audio_file(state.clone(), id, &HeaderMap::new(), None)
                .await
                .unwrap();
            assert_eq!(whole.status(), StatusCode::OK);
            assert_eq!(
                axum::body::to_bytes(whole.into_body(), 1024)
                    .await
                    .unwrap()
                    .as_ref(),
                original
            );
            assert_eq!(fs::read(&source).await.unwrap(), original);
            assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
        }
    }

    #[tokio::test]
    async fn cached_telegram_alac_honors_wasm_request_without_changing_native_requests() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        let cache = test_alac_cache();
        for requested in [None, Some("wasm_alac")] {
            task.status = DownloadStatus::Queued;
            task.streaming_mode = requested.map(str::to_string);
            state
                .downloads
                .write()
                .await
                .insert(task.id.clone(), task.clone());
            complete_task_from_cache(&state, &task.id, &cache.track_id, &cache)
                .await
                .unwrap();
            let completed = state.downloads.read().await[&task.id].clone();
            assert_eq!(completed.status, DownloadStatus::Completed);
            assert_eq!(completed.streaming_mode.as_deref(), requested);
        }
    }

    #[test]
    fn packet_upgrade_never_changes_running_native_tasks_or_non_alac_providers() {
        let cache = test_alac_cache();
        let original = task_from_cache("test", "applemusic", "100000", "hires", &cache);
        for status in [
            DownloadStatus::Queued,
            DownloadStatus::Processing,
            DownloadStatus::Streaming,
            DownloadStatus::Failed,
        ] {
            let mut task = original.clone();
            task.status = status;
            upgrade_completed_alac_transport(&mut task, Some("wasm_alac"));
            assert_eq!(task.streaming_mode, None);
        }
        for (platform, quality, format) in [
            ("netease", "hires", "m4a"),
            ("applemusic", "high", "m4a"),
            ("applemusic", "hires", "flac"),
        ] {
            let mut task = original.clone();
            task.platform = platform.into();
            task.quality = quality.into();
            task.format = Some(format.into());
            task.content_type = None;
            upgrade_completed_alac_transport(&mut task, Some("wasm_alac"));
            assert_eq!(task.streaming_mode, None);
        }
    }

    #[test]
    fn streaming_fallback_log_redacts_signed_urls() {
        let error = ApiError::Custom(
            "invalid media https://example.test/a.m3u8?token=private-value next skd://secret"
                .to_string(),
        );
        let reason = streaming_fallback_reason(&error);
        assert!(reason.contains("invalid media"));
        assert!(!reason.contains("private-value"));
        assert!(!reason.contains("secret"));
        assert!(!reason.contains("example.test"));
    }

    #[tokio::test]
    async fn incomplete_collection_error_lets_the_client_offer_import_anyway() {
        let response = ApiError::IncompleteCollection("缺 2 首".into()).into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["code"], INCOMPLETE_COLLECTION_CODE);
        assert_eq!(value["detail"], "缺 2 首");
    }

    #[tokio::test]
    async fn collection_requests_are_strict_unless_the_caller_opts_in() {
        assert!(!music::incomplete_collection_allowed());
        let parse = |query: &str| {
            let uri: axum::http::Uri = format!("/x?{query}").parse().unwrap();
            Query::<CollectionParams>::try_from_uri(&uri).unwrap().0
        };
        assert!(parse("platform=netease&url=x&allow_incomplete=true").allow_incomplete);
        assert!(!parse("platform=netease&url=x").allow_incomplete);
    }

    #[tokio::test]
    async fn unplayable_error_carries_a_machine_readable_code() {
        let response = ApiError::Unplayable("《岛》暂时无法播放".into()).into_response();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["code"], UNPLAYABLE_CODE);
        assert_eq!(value["detail"], "《岛》暂时无法播放");

        let response = ApiError::Custom("x".into()).into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value.get("code").is_none());
    }

    #[test]
    fn served_quality_relabels_lossless_task_as_plain_aac() {
        let mut task = DownloadTask {
            id: "applemusic--1470198631--lossless".to_string(),
            work_id: "w".to_string(),
            media_revision: "r".to_string(),
            status: DownloadStatus::Processing,
            platform: "applemusic".to_string(),
            query: String::new(),
            track_id: None,
            selected_id: Some("1470198631".to_string()),
            quality: "lossless".to_string(),
            include_cover: false,
            title: None,
            artist: None,
            album: None,
            source_url: None,
            filename: Some("audio.m4a".to_string()),
            cover_filename: None,
            file_id: None,
            thumb_file_id: None,
            content_type: Some("audio/mp4".to_string()),
            byte_size: None,
            bitrate: None,
            duration: None,
            format: Some("m4a".to_string()),
            decrypt_elapsed_seconds: None,
            error: None,
            error_code: None,
            streaming_mode: Some("wasm_alac".to_string()),
            stream_total: None,
            replay_gain_centi_db: None,
            cover_url: None,
            transfer: Arc::default(),
            upload_attempts: 0,
            upload_error: None,
        };
        let mut track = music::MusicTrack {
            id: "1470198631".to_string(),
            platform: music::MusicPlatform::AppleMusic,
            song: "岛".to_string(),
            singer: String::new(),
            album: String::new(),
            cover: String::new(),
            link: String::new(),
            url: String::new(),
            headers: HashMap::new(),
            max_chunk_size: None,
            duration: None,
            bitrate: None,
            format: Some("m4a".to_string()),
            replay_gain_centi_db: None,
            served_quality: None,
        };
        assert!(!adopt_served_quality(&mut task, &track));
        assert_eq!(task.quality, "lossless");

        track.served_quality = Some("high".to_string());
        assert!(adopt_served_quality(&mut task, &track));
        assert_eq!(task.quality, "high");
        assert_eq!(task.streaming_mode, None);
        // The completed AAC must not be offered as an ALAC source or packets.
        task.status = DownloadStatus::Completed;
        assert!(!serves_alac_source(&task));
        upgrade_completed_alac_transport(&mut task, Some("wasm_alac"));
        assert_eq!(task.streaming_mode, None);
        assert!(!adopt_served_quality(&mut task, &track));
    }

    #[tokio::test]
    async fn failed_growing_stream_does_not_publish_a_clean_eof() {
        let directory = tempfile::tempdir().unwrap();
        let mut task = DownloadTask {
            id: "test-stream".to_string(),
            work_id: "test-stream".to_string(),
            media_revision: "test-generation".to_string(),
            status: DownloadStatus::Streaming,
            platform: "applemusic".to_string(),
            query: String::new(),
            track_id: None,
            selected_id: None,
            quality: "lossless".to_string(),
            include_cover: false,
            title: None,
            artist: None,
            album: None,
            source_url: None,
            filename: Some("audio.m4a.streaming".to_string()),
            cover_filename: None,
            file_id: None,
            thumb_file_id: None,
            content_type: None,
            byte_size: None,
            bitrate: None,
            duration: None,
            format: None,
            decrypt_elapsed_seconds: None,
            error: None,
            error_code: None,
            streaming_mode: Some("wasm_alac".to_string()),
            stream_total: None,
            replay_gain_centi_db: None,
            cover_url: None,
            transfer: Arc::default(),
            upload_attempts: 0,
            upload_error: None,
        };
        let state = AppState {
            downloads: Arc::new(RwLock::new(HashMap::new())),
            task_events: Arc::new(Notify::new()),
            accounts: None,
            inflight: Arc::new(RwLock::new(HashMap::new())),
            data_dir: directory.path().to_path_buf(),
            media_base_url: None,
        };
        let folder = directory.path().join("downloads/test-stream");
        fs::create_dir_all(&folder).await.unwrap();
        fs::write(
            folder.join("audio.m4a.streaming.alacstream"),
            b"complete packet boundary",
        )
        .await
        .unwrap();
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());
        let native =
            serve_audio_file(state.clone(), task.id.clone(), &HeaderMap::new(), None).await;
        assert!(
            matches!(
                native,
                Err(ApiError::Http {
                    status: StatusCode::CONFLICT,
                    ..
                })
            ),
            "native audio must not receive a growing ALAC container from a WASM task"
        );
        for status in [DownloadStatus::Completed, DownloadStatus::Failed] {
            task.status = status;
            state
                .downloads
                .write()
                .await
                .insert(task.id.clone(), task.clone());
            let response = stream_growing_audio_file(state.clone(), &task, true, &HeaderMap::new())
                .await
                .unwrap();
            assert_eq!(response.headers()["x-accel-buffering"], "no");
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "no-store, no-transform"
            );
            let result = axum::body::to_bytes(response.into_body(), 1024).await;
            if status == DownloadStatus::Completed {
                assert_eq!(result.unwrap(), "complete packet boundary");
            } else {
                assert!(
                    result.is_err(),
                    "a truncated song must not become a complete cached file"
                );
            }
        }
    }

    #[tokio::test]
    async fn growing_stream_keeps_reading_appends_until_the_producer_finishes() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.status = DownloadStatus::Streaming;
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        let path = folder.join(task.filename.as_deref().unwrap());
        fs::write(&path, b"first packet").await.unwrap();
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());
        let response = stream_growing_audio_file(state.clone(), &task, false, &HeaderMap::new())
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), "first packet");
        let mut next = Box::pin(body.next());
        tokio::select! {
            _ = &mut next => panic!("temporary producer EOF must not end the response"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        let mut producer = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        producer.write_all(b"final packet").await.unwrap();
        producer.flush().await.unwrap();
        state
            .downloads
            .write()
            .await
            .get_mut(&task.id)
            .unwrap()
            .status = DownloadStatus::Completed;
        assert_eq!(next.await.unwrap().unwrap(), "final packet");
        assert!(body.next().await.is_none());
    }

    #[test]
    fn movie_header_end_waits_for_a_whole_moov() {
        let boxed = |typ: &[u8; 4], payload: usize| {
            let mut out = ((payload + 8) as u32).to_be_bytes().to_vec();
            out.extend_from_slice(typ);
            out.extend(std::iter::repeat_n(0u8, payload));
            out
        };
        let mut file = boxed(b"ftyp", 16);
        file.extend(boxed(b"moov", 100));
        file.extend(boxed(b"mdat", 50));
        let moov_end = 24 + 108;
        assert!(matches!(movie_header_end(&file[..10]), HeaderEnd::NeedMore));
        assert!(matches!(
            movie_header_end(&file[..moov_end - 1]),
            HeaderEnd::NeedMore
        ));
        assert!(matches!(
            movie_header_end(&file[..moov_end]),
            HeaderEnd::Complete
        ));
        // Media data before any moov: nothing more to wait for.
        let mut no_moov = boxed(b"ftyp", 16);
        no_moov.extend(boxed(b"mdat", 50));
        assert!(matches!(
            movie_header_end(&no_moov[..40]),
            HeaderEnd::Complete
        ));
    }

    #[tokio::test]
    async fn fourcc_patch_applies_across_chunk_boundaries() {
        use futures::StreamExt;
        let chunks = vec![
            Ok(axum::body::Bytes::from_static(b"0123al")),
            Ok(axum::body::Bytes::from_static(b"ac6789")),
        ];
        let patched: Vec<u8> = patch_fourcc(futures::stream::iter(chunks), 100, 104)
            .map(|chunk| chunk.unwrap().to_vec())
            .concat()
            .await;
        assert_eq!(patched, b"0123mp4a6789");
    }

    #[test]
    fn public_task_exposes_stream_length_only_while_streaming() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.stream_total = Some(10);
        task.status = DownloadStatus::Streaming;
        let value = serde_json::to_value(public_task(&state, &task)).unwrap();
        assert_eq!(value["stream_length"], 10);
        task.status = DownloadStatus::Completed;
        let value = serde_json::to_value(public_task(&state, &task)).unwrap();
        assert!(value.get("stream_length").is_none());
        task.status = DownloadStatus::Streaming;
        task.stream_total = None;
        let value = serde_json::to_value(public_task(&state, &task)).unwrap();
        assert!(value.get("stream_length").is_none());
    }

    #[tokio::test]
    async fn progressive_growing_stream_serves_ranges_of_the_declared_length() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.status = DownloadStatus::Streaming;
        task.content_type = Some("audio/flac".to_string());
        task.stream_total = Some(10);
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        let path = folder.join(task.filename.as_deref().unwrap());
        fs::write(&path, b"0123").await.unwrap();
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());

        let full = stream_growing_audio_file(state.clone(), &task, false, &HeaderMap::new())
            .await
            .unwrap();
        assert_eq!(full.status(), StatusCode::OK);
        assert_eq!(full.headers()[header::CONTENT_LENGTH], "10");
        assert_eq!(full.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(full.headers()[header::CONTENT_TYPE], "audio/flac");

        // A seek past the mirrored prefix waits for the bytes, then stops at
        // the requested end instead of reading the rest of the file.
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=6-8".parse().unwrap());
        let ranged = stream_growing_audio_file(state.clone(), &task, false, &headers)
            .await
            .unwrap();
        assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(ranged.headers()[header::CONTENT_RANGE], "bytes 6-8/10");
        assert_eq!(ranged.headers()[header::CONTENT_LENGTH], "3");
        let mut body = ranged.into_body().into_data_stream();
        let mut next = Box::pin(body.next());
        tokio::select! {
            _ = &mut next => panic!("a range beyond the mirror must wait for it"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        let mut producer = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        producer.write_all(b"456789").await.unwrap();
        producer.flush().await.unwrap();
        assert_eq!(next.await.unwrap().unwrap(), "678");
        assert!(body.next().await.is_none());

        let mut beyond = HeaderMap::new();
        beyond.insert(header::RANGE, "bytes=10-".parse().unwrap());
        let error = stream_growing_audio_file(state.clone(), &task, false, &beyond)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ApiError::Http {
                status: StatusCode::RANGE_NOT_SATISFIABLE,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn progressive_growing_stream_errors_when_the_file_ends_short() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.status = DownloadStatus::Completed;
        task.stream_total = Some(10);
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        fs::write(folder.join(task.filename.as_deref().unwrap()), b"0123")
            .await
            .unwrap();
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());
        let response = stream_growing_audio_file(state.clone(), &task, false, &HeaderMap::new())
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), "0123");
        assert!(body.next().await.unwrap().is_err());
    }

    #[tokio::test]
    async fn completed_status_racing_the_last_append_does_not_truncate_the_stream() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.status = DownloadStatus::Streaming;
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        let path = folder.join(task.filename.as_deref().unwrap());
        fs::write(&path, b"first packet").await.unwrap();
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());
        let response = stream_growing_audio_file(state.clone(), &task, false, &HeaderMap::new())
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), "first packet");

        // Hold status publication while the reader observes a temporary EOF.
        // Then publish both the final bytes and Completed before releasing it.
        let mut tasks = state.downloads.write().await;
        let mut next = Box::pin(body.next());
        tokio::select! {
            _ = &mut next => panic!("reader should await the producer status"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        let mut producer = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        producer.write_all(b"racing final packet").await.unwrap();
        producer.flush().await.unwrap();
        tasks.get_mut(&task.id).unwrap().status = DownloadStatus::Completed;
        drop(tasks);
        assert_eq!(next.await.unwrap().unwrap(), "racing final packet");
        assert!(body.next().await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn growing_and_completed_ranges_keep_the_same_identity_after_appends() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.status = DownloadStatus::Streaming;
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        let path = music::wasm_alac_stream_path(&folder.join(task.filename.as_deref().unwrap()));
        fs::write(&path, b"first packet").await.unwrap();
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());
        let growing = stream_growing_audio_file(state.clone(), &task, true, &HeaderMap::new())
            .await
            .unwrap();
        let identity = growing.headers()["x-audio-stream-id"].clone();
        assert_eq!(identity.as_bytes().len(), 32);
        assert!(identity.as_bytes().iter().all(u8::is_ascii_hexdigit));
        let mut body = growing.into_body().into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap(), "first packet");

        let mut producer = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        producer.write_all(b"final packet").await.unwrap();
        producer.flush().await.unwrap();
        state
            .downloads
            .write()
            .await
            .get_mut(&task.id)
            .unwrap()
            .status = DownloadStatus::Completed;
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=12-".parse().unwrap());
        let range = stream_local_file(
            &path,
            "audio.alacstream",
            "application/x-splayer-alac",
            &headers,
        )
        .await
        .unwrap();
        assert_eq!(range.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(range.headers()["x-audio-stream-id"], identity);
        assert_eq!(range.headers()[header::CONTENT_RANGE], "bytes 12-23/24");
        assert_eq!(
            axum::body::to_bytes(range.into_body(), 1024).await.unwrap(),
            "final packet"
        );
        let completed = stream_local_file(
            &path,
            "audio.alacstream",
            "application/x-splayer-alac",
            &HeaderMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(completed.status(), StatusCode::OK);
        assert_eq!(completed.headers()["x-audio-stream-id"], identity);
        assert_eq!(
            axum::body::to_bytes(completed.into_body(), 1024)
                .await
                .unwrap(),
            "first packetfinal packet"
        );
        assert_eq!(body.next().await.unwrap().unwrap(), "final packet");
        assert!(body.next().await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replacing_a_packet_file_changes_identity_without_redirecting_its_open_reader() {
        let directory = tempfile::tempdir().unwrap();
        let (state, mut task) = test_cache_state(directory.path());
        task.status = DownloadStatus::Streaming;
        let folder = audio_work_dir(&state, &task.work_id);
        fs::create_dir_all(&folder).await.unwrap();
        let path = music::wasm_alac_stream_path(&folder.join(task.filename.as_deref().unwrap()));
        fs::write(&path, b"original packet").await.unwrap();
        state
            .downloads
            .write()
            .await
            .insert(task.id.clone(), task.clone());
        let growing = stream_growing_audio_file(state.clone(), &task, true, &HeaderMap::new())
            .await
            .unwrap();
        let original_identity = growing.headers()["x-audio-stream-id"].clone();
        // The growing body has opened the original inode but has not read yet.
        let replacement = folder.join("replacement.tmp");
        fs::write(&replacement, b"replaced packet").await.unwrap();
        fs::rename(&replacement, &path).await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=4-".parse().unwrap());
        let completed = stream_local_file(
            &path,
            "audio.alacstream",
            "application/x-splayer-alac",
            &headers,
        )
        .await
        .unwrap();
        assert_eq!(completed.status(), StatusCode::PARTIAL_CONTENT);
        assert_ne!(completed.headers()["x-audio-stream-id"], original_identity);
        assert_eq!(
            axum::body::to_bytes(completed.into_body(), 1024)
                .await
                .unwrap(),
            "aced packet"
        );
        state
            .downloads
            .write()
            .await
            .get_mut(&task.id)
            .unwrap()
            .status = DownloadStatus::Completed;
        assert_eq!(
            axum::body::to_bytes(growing.into_body(), 1024)
                .await
                .unwrap(),
            "original packet"
        );
    }

    #[test]
    fn collection_import_limit_checks_one_extra_item_without_silent_clamping() {
        assert_eq!(
            collection_request_limit(None).unwrap(),
            music::MAX_COLLECTION_ITEMS + 1
        );
        assert_eq!(collection_request_limit(Some(275)).unwrap(), 275);
        assert!(collection_request_limit(Some(0)).is_err());
        assert!(collection_request_limit(Some(music::MAX_COLLECTION_ITEMS + 1)).is_err());
        let params: CollectionParams = serde_json::from_value(serde_json::json!({
            "platform": "netease", "url": "https://music.163.com/playlist?id=1"
        }))
        .unwrap();
        assert_eq!(params.limit, None);
    }

    #[tokio::test]
    async fn same_platform_collection_preserves_every_source_in_order() {
        let items = (1..=200)
            .map(|id| MatchSourceItem {
                id: id.to_string(),
                song: format!("Track {id}"),
                singer: "Artist".to_string(),
                cover: String::new(),
            })
            .collect();
        let response = match_collection(Json(MatchCollectionRequest {
            source_platform: "netease".to_string(),
            target_platform: "netease".to_string(),
            items,
        }))
        .await
        .unwrap()
        .0;
        assert_eq!(response.matched_count, 200);
        assert_eq!(response.review_count + response.missing_count, 0);
        for (index, item) in response.items.iter().enumerate() {
            assert_eq!(item.source.id, (index + 1).to_string());
            assert_eq!(item.candidates[0].item.id, item.source.id);
        }
    }

    #[test]
    fn parses_stable_playback_id_after_process_restart() {
        assert_eq!(
            parse_playback_id("applemusic--965771855--high"),
            Some((
                "applemusic".to_string(),
                "965771855".to_string(),
                "high".to_string()
            ))
        );
        assert_eq!(
            parse_playback_id("tencent--song-id-with-hyphens--lossless"),
            Some((
                "tencent".to_string(),
                "song-id-with-hyphens".to_string(),
                "lossless".to_string()
            ))
        );
        assert_eq!(parse_playback_id("invalid"), None);
    }

    #[test]
    fn matches_cross_platform_tracks_only_when_identity_is_strong() {
        assert!(matching::score("最初", "逆时针向", "最初", "逆时针向") >= 90);
        assert!(matching::score("最初", "逆时针向", "最初", "另一位歌手") < 90);
    }

    #[test]
    fn exact_song_and_artist_match_overrides_a_close_candidate_tie() {
        let source = MatchSourceItem {
            id: "source".to_string(),
            song: "Mirrors (Acoustic)".to_string(),
            singer: "Beth".to_string(),
            cover: String::new(),
        };
        let candidates = vec![
            crate::music::MusicSearchItem {
                platform: crate::music::parse_platform("netease").unwrap(),
                id: "wrong-version".to_string(),
                song: "Mirrors".to_string(),
                singer: "Beth".to_string(),
                cover: String::new(),
            },
            crate::music::MusicSearchItem {
                platform: crate::music::parse_platform("netease").unwrap(),
                id: "exact-version".to_string(),
                song: "Mirrors (Acoustic)".to_string(),
                singer: "Beth".to_string(),
                cover: String::new(),
            },
        ];
        let item = match_collection_item(source, candidates);
        assert!(matches!(item.status, MatchStatus::Matched));
        assert_eq!(item.candidates[0].item.id, "exact-version");
    }

    #[test]
    fn same_platform_match_passes_source_identity_through() {
        let source = MatchSourceItem {
            id: "123".to_string(),
            song: "最初".to_string(),
            singer: "逆时针向".to_string(),
            cover: "https://example.com/cover.jpg".to_string(),
        };
        let item = direct_match_collection_item(source, "netease");
        assert!(matches!(item.status, MatchStatus::Matched));
        assert_eq!(item.confidence, 100);
        assert_eq!(item.candidates.len(), 1);
        assert_eq!(item.candidates[0].item.id, "123");
        assert_eq!(item.candidates[0].item.platform, "netease");
    }
}
