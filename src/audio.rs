use crate::{ApiError, ApiResult};
use async_trait::async_trait;
use std::{
    collections::HashMap,
    env,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, Weak},
};
use tokio::{fs, process::Command};

type AudioPathLock = tokio::sync::Mutex<()>;
static AUDIO_PATH_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<AudioPathLock>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Serialize producers of the same immutable audio object. Weak entries keep
/// the registry bounded even when many different songs have been played.
pub(crate) async fn lock_audio_path(path: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    let lock = {
        let mut locks = AUDIO_PATH_LOCKS.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        match locks.get(path).and_then(Weak::upgrade) {
            Some(lock) => lock,
            None => {
                let lock = Arc::new(AudioPathLock::new(()));
                locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
                lock
            }
        }
    };
    lock.lock_owned().await
}

/// Removes unfinished output on errors and cancellation, including when an
/// HTTP client drops a request while its Telegram transfer is in progress.
pub(crate) struct PendingAudioFile(PathBuf);

impl PendingAudioFile {
    pub(crate) fn new(destination: &Path) -> ApiResult<Self> {
        let path = destination.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        // Reserve synchronously so cancellation cannot leave an asynchronous
        // create operation that recreates the file after the guard removed it.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self(path))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PendingAudioFile {
    fn drop(&mut self) {
        // Drop cannot await. This only unlinks our uniquely named temporary
        // file; published cache files are never removed here.
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(crate) async fn valid_audio_file(path: &Path, expected_size: Option<u64>) -> bool {
    fs::metadata(path).await.is_ok_and(|metadata| {
        metadata.is_file()
            && metadata.len() > 0
            && expected_size
                .filter(|size| *size > 0)
                .is_none_or(|size| size == metadata.len())
    })
}

/// Fill a temporary file once and atomically publish only a complete result.
/// A cancelled/failed producer releases the lock so the next request retries.
pub(crate) async fn ensure_audio_file<F, Fut>(
    destination: &Path,
    expected_size: Option<u64>,
    produce: F,
) -> ApiResult<PathBuf>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = ApiResult<()>>,
{
    let _guard = lock_audio_path(destination).await;
    if valid_audio_file(destination, expected_size).await {
        return Ok(destination.to_path_buf());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).await?;
    }
    // Remove an incomplete cache left by an older producer. It must not be
    // visible to another endpoint while this request repairs it.
    match fs::remove_file(destination).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let pending = PendingAudioFile::new(destination)?;
    produce(pending.path().to_path_buf()).await?;
    if !valid_audio_file(pending.path(), expected_size).await {
        return Err(ApiError::Custom(
            "音频缓存不完整，未发布缓存文件，请重试播放".to_string(),
        ));
    }
    fs::rename(pending.path(), destination).await?;
    Ok(destination.to_path_buf())
}

#[derive(Debug, Clone)]
pub struct BrowserAudioFile {
    pub path: PathBuf,
    pub filename: String,
    pub content_type: &'static str,
}

#[derive(Debug, Clone)]
pub struct BrowserAudioService<T = FfmpegLosslessTranscoder> {
    transcoder: T,
}

impl Default for BrowserAudioService<FfmpegLosslessTranscoder> {
    fn default() -> Self {
        Self {
            transcoder: FfmpegLosslessTranscoder::from_env(),
        }
    }
}

impl<T> BrowserAudioService<T>
where
    T: LosslessTranscoder,
{
    pub async fn ensure_playable_lossless(
        &self,
        source: &Path,
        work_dir: &Path,
    ) -> ApiResult<BrowserAudioFile> {
        fs::create_dir_all(work_dir).await?;
        let output = work_dir.join("browser-lossless.flac");
        self.transcoder.ensure_flac(source, &output).await?;
        Ok(BrowserAudioFile {
            path: output,
            filename: "browser-lossless.flac".to_string(),
            content_type: "audio/flac",
        })
    }
}

#[async_trait]
pub trait LosslessTranscoder: Clone + Send + Sync + 'static {
    async fn ensure_flac(&self, input: &Path, output: &Path) -> ApiResult<()>;
}

#[derive(Debug, Clone)]
pub struct FfmpegLosslessTranscoder {
    binary: String,
}

impl FfmpegLosslessTranscoder {
    pub fn from_env() -> Self {
        Self {
            binary: env::var("MUSIC_API_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string()),
        }
    }
}

#[async_trait]
impl LosslessTranscoder for FfmpegLosslessTranscoder {
    async fn ensure_flac(&self, input: &Path, output: &Path) -> ApiResult<()> {
        ensure_audio_file(output, None, |temporary| async move {
            let output_result = Command::new(&self.binary)
                .kill_on_drop(true)
                .arg("-hide_banner")
                .arg("-loglevel")
                .arg("error")
                .arg("-y")
                .arg("-i")
                .arg(input)
                .arg("-map")
                .arg("0:a:0")
                .arg("-vn")
                .arg("-c:a")
                .arg("flac")
                .arg("-compression_level")
                .arg("5")
                .arg("-f")
                .arg("flac")
                .arg(&temporary)
                .output()
                .await
                .map_err(|err| ApiError::Http {
                    status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    message: format!("无损转码器启动失败：{err}"),
                })?;

            if !output_result.status.success() {
                let stderr = String::from_utf8_lossy(&output_result.stderr);
                let detail = stderr.trim();
                return Err(ApiError::Http {
                    status: axum::http::StatusCode::BAD_GATEWAY,
                    message: if detail.is_empty() {
                        format!("无损音频转 FLAC 失败：{}", output_result.status)
                    } else {
                        format!("无损音频转 FLAC 失败：{detail}")
                    },
                });
            }

            Ok(())
        })
        .await?;
        Ok(())
    }
}

pub fn needs_browser_lossless_derivative(
    quality: &str,
    format: Option<&str>,
    content_type: Option<&str>,
    filename: Option<&str>,
) -> bool {
    let quality = quality.to_ascii_lowercase();
    if !matches!(quality.as_str(), "lossless" | "hires" | "hi-res") {
        return false;
    }

    let format = format
        .or_else(|| filename.and_then(|name| name.rsplit_once('.').map(|(_, ext)| ext)))
        .unwrap_or_default()
        .to_ascii_lowercase();
    let content_type = content_type.unwrap_or_default().to_ascii_lowercase();

    matches!(format.as_str(), "m4a" | "mp4" | "alac") || content_type.contains("mp4")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    #[tokio::test]
    async fn audio_cache_repairs_old_partial_and_publishes_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("track.m4a");
        fs::write(&destination, b"bad").await.unwrap();
        let published = destination.clone();
        ensure_audio_file(&destination, Some(8), |temporary| async move {
            assert_ne!(temporary, published);
            assert!(!published.exists(), "partial cache must be removed");
            fs::write(&temporary, b"complete").await?;
            assert!(!published.exists(), "output is invisible until complete");
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(fs::read(&destination).await.unwrap(), b"complete");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn failed_or_truncated_audio_cache_never_publishes_partial_output() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("track.m4a");
        for fail in [false, true] {
            let result = ensure_audio_file(&destination, Some(8), |temporary| async move {
                fs::write(temporary, b"bad").await?;
                if fail {
                    Err(ApiError::Custom("connection lost".into()))
                } else {
                    Ok(())
                }
            })
            .await;
            assert!(result.is_err());
            assert!(!destination.exists());
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
        ensure_audio_file(&destination, Some(8), |temporary| async move {
            fs::write(temporary, b"complete").await?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(fs::read(destination).await.unwrap(), b"complete");
    }

    #[tokio::test]
    async fn concurrent_audio_cache_requests_share_one_producer() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("track.m4a");
        let produced = Arc::new(AtomicUsize::new(0));
        let mut requests = Vec::new();
        for _ in 0..8 {
            let destination = destination.clone();
            let produced = Arc::clone(&produced);
            requests.push(tokio::spawn(async move {
                ensure_audio_file(&destination, Some(8), |temporary| async move {
                    produced.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    fs::write(temporary, b"complete").await?;
                    Ok(())
                })
                .await
            }));
        }
        for request in requests {
            request.await.unwrap().unwrap();
        }
        assert_eq!(produced.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read(destination).await.unwrap(), b"complete");
    }

    #[tokio::test]
    async fn cancelling_audio_cache_cleans_temp_and_allows_retry() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("track.m4a");
        let path = destination.clone();
        let (started, received) = tokio::sync::oneshot::channel();
        let request = tokio::spawn(async move {
            ensure_audio_file(&path, Some(8), |temporary| async move {
                fs::write(&temporary, b"bad").await?;
                let _ = started.send(temporary);
                std::future::pending::<ApiResult<()>>().await
            })
            .await
        });
        let temporary = received.await.unwrap();
        assert!(temporary.exists());
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert!(!temporary.exists());
        assert!(!destination.exists());
        ensure_audio_file(&destination, Some(8), |temporary| async move {
            fs::write(temporary, b"complete").await?;
            Ok(())
        })
        .await
        .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_flac_requests_start_one_transcoder_and_failed_output_is_removed() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("fake-ffmpeg");
        let input = directory.path().join("input.m4a");
        let output = directory.path().join("browser-lossless.flac");
        fs::write(&input, b"source").await.unwrap();
        fs::write(&binary, "#!/bin/sh\nfor last; do :; done\nprintf x >> \"$0.calls\"\nsleep 0.05\nprintf fLaCtest > \"$last\"\n").await.unwrap();
        fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))
            .await
            .unwrap();
        let transcoder = FfmpegLosslessTranscoder {
            binary: binary.to_string_lossy().into_owned(),
        };
        let mut requests = Vec::new();
        for _ in 0..6 {
            let transcoder = transcoder.clone();
            let input = input.clone();
            let output = output.clone();
            requests.push(tokio::spawn(async move {
                transcoder.ensure_flac(&input, &output).await
            }));
        }
        for request in requests {
            request.await.unwrap().unwrap();
        }
        assert_eq!(
            fs::read(binary.with_extension("calls")).await.unwrap(),
            b"x"
        );
        assert_eq!(fs::read(&output).await.unwrap(), b"fLaCtest");
        fs::remove_file(&output).await.unwrap();
        fs::write(
            &binary,
            "#!/bin/sh\nfor last; do :; done\nprintf bad > \"$last\"\nexit 1\n",
        )
        .await
        .unwrap();
        assert!(transcoder.ensure_flac(&input, &output).await.is_err());
        assert!(!output.exists());
        assert!(std::fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "tmp")
        }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_flac_request_stops_child_and_removes_temporary_output() {
        use std::{os::unix::fs::PermissionsExt, process::Stdio};
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("fake-ffmpeg");
        let input = directory.path().join("input.m4a");
        let output = directory.path().join("browser-lossless.flac");
        let pid_path = binary.with_extension("pid");
        fs::write(&input, b"source").await.unwrap();
        fs::write(&binary, "#!/bin/sh\nfor last; do :; done\nprintf bad > \"$last\"\nprintf '%s' \"$$\" > \"$0.pid\"\nexec sleep 30\n").await.unwrap();
        fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))
            .await
            .unwrap();
        let transcoder = FfmpegLosslessTranscoder {
            binary: binary.to_string_lossy().into_owned(),
        };
        let pending_output = output.clone();
        let request =
            tokio::spawn(async move { transcoder.ensure_flac(&input, &pending_output).await });
        let pid = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(pid) = fs::read_to_string(&pid_path).await {
                    if pid.parse::<u32>().is_ok() {
                        break pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let alive = Command::new("/bin/kill")
                    .args(["-0", &pid])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .unwrap()
                    .success();
                if !alive {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled transcoder process must exit");
        assert!(!output.exists());
        assert!(std::fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "tmp")
        }));
    }
}
