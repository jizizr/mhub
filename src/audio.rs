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

/// AAC bit rate (kbps) for re-encoding a lossy source of `source_bps`: a
/// quarter above it to absorb the second generation, within 128-320.
pub fn aac_kbps_for(source_bps: Option<u32>) -> u32 {
    let source_kbps = source_bps.filter(|bps| *bps > 0).unwrap_or(192_000) / 1000;
    (source_kbps.saturating_mul(5) / 4).clamp(128, 320)
}

/// `audio` (any format ffmpeg reads) re-encoded as AAC in a plain,
/// non-fragmented M4A with its index up front.
pub async fn transcode_to_m4a(audio: &[u8], kbps: u32) -> ApiResult<Vec<u8>> {
    let dir = env::temp_dir().join(format!("mhub-m4a-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).await?;
    let result = async {
        let input = dir.join("input");
        let output = dir.join("output.m4a");
        fs::write(&input, audio).await?;
        let binary = env::var("MUSIC_API_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
        let finished = Command::new(binary)
            .kill_on_drop(true)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&input)
            .args(["-map", "0:a:0", "-vn", "-c:a", "aac", "-b:a"])
            .arg(format!("{kbps}k"))
            .args(["-movflags", "+faststart", "-f", "mp4"])
            .arg(&output)
            .output()
            .await
            .map_err(|err| ApiError::Custom(format!("转码器启动失败：{err}")))?;
        if !finished.status.success() {
            let stderr = String::from_utf8_lossy(&finished.stderr);
            return Err(ApiError::Custom(format!(
                "音频转 M4A 失败：{}",
                match stderr.trim() {
                    "" => finished.status.to_string(),
                    detail => detail.to_string(),
                }
            )));
        }
        Ok(fs::read(&output).await?)
    }
    .await;
    let _ = fs::remove_dir_all(&dir).await;
    result
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

/// Older Apple Music downloads labelled decrypted AAC as `alac` in the MP4
/// sample entry. Chromium decodes from `esds` regardless; Safari/CoreAudio
/// trust the fourcc and reject the file. Returns the byte offset of such a
/// mislabelled fourcc inside `header` (which must start at the file's first
/// byte and contain the whole `moov`), or `None` when the file is fine.
pub fn mislabeled_aac_entry_offset(header: &[u8]) -> Option<usize> {
    fn boxes(
        data: &[u8],
        start: usize,
        end: usize,
    ) -> impl Iterator<Item = (usize, [u8; 4], usize)> + '_ {
        let mut pos = start;
        std::iter::from_fn(move || {
            if pos + 8 > end {
                return None;
            }
            let size = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
            let typ: [u8; 4] = data[pos + 4..pos + 8].try_into().ok()?;
            let (header_len, size) = match size {
                1 => {
                    let large = u64::from_be_bytes(data.get(pos + 8..pos + 16)?.try_into().ok()?);
                    (16, usize::try_from(large).ok()?)
                }
                0 => (8, end - pos),
                size => (8, size),
            };
            if size < header_len || pos + size > end {
                return None;
            }
            let item = (pos, typ, pos + size);
            pos += size;
            Some(item)
        })
    }
    fn child(data: &[u8], start: usize, end: usize, want: &[u8; 4]) -> Option<(usize, usize)> {
        boxes(data, start, end)
            .find(|(_, typ, _)| typ == want)
            .map(|(pos, _, end)| (pos, end))
    }
    let (moov, moov_end) = child(header, 0, header.len(), b"moov")?;
    for (trak, typ, trak_end) in boxes(header, moov + 8, moov_end) {
        if &typ != b"trak" {
            continue;
        }
        let Some(stsd) = child(header, trak + 8, trak_end, b"mdia")
            .and_then(|(mdia, end)| child(header, mdia + 8, end, b"minf"))
            .and_then(|(minf, end)| child(header, minf + 8, end, b"stbl"))
            .and_then(|(stbl, end)| child(header, stbl + 8, end, b"stsd"))
        else {
            continue;
        };
        for (entry, typ, entry_end) in boxes(header, stsd.0 + 16, stsd.1) {
            // Audio sample entries carry 28 bytes of fixed fields.
            if &typ == b"alac" && child(header, entry + 36, entry_end, b"esds").is_some() {
                return Some(entry + 4);
            }
        }
    }
    None
}

/// Rewrites a mislabelled AAC sample entry in place (four bytes, same size).
/// Only the leading boxes are read, up to the end of a `moov` within the
/// first 4 MiB; a file is checked once per process.
/// Read a file's leading boxes up to and including `moov` (at most 4 MiB).
async fn read_movie_header(file: &mut fs::File) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    const LIMIT: usize = 4 * 1024 * 1024;
    let mut header = Vec::new();
    let mut pos = 0usize;
    loop {
        if header.len() < pos + 16 {
            let want = (pos + 16).max(header.len() + 64 * 1024).min(LIMIT);
            if want <= header.len() {
                break;
            }
            let read = (&mut *file)
                .take((want - header.len()) as u64)
                .read_to_end(&mut header)
                .await?;
            if read == 0 && header.len() < pos + 8 {
                break;
            }
        }
        if header.len() < pos + 8 {
            break;
        }
        let size = u32::from_be_bytes(header[pos..pos + 4].try_into().unwrap()) as usize;
        let typ = &header[pos + 4..pos + 8];
        if size < 8 || typ == b"mdat" {
            break;
        }
        if typ == b"moov" {
            let end = pos + size;
            if end > LIMIT {
                break;
            }
            if header.len() < end {
                (&mut *file)
                    .take((end - header.len()) as u64)
                    .read_to_end(&mut header)
                    .await?;
            }
            break;
        }
        pos += size;
    }
    Ok(header)
}

/// File offset of a mislabelled AAC sample-entry fourcc, without changing
/// the file. Telegram copies are never rewritten: they are patched while
/// streaming instead (see `stream_local_file_patched`). Results are cached
/// per path, since a file is checked on every Range request.
pub async fn find_mislabeled_aac(path: &Path) -> std::io::Result<Option<u64>> {
    static FOUND: LazyLock<Mutex<HashMap<PathBuf, Option<u64>>>> = LazyLock::new(Default::default);
    if let Some(found) = FOUND.lock().unwrap().get(path) {
        return Ok(*found);
    }
    let mut file = fs::File::open(path).await?;
    let found = mislabeled_aac_entry_offset(&read_movie_header(&mut file).await?)
        .map(|offset| offset as u64);
    let mut cache = FOUND.lock().unwrap();
    // The process serves a bounded working set; never grow without limit.
    if cache.len() > 4096 {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), found);
    Ok(found)
}

pub async fn repair_mislabeled_aac(path: &Path) -> std::io::Result<bool> {
    use std::collections::HashSet;
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};
    static CHECKED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);
    if CHECKED.lock().unwrap().contains(path) {
        return Ok(false);
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .await?;
    let header = read_movie_header(&mut file).await?;
    let offset = mislabeled_aac_entry_offset(&header);
    if let Some(offset) = offset {
        file.seek(std::io::SeekFrom::Start(offset as u64)).await?;
        file.write_all(b"mp4a").await?;
        file.sync_data().await?;
    }
    CHECKED.lock().unwrap().insert(path.to_path_buf());
    Ok(offset.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aac_rate_sits_above_the_source_within_bounds() {
        assert_eq!(aac_kbps_for(Some(96_000)), 128);
        assert_eq!(aac_kbps_for(Some(192_000)), 240);
        assert_eq!(aac_kbps_for(Some(320_000)), 320);
        assert_eq!(aac_kbps_for(None), 240);
        assert_eq!(aac_kbps_for(Some(0)), 240);
    }

    /// Needs an ffmpeg with the native Vorbis encoder; skipped without one.
    #[tokio::test]
    async fn ogg_vorbis_becomes_a_plain_m4a() {
        let dir = tempfile::tempdir().unwrap();
        let ogg = dir.path().join("in.ogg");
        let made = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=2")
            .args(["-ac", "2", "-c:a", "vorbis", "-strict", "-2"])
            .arg(&ogg)
            .status()
            .await;
        if !made.is_ok_and(|status| status.success()) {
            eprintln!("skipped: ffmpeg cannot make an OGG Vorbis file here");
            return;
        }
        let source = std::fs::read(&ogg).unwrap();
        assert!(source.starts_with(b"OggS"));
        assert!(crate::lyric_tags::Container::sniff(&source).is_none());

        let m4a = transcode_to_m4a(&source, 128).await.unwrap();
        assert_eq!(
            crate::lyric_tags::Container::sniff(&m4a),
            Some(crate::lyric_tags::Container::Mp4)
        );
        // Index first, and no fragments: lyrics can still be written in.
        let moov = m4a.windows(4).position(|w| w == b"moov").unwrap();
        let mdat = m4a.windows(4).position(|w| w == b"mdat").unwrap();
        assert!(moov < mdat);
        assert!(!m4a.windows(4).any(|w| w == b"moof"));
        crate::lyric_tags::embed(&m4a, "[00:01.00]第一行").unwrap();
    }

    #[tokio::test]
    async fn unreadable_audio_fails_to_transcode() {
        if Command::new("ffmpeg")
            .arg("-version")
            .output()
            .await
            .is_err()
        {
            return;
        }
        assert!(transcode_to_m4a(b"not audio", 128).await.is_err());
    }

    fn test_box(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(typ);
        out.extend_from_slice(payload);
        out
    }

    fn movie_with_entry(entry_type: &[u8; 4], config: &[u8; 4]) -> Vec<u8> {
        let entry = test_box(
            entry_type,
            &[vec![0u8; 28], test_box(config, &[0u8; 12])].concat(),
        );
        let stsd = test_box(b"stsd", &[vec![0, 0, 0, 0, 0, 0, 0, 1], entry].concat());
        let stbl = test_box(b"stbl", &stsd);
        let minf = test_box(b"minf", &[test_box(b"smhd", &[0u8; 8]), stbl].concat());
        let mdia = test_box(b"mdia", &[test_box(b"mdhd", &[0u8; 24]), minf].concat());
        let trak = test_box(b"trak", &[test_box(b"tkhd", &[0u8; 84]), mdia].concat());
        let moov = test_box(b"moov", &[test_box(b"mvhd", &[0u8; 100]), trak].concat());
        [
            test_box(b"ftyp", b"iso5\0\0\0\x01"),
            moov,
            test_box(b"mdat", &[1, 2, 3]),
        ]
        .concat()
    }

    #[tokio::test]
    async fn mislabeled_aac_is_found_without_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.m4a");
        let original = movie_with_entry(b"alac", b"esds");
        fs::write(&path, &original).await.unwrap();
        let offset = find_mislabeled_aac(&path).await.unwrap().unwrap() as usize;
        assert_eq!(&original[offset..offset + 4], b"alac");
        assert_eq!(fs::read(&path).await.unwrap(), original);
        let good = dir.path().join("good.m4a");
        fs::write(&good, movie_with_entry(b"mp4a", b"esds"))
            .await
            .unwrap();
        assert_eq!(find_mislabeled_aac(&good).await.unwrap(), None);
    }

    #[tokio::test]
    async fn mislabeled_aac_entry_is_repaired_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.m4a");
        let original = movie_with_entry(b"alac", b"esds");
        fs::write(&path, &original).await.unwrap();

        assert!(repair_mislabeled_aac(&path).await.unwrap());
        let repaired = fs::read(&path).await.unwrap();
        assert_eq!(repaired.len(), original.len());
        assert_eq!(repaired, movie_with_entry(b"mp4a", b"esds"));
        assert!(!repair_mislabeled_aac(&path).await.unwrap());

        // Real ALAC and correct AAC entries stay untouched.
        assert_eq!(
            mislabeled_aac_entry_offset(&movie_with_entry(b"alac", b"alac")),
            None
        );
        assert_eq!(
            mislabeled_aac_entry_offset(&movie_with_entry(b"mp4a", b"esds")),
            None
        );
    }
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
            entry
                .unwrap()
                .path()
                .extension()
                .is_none_or(|ext| ext != "tmp")
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
                if let Ok(pid) = fs::read_to_string(&pid_path).await
                    && pid.parse::<u32>().is_ok()
                {
                    break pid;
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
            entry
                .unwrap()
                .path()
                .extension()
                .is_none_or(|ext| ext != "tmp")
        }));
    }
}
