use crate::{ApiError, ApiResult, settings::SETTINGS};
use lazy_static::lazy_static;
use reqwest::{Client, Response, multipart};
use serde::Deserialize;
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::sync::RwLock;
use url::Url;

lazy_static! {
    // Reuse the connection pool for both getFile and file downloads. Creating a
    // new reqwest Client for every playback request throws away the pool and
    // forces another TCP/TLS handshake, which is especially noticeable at the
    // beginning of a song.
    static ref CLIENT: Client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(60))
        .build()
        .expect("telegram http client");
    // Telegram file paths are stable for a while, but getFile itself is a
    // network round trip. Keep a short-lived in-process cache so every range
    // request made by the browser reuses the same path and connection pool.
    static ref FILE_CACHE: RwLock<HashMap<String, CachedTelegramFile>> =
        RwLock::new(HashMap::new());
}

const FILE_CACHE_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug)]
struct CachedTelegramFile {
    fetched_at: Instant,
    file: TelegramFile,
}

#[derive(Clone, Debug)]
pub struct TelegramFile {
    pub file_id: String,
    pub file_unique_id: String,
    pub file_size: Option<u64>,
    pub file_path: String,
}

#[derive(Clone, Debug)]
pub struct UploadedAudio {
    pub file_id: String,
    pub thumb_file_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramFileResult {
    file_id: String,
    file_unique_id: String,
    file_size: Option<u64>,
    file_path: String,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    audio: Option<TelegramAudio>,
}

#[derive(Debug, Deserialize)]
struct TelegramAudio {
    file_id: String,
    thumbnail: Option<TelegramThumbnail>,
}

#[derive(Debug, Deserialize)]
struct TelegramThumbnail {
    file_id: String,
}

pub async fn get_file(file_id: &str) -> ApiResult<TelegramFile> {
    if let Some(cached) = FILE_CACHE.read().await.get(file_id).cloned()
        && cached.fetched_at.elapsed() < FILE_CACHE_TTL
    {
        return Ok(cached.file);
    }

    let file = get_file_uncached(file_id).await?;
    FILE_CACHE.write().await.insert(
        file_id.to_string(),
        CachedTelegramFile {
            fetched_at: Instant::now(),
            file: file.clone(),
        },
    );
    Ok(file)
}

async fn get_file_uncached(file_id: &str) -> ApiResult<TelegramFile> {
    let token = bot_token()?;
    let url = format!("{}/bot{}/getFile", telegram_api_base(), token);
    let response: TelegramResponse<TelegramFileResult> = CLIENT
        .get(url)
        .query(&[("file_id", file_id)])
        .send()
        .await?
        .json()
        .await?;

    if !response.ok {
        let description = response
            .description
            .unwrap_or_else(|| "unknown error".to_string());
        if description.contains("file is too big") {
            return Err(ApiError::Custom(format!(
                "Telegram getFile failed: {description}. Web 播放需要下载 file_id 对应的字节流；大文件请在 [bot].api_url 配置本地 Telegram Bot API Server"
            )));
        }
        return Err(ApiError::Custom(format!(
            "Telegram getFile failed: {description}"
        )));
    }

    let result = response
        .result
        .ok_or_else(|| ApiError::Custom("Telegram getFile returned no result".to_string()))?;
    Ok(TelegramFile {
        file_id: result.file_id,
        file_unique_id: result.file_unique_id,
        file_size: result.file_size,
        file_path: result.file_path,
    })
}

pub async fn download_file(file_path: &str) -> ApiResult<Response> {
    download_file_with_range(file_path, None).await
}

/// Download a Telegram file, optionally forwarding the browser's single byte
/// range. Telegram's local Bot API server supports Range and preserving it
/// allows the browser to keep its normal media buffering/seek strategy.
pub async fn download_file_with_range(file_path: &str, range: Option<&str>) -> ApiResult<Response> {
    let token = bot_token()?;
    let base = format!("{}/file/bot{}/", telegram_api_base(), token);
    let url = Url::parse(&base)
        .and_then(|base| base.join(file_path))
        .map_err(ApiError::from)?;
    let range = range
        .map(str::trim)
        .filter(|range| range.starts_with("bytes=") && !range.contains(','))
        .map(str::to_owned);

    // GET + Range is idempotent. A short retry here handles transient 502/503
    // responses from a remote Bot API or Telegram edge without making the
    // browser restart its media request from scratch.
    let retry_delays = [Duration::from_millis(100), Duration::from_millis(250)];
    let mut last_status = None;
    for attempt in 0..=retry_delays.len() {
        let mut request = CLIENT.get(url.clone());
        if let Some(range) = range.as_deref() {
            request = request.header(reqwest::header::RANGE, range);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status();
                last_status = Some(status);
                let retryable =
                    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                if !retryable || attempt == retry_delays.len() {
                    break;
                }
            }
            Err(error) => {
                if attempt == retry_delays.len() {
                    return Err(error.into());
                }
            }
        }
        tokio::time::sleep(retry_delays[attempt]).await;
    }

    Err(ApiError::Custom(format!(
        "Telegram file download failed: HTTP {}",
        last_status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "request error".to_string())
    )))
}

pub fn local_file_path(file_path: &str) -> Option<PathBuf> {
    let file_path = file_path.trim();
    if file_path.is_empty() {
        return None;
    }

    let host_root = env::var("MUSIC_API_TELEGRAM_LOCAL_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/telegram-bot-api"));
    let container_root = env::var("MUSIC_API_TELEGRAM_CONTAINER_ROOT")
        .unwrap_or_else(|_| "/var/lib/telegram-bot-api".to_string());
    let container_root = Path::new(container_root.trim_end_matches('/'));
    let path = Path::new(file_path);

    if path.is_absolute() && path.exists() {
        return Some(path.to_path_buf());
    }

    if let Ok(relative) = path.strip_prefix(container_root) {
        let candidate = host_root.join(relative);
        if candidate.exists() {
            return Some(candidate);
        }
        let candidate = host_root.join("data").join(relative);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    let candidate = host_root.join(file_path);
    if candidate.exists() {
        return Some(candidate);
    }

    if let Ok(token) = bot_token() {
        let candidate = host_root.join(token).join(file_path);
        if candidate.exists() {
            return Some(candidate);
        }
        let candidate = host_root.join("data").join(token).join(file_path);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

pub async fn upload_audio(
    chat_id: i64,
    filename: &str,
    content_type: &str,
    audio: Vec<u8>,
    thumbnail: Option<Vec<u8>>,
    title: &str,
    performer: &str,
    duration: Option<u32>,
) -> ApiResult<UploadedAudio> {
    let token = bot_token()?;
    let url = format!("{}/bot{}/sendAudio", telegram_api_base(), token);
    let part = multipart::Part::bytes(audio)
        .file_name(filename.to_string())
        .mime_str(content_type)
        .map_err(|err| ApiError::Custom(format!("invalid audio content type: {err}")))?;
    let mut form = multipart::Form::new()
        .text("chat_id", chat_id.to_string())
        .part("audio", part)
        .text("title", title.to_string())
        .text("performer", performer.to_string());
    if let Some(thumbnail) = thumbnail {
        let part = multipart::Part::bytes(thumbnail)
            .file_name("cover.jpg".to_string())
            .mime_str("image/jpeg")
            .map_err(|err| ApiError::Custom(format!("invalid thumbnail content type: {err}")))?;
        form = form.part("thumbnail", part);
    }
    if let Some(duration) = duration.filter(|duration| *duration > 0) {
        form = form.text("duration", duration.to_string());
    }

    let response: TelegramResponse<TelegramMessage> = CLIENT
        .post(url)
        .multipart(form)
        .send()
        .await?
        .json()
        .await?;
    if !response.ok {
        return Err(ApiError::Custom(format!(
            "Telegram sendAudio failed: {}",
            response
                .description
                .unwrap_or_else(|| "unknown error".to_string())
        )));
    }
    let message = response
        .result
        .ok_or_else(|| ApiError::Custom("Telegram sendAudio returned no message".to_string()))?;
    let audio = message
        .audio
        .ok_or_else(|| ApiError::Custom("Telegram sendAudio returned no audio".to_string()))?;
    Ok(UploadedAudio {
        file_id: audio.file_id,
        thumb_file_id: audio.thumbnail.map(|thumbnail| thumbnail.file_id),
    })
}

fn telegram_api_base() -> String {
    env::var("MUSIC_API_TELEGRAM_API_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            SETTINGS
                .bot
                .api_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "https://api.telegram.org".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn bot_token() -> ApiResult<&'static str> {
    let token = SETTINGS.bot.token.trim();
    if token.is_empty() {
        return Err(ApiError::Custom(
            "bot.token 未配置，无法从 Telegram file_id 下载文件".to_string(),
        ));
    }
    Ok(token)
}
