use crate::{ApiError, ApiResult, settings::SETTINGS};
use image::{DynamicImage, ImageFormat, imageops::FilterType};
use lazy_static::lazy_static;
use reqwest::{Client, Response, multipart};
use serde::Deserialize;
use std::{
    collections::HashMap,
    env,
    io::Cursor,
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
    message_id: i64,
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

/// Ask Telegram for the file again, ignoring a remembered answer. A local Bot
/// API server reports a path on its disk, which stops being true once that
/// file is cleaned up.
async fn refresh_file(file_id: &str) -> ApiResult<TelegramFile> {
    FILE_CACHE.write().await.remove(file_id);
    get_file(file_id).await
}

/// The local Bot API file behind `file_id`, when an earlier `getFile` has
/// already put it on disk. Never waits and never asks Telegram.
pub fn cached_local_path(file_id: &str) -> Option<PathBuf> {
    let cache = FILE_CACHE.try_read().ok()?;
    let cached = cache.get(file_id)?;
    if cached.fetched_at.elapsed() >= FILE_CACHE_TTL {
        return None;
    }
    local_file_path(&cached.file.file_path)
}

/// Where a file's bytes are right now. A remembered local path whose file
/// has since left the Bot API server's disk is looked up again, which makes
/// the server fetch it back instead of failing every request until the
/// remembered answer expires.
pub async fn locate_file(file_id: &str) -> ApiResult<TelegramFile> {
    let file = get_file(file_id).await?;
    if is_local_server_path(&file.file_path) && local_file_path(&file.file_path).is_none() {
        return refresh_file(file_id).await;
    }
    Ok(file)
}

/// Whether `file_path` names a file on a local Bot API server's disk, as
/// opposed to a path under Telegram's HTTP file endpoint.
pub fn is_local_server_path(file_path: &str) -> bool {
    Path::new(file_path.trim()).is_absolute()
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

/// Uploads a song for the main bot. With an upload channel configured, the
/// upload bot posts it there and the main bot forwards that post to get its
/// own file id: Telegram stores a file in the uploader's data centre, so a
/// bot whose data centre is near the server uploads many times faster. A
/// failure on that route falls back to uploading directly to `chat_id`,
/// except a file Telegram did not keep as a song: that is the file's fault,
/// and the fallback would only post it to `chat_id` (the owner) as well.
#[allow(clippy::too_many_arguments)]
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
    if let Some((channel_id, uploader)) = upload_relay() {
        let relayed = relay_audio(
            channel_id,
            &uploader,
            filename,
            content_type,
            audio.clone(),
            thumbnail.clone(),
            title,
            performer,
            duration,
        )
        .await;
        match relayed {
            Ok(Some(uploaded)) => return Ok(uploaded),
            Ok(None) => return Err(not_a_song(filename)),
            Err(err) => tracing::warn!("upload bot route failed, uploading directly: {err}"),
        }
    }
    let (_, uploaded) = send_audio(
        bot_token()?,
        chat_id,
        filename,
        content_type,
        audio,
        thumbnail,
        title,
        performer,
        duration,
    )
    .await?;
    uploaded.ok_or_else(|| not_a_song(filename))
}

fn not_a_song(filename: &str) -> ApiError {
    ApiError::Custom(format!("Telegram 没有把 {filename} 存为歌曲"))
}

/// The upload channel and the upload bot's token, when both are configured.
fn upload_relay() -> Option<(i64, String)> {
    let channel_id = SETTINGS.music.upload_channel_id;
    if channel_id == 0 {
        return None;
    }
    let token = std::fs::read_to_string(&SETTINGS.music.upload_bot_token_file).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| (channel_id, token.to_string()))
}

#[allow(clippy::too_many_arguments)]
async fn relay_audio(
    channel_id: i64,
    uploader: &str,
    filename: &str,
    content_type: &str,
    audio: Vec<u8>,
    thumbnail: Option<Vec<u8>>,
    title: &str,
    performer: &str,
    duration: Option<u32>,
) -> ApiResult<Option<UploadedAudio>> {
    let (post_id, posted) = send_audio(
        uploader,
        channel_id,
        filename,
        content_type,
        audio,
        thumbnail,
        title,
        performer,
        duration,
    )
    .await?;
    if posted.is_none() {
        return Ok(None);
    }
    let token = bot_token()?;
    let url = format!("{}/bot{}/forwardMessage", telegram_api_base(), token);
    let response: TelegramResponse<TelegramMessage> = CLIENT
        .post(url)
        .form(&[
            ("chat_id", channel_id.to_string()),
            ("from_chat_id", channel_id.to_string()),
            ("message_id", post_id.to_string()),
            ("disable_notification", "true".to_string()),
        ])
        .send()
        .await?
        .json()
        .await?;
    let forwarded = telegram_result(response, "forwardMessage")?;
    // The upload bot's post keeps the file; the forward was only how the main
    // bot learns its own file id.
    delete_message(token, channel_id, forwarded.message_id).await;
    uploaded_audio(forwarded, "forwardMessage").map(Some)
}

async fn delete_message(token: &str, chat_id: i64, message_id: i64) {
    let delete = format!("{}/bot{}/deleteMessage", telegram_api_base(), token);
    let _ = CLIENT
        .post(delete)
        .form(&[
            ("chat_id", chat_id.to_string()),
            ("message_id", message_id.to_string()),
        ])
        .send()
        .await;
}

fn telegram_result<T>(response: TelegramResponse<T>, method: &str) -> ApiResult<T> {
    if !response.ok {
        return Err(ApiError::Custom(format!(
            "Telegram {method} failed: {}",
            response
                .description
                .unwrap_or_else(|| "unknown error".to_string())
        )));
    }
    response
        .result
        .ok_or_else(|| ApiError::Custom(format!("Telegram {method} returned no message")))
}

fn uploaded_audio(message: TelegramMessage, method: &str) -> ApiResult<UploadedAudio> {
    let audio = message
        .audio
        .ok_or_else(|| ApiError::Custom(format!("Telegram {method} returned no audio")))?;
    Ok(UploadedAudio {
        file_id: audio.file_id,
        thumb_file_id: audio.thumbnail.map(|thumbnail| thumbnail.file_id),
    })
}

/// Sends a song as `token`'s bot; returns the message id and its file ids,
/// or `None` (the message deleted again) when Telegram kept the file as
/// something other than a song, such as an OGG turned into a voice message.
#[allow(clippy::too_many_arguments)]
async fn send_audio(
    token: &str,
    chat_id: i64,
    filename: &str,
    content_type: &str,
    audio: Vec<u8>,
    thumbnail: Option<Vec<u8>>,
    title: &str,
    performer: &str,
    duration: Option<u32>,
) -> ApiResult<(i64, Option<UploadedAudio>)> {
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
    let message = telegram_result(response, "sendAudio")?;
    let message_id = message.message_id;
    if message.audio.is_none() {
        tracing::warn!(
            chat_id,
            filename,
            "Telegram did not keep the upload as a song; deleting it"
        );
        delete_message(token, chat_id, message_id).await;
        return Ok((message_id, None));
    }
    Ok((message_id, Some(uploaded_audio(message, "sendAudio")?)))
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

/// The bytes of a file the bot can see: over MTProto when `size` is known,
/// otherwise from the local Bot API server's disk when it is there.
pub async fn read_file(file_id: &str, size: Option<u64>) -> ApiResult<Vec<u8>> {
    // Read straight from Telegram when the size is known, without making
    // the Bot API server fetch and keep its own copy.
    if let Some(size) = size.filter(|size| *size > 0) {
        match crate::telegram_segments::read_all(file_id, size).await {
            Some(Ok(bytes)) => return Ok(bytes),
            Some(Err(err)) => {
                tracing::warn!("Telegram direct read of a whole file failed, using getFile: {err}")
            }
            None => {}
        }
    }
    let file = locate_file(file_id).await?;
    if let Some(path) = local_file_path(&file.file_path)
        && let Ok(bytes) = tokio::fs::read(&path).await
    {
        return Ok(bytes);
    }
    let response = download_file(&file.file_path).await?;
    let bytes = response.error_for_status()?.bytes().await?;
    if file
        .file_size
        .is_some_and(|size| size != bytes.len() as u64)
    {
        return Err(ApiError::Custom("Telegram 文件下载不完整".to_string()));
    }
    Ok(bytes.to_vec())
}

/// A cover turned into what Telegram accepts as an audio thumbnail: a JPEG
/// of at most 320 px and 200 KB.
pub fn audio_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() {
        return None;
    }
    let image = image::load_from_memory(bytes).ok()?;
    let image = DynamicImage::ImageRgb8(image.resize(320, 320, FilterType::Lanczos3).to_rgb8());
    for quality in [85, 75, 65, 55, 45, 35] {
        let mut out = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        if encoder.encode_image(&image).is_ok() && out.len() <= 200 * 1024 {
            return Some(out);
        }
    }
    let mut out = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)
        .ok()?;
    (out.len() <= 200 * 1024).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    #[test]
    fn cover_is_resized_to_telegram_thumbnail() {
        let image = ImageBuffer::from_fn(1200, 1200, |x, y| {
            Rgb([(x % 255) as u8, (y % 255) as u8, ((x + y) % 255) as u8])
        });
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();

        let thumb = audio_thumbnail(&bytes).unwrap();

        assert!(thumb.len() <= 200 * 1024);
        assert_eq!(&thumb[..3], &[0xff, 0xd8, 0xff]);
        assert!(audio_thumbnail(&[]).is_none());
    }
}
