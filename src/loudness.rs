//! On-demand whole-file loudness measurement. This never changes audio bytes,
//! downloads a missing source, or delays the playback response. Measurements
//! describe the completed encoding, not an Apple Sound Check implementation.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, LazyLock},
    time::{Duration, Instant, UNIX_EPOCH},
};
use tokio::{
    fs,
    io::AsyncReadExt,
    process::Command,
    sync::{Mutex, Semaphore},
};

const VERSION: u8 = 1;
const TARGET_LUFS: f64 = -16.0;
const TRUE_PEAK_CEILING: f64 = -1.0;
const MAX_GAIN_DB: f64 = 6.0;
const FAILURE_COOLDOWN: Duration = Duration::from_secs(10 * 60);
const ANALYSIS_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_ENTRIES: usize = 256;
const MAX_PENDING: usize = 16;
const MAX_STDERR_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LoudnessState {
    Pending,
    Ready,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LoudnessResponse {
    pub state: LoudnessState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrated_lufs: Option<f64>,
    /// FFmpeg's true-peak level is expressed relative to digital full scale
    /// (dBTP). This field name is retained as the public API's agreed contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub true_peak_dbfs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f64>,
    pub version: u8,
}

impl LoudnessResponse {
    pub(crate) fn pending() -> Self {
        Self::state(LoudnessState::Pending)
    }

    pub(crate) fn unavailable() -> Self {
        Self::state(LoudnessState::Unavailable)
    }

    fn state(state: LoudnessState) -> Self {
        Self {
            state,
            integrated_lufs: None,
            true_peak_dbfs: None,
            gain_db: None,
            version: VERSION,
        }
    }

    fn measured(integrated_lufs: f64, true_peak_dbfs: f64) -> Option<Self> {
        // ebur128 reports silence as -70 LUFS / -inf peak. Reject missing,
        // non-finite and implausible values instead of amplifying noise.
        if !integrated_lufs.is_finite()
            || !true_peak_dbfs.is_finite()
            || !(-70.0..=10.0).contains(&integrated_lufs)
            || integrated_lufs <= -70.0
            || !(-100.0..=20.0).contains(&true_peak_dbfs)
        {
            return None;
        }
        let gain_db = (TARGET_LUFS - integrated_lufs)
            .min(TRUE_PEAK_CEILING - true_peak_dbfs)
            .min(MAX_GAIN_DB);
        Some(Self {
            state: LoudnessState::Ready,
            integrated_lufs: Some(integrated_lufs),
            true_peak_dbfs: Some(true_peak_dbfs),
            gain_db: Some(gain_db),
            version: VERSION,
        })
    }

    fn validated(&self) -> Option<Self> {
        if self.version != VERSION || self.state != LoudnessState::Ready {
            return None;
        }
        let actual = Self::measured(self.integrated_lufs?, self.true_peak_dbfs?)?;
        let gain = self.gain_db?;
        (gain.is_finite() && (gain - actual.gain_db?).abs() < 0.000_001).then_some(actual)
    }
}

#[derive(Deserialize, Serialize)]
struct DiskMeasurement {
    fingerprint: String,
    result: LoudnessResponse,
}

struct MeasurementInput {
    path: PathBuf,
    fingerprint: String,
    file_signature: String,
    cache_path: PathBuf,
}

impl MeasurementInput {
    async fn new(
        path: &Path,
        expected_size: Option<u64>,
        identity: &str,
        cache_dir: &Path,
    ) -> Option<Self> {
        let path = fs::canonicalize(path).await.ok()?;
        let metadata = fs::metadata(&path).await.ok()?;
        if !metadata.is_file()
            || metadata.len() == 0
            || expected_size
                .filter(|size| *size > 0)
                .is_some_and(|size| size != metadata.len())
        {
            return None;
        }
        let filename = path.file_name()?.to_str()?;
        if filename.ends_with(".alacstream")
            || filename.ends_with(".streaming")
            || filename.ends_with(".tmp")
        {
            return None;
        }
        // Explicitly refuse the custom WASM transport even if its extension
        // is wrong. The normal completed M4A/FLAC source must be available.
        let mut prefix = [0_u8; 8];
        let mut file = fs::File::open(&path).await.ok()?;
        let count = file.read(&mut prefix).await.ok()?;
        if count == prefix.len() && &prefix == b"SPLALAC1" {
            return None;
        }
        let file_signature = file_signature(&metadata)?;
        // Platform / source ID / quality / format come from the task. Path,
        // length, nanosecond mtime and inode distinguish replaced encodings,
        // including old Telegram FLAC and current ALAC under the same ID.
        let fingerprint = format!(
            "{:x}",
            md5::compute(format!(
                "v{VERSION}\0{identity}\0{}\0{file_signature}",
                path.display()
            ))
        );
        let cache_path = cache_dir.join(format!("{fingerprint}.json"));
        Some(Self {
            path,
            fingerprint,
            file_signature,
            cache_path,
        })
    }

    async fn unchanged(&self) -> bool {
        fs::metadata(&self.path)
            .await
            .ok()
            .and_then(|metadata| file_signature(&metadata))
            .is_some_and(|signature| signature == self.file_signature)
    }

    async fn cached(&self) -> Option<LoudnessResponse> {
        let file = fs::File::open(&self.cache_path).await.ok()?;
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes).await.ok()?;
        if bytes.len() > 4096 {
            return None;
        }
        let saved: DiskMeasurement = serde_json::from_slice(&bytes).ok()?;
        if saved.fingerprint != self.fingerprint {
            return None;
        }
        saved.result.validated()
    }

    async fn persist(&self, result: &LoudnessResponse) {
        let Some(directory) = self.cache_path.parent() else {
            return;
        };
        if fs::create_dir_all(directory).await.is_err() {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(&DiskMeasurement {
            fingerprint: self.fingerprint.clone(),
            result: result.clone(),
        }) else {
            return;
        };
        let Ok(pending) = crate::audio::PendingAudioFile::new(&self.cache_path) else {
            return;
        };
        if fs::write(pending.path(), bytes).await.is_ok() {
            let _ = fs::rename(pending.path(), &self.cache_path).await;
        }
    }
}

fn file_signature(metadata: &std::fs::Metadata) -> Option<String> {
    if !metadata.is_file() || metadata.len() == 0 {
        return None;
    }
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    let signature = format!(
        "{}:{}:{}",
        metadata.len(),
        modified.as_secs(),
        modified.subsec_nanos()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!("{signature}:{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    Some(signature)
}

struct Entry {
    result: LoudnessResponse,
    updated: Instant,
}

struct LoudnessService {
    entries: Mutex<HashMap<String, Entry>>,
    worker: Semaphore,
    binary: String,
    timeout: Duration,
}

static SERVICE: LazyLock<Arc<LoudnessService>> = LazyLock::new(|| {
    Arc::new(LoudnessService::new(
        env::var("MUSIC_API_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string()),
        ANALYSIS_TIMEOUT,
    ))
});

impl LoudnessService {
    fn new(binary: String, timeout: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            worker: Semaphore::new(1),
            binary,
            timeout,
        }
    }

    async fn request(self: &Arc<Self>, input: MeasurementInput) -> LoudnessResponse {
        let key = input.cache_path.to_string_lossy().into_owned();
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get(&key) {
            if entry.result.state != LoudnessState::Unavailable
                || entry.updated.elapsed() < FAILURE_COOLDOWN
            {
                return entry.result.clone();
            }
        }
        // Bound the queued work as well as active CPU usage. A later poll may
        // retry a busy service; it never spawns an unbounded waiting process.
        if entries
            .values()
            .filter(|entry| entry.result.state == LoudnessState::Pending)
            .count()
            >= MAX_PENDING
        {
            return LoudnessResponse::pending();
        }
        while entries.len() >= MAX_ENTRIES {
            let oldest = entries
                .iter()
                .filter(|(_, entry)| entry.result.state != LoudnessState::Pending)
                .min_by_key(|(_, entry)| entry.updated)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else { break };
            entries.remove(&oldest);
        }
        entries.insert(
            key.clone(),
            Entry {
                result: LoudnessResponse::pending(),
                updated: Instant::now(),
            },
        );
        let service = self.clone();
        tokio::spawn(async move {
            let result = service
                .analyze(&input)
                .await
                .unwrap_or_else(LoudnessResponse::unavailable);
            service.entries.lock().await.insert(
                key,
                Entry {
                    result,
                    updated: Instant::now(),
                },
            );
        });
        LoudnessResponse::pending()
    }

    async fn analyze(&self, input: &MeasurementInput) -> Option<LoudnessResponse> {
        if !input.unchanged().await {
            return None;
        }
        if let Some(saved) = input.cached().await {
            return Some(saved);
        }
        let _permit = self.worker.acquire().await.ok()?;
        if !input.unchanged().await {
            return None;
        }
        let result = measure(&self.binary, &input.path, self.timeout).await?;
        if !input.unchanged().await {
            return None;
        }
        input.persist(&result).await;
        Some(result)
    }
}

pub(crate) async fn request_completed_file(
    path: &Path,
    expected_size: Option<u64>,
    identity: &str,
    cache_dir: &Path,
) -> LoudnessResponse {
    let Some(input) = MeasurementInput::new(path, expected_size, identity, cache_dir).await else {
        return LoudnessResponse::unavailable();
    };
    SERVICE.request(input).await
}

async fn measure(binary: &str, path: &Path, timeout: Duration) -> Option<LoudnessResponse> {
    // nice executes FFmpeg directly, so kill_on_drop terminates the measured
    // process on timeout. No shell, remote protocols, output media or mutation.
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("nice");
        command.args(["-n", "10"]).arg(binary);
        command
    };
    #[cfg(not(unix))]
    let mut command = Command::new(binary);
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("LC_ALL", "C")
        .args([
            "-nostdin",
            "-hide_banner",
            "-nostats",
            "-loglevel",
            "info",
            "-xerror",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-protocol_whitelist",
            "file,pipe",
            "-format_whitelist",
            "mov,mp4,m4a,3gp,3g2,mj2,flac,mp3,aac,wav,ogg,matroska,webm,aiff,ape,wavpack,mpegts",
            "-i",
        ])
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-sn",
            "-dn",
            "-af",
            "ebur128=peak=true:framelog=verbose:dualmono=true",
            "-f",
            "null",
            "-",
        ]);
    let mut child = command.spawn().ok()?;
    let stderr = child.stderr.take()?;
    let mut bytes = Vec::new();
    let completed = tokio::time::timeout(timeout, async {
        tokio::try_join!(child.wait(), async {
            stderr
                .take(MAX_STDERR_BYTES + 1)
                .read_to_end(&mut bytes)
                .await
        })
    })
    .await
    .ok()?
    .ok()?;
    if !completed.0.success() || bytes.len() > MAX_STDERR_BYTES as usize {
        return None;
    }
    // Never return FFmpeg stderr to callers or logs: it can contain a source
    // filename and embedded tags. Only validated numerical results leave here.
    parse_summary(std::str::from_utf8(&bytes).ok()?)
}

fn parse_summary(stderr: &str) -> Option<LoudnessResponse> {
    let summary = stderr.rsplit_once("Summary:")?.1;
    let integrated = summary.split_once("Integrated loudness:")?.1;
    let integrated = integrated.split_once("Loudness range:")?.0;
    let peak = summary.split_once("True peak:")?.1;
    let parse_value = |section: &str, prefix: &str, unit: &str| {
        section.lines().find_map(|line| {
            let value = line.trim().strip_prefix(prefix)?.trim();
            let mut parts = value.split_whitespace();
            let number = parts.next()?.parse::<f64>().ok()?;
            (parts.next()? == unit).then_some(number)
        })
    };
    LoudnessResponse::measured(
        parse_value(integrated, "I:", "LUFS")?,
        parse_value(peak, "Peak:", "dBFS")?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_preserves_dynamic_range_and_caps_peak_and_boost() {
        let loud = LoudnessResponse::measured(-8.0, -0.2).unwrap();
        assert_eq!(loud.gain_db, Some(-8.0));
        let dynamic = LoudnessResponse::measured(-23.0, -0.5).unwrap();
        assert_eq!(dynamic.gain_db, Some(-0.5));
        let quiet = LoudnessResponse::measured(-30.0, -18.0).unwrap();
        assert_eq!(quiet.gain_db, Some(6.0));
        for (lufs, peak) in [
            (-70.0, f64::NEG_INFINITY),
            (f64::NAN, 0.0),
            (-20.0, f64::INFINITY),
            (30.0, 0.0),
        ] {
            assert!(LoudnessResponse::measured(lufs, peak).is_none());
        }
    }

    #[test]
    fn parses_only_final_true_peak_summary() {
        let text = "Summary:\n Integrated loudness:\n I: -70.0 LUFS\n Loudness range:\n True peak:\n Peak: -inf dBFS\n\n[filter] Summary:\n  Integrated loudness:\n    I: -20.3 LUFS\n    Threshold: -30.3 LUFS\n  Loudness range:\n    LRA: 1.0 LU\n  True peak:\n    Peak: -3.1 dBFS\n";
        let result = parse_summary(text).unwrap();
        assert_eq!(result.integrated_lufs, Some(-20.3));
        assert!((result.gain_db.unwrap() - 2.1).abs() < 0.000_001);
        assert!(parse_summary(&text.replace("True peak:", "Sample peak:")).is_none());
        assert!(parse_summary(&text.replace("-20.3", "nan")).is_none());
    }

    fn write_wav(path: &Path, amplitude: f64) {
        let rate = 48000_u32;
        let frames = rate * 2;
        let data_bytes = frames * 4;
        let mut bytes = Vec::with_capacity(data_bytes as usize + 44);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(data_bytes + 36).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x02\0");
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 4).to_le_bytes());
        bytes.extend_from_slice(b"\x04\0\x10\0data");
        bytes.extend_from_slice(&data_bytes.to_le_bytes());
        for frame in 0..frames {
            let phase = f64::from(frame) * 1000.0 * std::f64::consts::TAU / f64::from(rate);
            let sample = (phase.sin() * amplitude * f64::from(i16::MAX)).round() as i16;
            bytes.extend_from_slice(&sample.to_le_bytes());
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[tokio::test]
    async fn rejects_incomplete_files_and_custom_packets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio.m4a");
        fs::write(&path, b"SPLALAC1\0\0\0\x02test").await.unwrap();
        assert!(
            MeasurementInput::new(&path, None, "a", directory.path())
                .await
                .is_none()
        );
        write_wav(&path, 0.5);
        assert!(
            MeasurementInput::new(&path, Some(1), "a", directory.path())
                .await
                .is_none()
        );
        let original = MeasurementInput::new(&path, None, "a", directory.path())
            .await
            .unwrap();
        let changed_quality = MeasurementInput::new(&path, None, "b", directory.path())
            .await
            .unwrap();
        assert_ne!(original.fingerprint, changed_quality.fingerprint);
        fs::write(&path, b"changed encoding").await.unwrap();
        let changed_file = MeasurementInput::new(&path, None, "a", directory.path())
            .await
            .unwrap();
        assert_ne!(original.fingerprint, changed_file.fingerprint);
        assert!(!original.unchanged().await);
    }

    #[tokio::test]
    async fn persisted_measurements_validate_version_source_and_gain() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio.wav");
        write_wav(&path, 0.5);
        let input = MeasurementInput::new(&path, None, "song", directory.path())
            .await
            .unwrap();
        let result = LoudnessResponse::measured(-10.0, -3.0).unwrap();
        input.persist(&result).await;
        assert_eq!(input.cached().await.unwrap().gain_db, Some(-6.0));
        let mut saved: DiskMeasurement =
            serde_json::from_slice(&fs::read(&input.cache_path).await.unwrap()).unwrap();
        saved.result.gain_db = Some(100.0);
        fs::write(&input.cache_path, serde_json::to_vec(&saved).unwrap())
            .await
            .unwrap();
        assert!(input.cached().await.is_none());
        saved.result = result;
        saved.result.version = 9;
        fs::write(&input.cache_path, serde_json::to_vec(&saved).unwrap())
            .await
            .unwrap();
        assert!(input.cached().await.is_none());
    }

    async fn await_result(
        service: &Arc<LoudnessService>,
        path: &Path,
        cache: &Path,
    ) -> LoudnessResponse {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let input = MeasurementInput::new(path, None, "song", cache)
                    .await
                    .unwrap();
                let result = service.request(input).await;
                if result.state != LoudnessState::Pending {
                    return result;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn missing_analyzer_has_cooldown_and_no_repeated_jobs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio.wav");
        write_wav(&path, 0.2);
        let service = Arc::new(LoudnessService::new(
            "/nonexistent/loudness-analyzer".into(),
            Duration::from_secs(1),
        ));
        assert_eq!(
            await_result(&service, &path, directory.path()).await.state,
            LoudnessState::Unavailable
        );
        let updated = service
            .entries
            .lock()
            .await
            .values()
            .next()
            .unwrap()
            .updated;
        for _ in 0..10 {
            assert_eq!(
                await_result(&service, &path, directory.path()).await.state,
                LoudnessState::Unavailable
            );
        }
        assert_eq!(
            service
                .entries
                .lock()
                .await
                .values()
                .next()
                .unwrap()
                .updated,
            updated
        );
    }

    #[cfg(unix)]
    fn executable(path: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn simultaneous_polling_is_singleflight_and_different_files_run_serially() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.wav");
        let second = directory.path().join("second.wav");
        write_wav(&first, 0.5);
        write_wav(&second, 0.2);
        let binary = directory.path().join("scanner");
        executable(
            &binary,
            "#!/bin/sh\nmkdir \"$0.lock\" || exit 4\ntrap 'rmdir \"$0.lock\"' EXIT\nprintf x >> \"$0.count\"\nsleep 0.1\ncat >&2 <<'SUMMARY'\nSummary:\n Integrated loudness:\n I: -20.0 LUFS\n Loudness range:\n True peak:\n Peak: -3.0 dBFS\nSUMMARY\n",
        );
        let service = Arc::new(LoudnessService::new(
            binary.to_string_lossy().into_owned(),
            Duration::from_secs(3),
        ));
        let polls = (0..20).map(|index| {
            let service = service.clone();
            let path = if index % 2 == 0 {
                first.clone()
            } else {
                second.clone()
            };
            let cache = directory.path().to_path_buf();
            async move { await_result(&service, &path, &cache).await }
        });
        for response in futures::future::join_all(polls).await {
            assert_eq!(response.state, LoudnessState::Ready);
        }
        assert_eq!(
            fs::read(binary.with_extension("count")).await.unwrap(),
            b"xx"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn analyzer_timeout_kills_the_low_priority_process() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("scanner");
        executable(
            &binary,
            "#!/bin/sh\nprintf '%s' \"$$\" > \"$0.pid\"\nexec sleep 10\n",
        );
        let path = directory.path().join("audio.wav");
        write_wav(&path, 0.5);
        assert!(
            measure(binary.to_str().unwrap(), &path, Duration::from_secs(2))
                .await
                .is_none()
        );
        let pid = fs::read_to_string(binary.with_extension("pid"))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let alive = Command::new("kill")
                    .args(["-0", pid.trim()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .unwrap()
                    .success();
                if !alive {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("timed out analyzer must be reaped");
    }

    #[tokio::test]
    #[ignore = "requires local ffmpeg with the ebur128 true-peak scanner"]
    async fn synthetic_wavs_measure_real_loudness_and_reuse_disk_cache() {
        let directory = tempfile::tempdir().unwrap();
        let loud_path = directory.path().join("loud.wav");
        let quiet_path = directory.path().join("quiet.wav");
        let silent_path = directory.path().join("silent.wav");
        write_wav(&loud_path, 0.5);
        write_wav(&quiet_path, 0.05);
        write_wav(&silent_path, 0.0);
        let binary = env::var("MUSIC_API_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
        let loud = measure(&binary, &loud_path, Duration::from_secs(10))
            .await
            .unwrap();
        let quiet = measure(&binary, &quiet_path, Duration::from_secs(10))
            .await
            .unwrap();
        assert!(
            (loud.integrated_lufs.unwrap() - quiet.integrated_lufs.unwrap() - 20.0).abs() < 0.2
        );
        assert!((loud.true_peak_dbfs.unwrap() + 6.0).abs() < 0.2);
        assert!(loud.gain_db.unwrap() < 0.0);
        assert_eq!(quiet.gain_db, Some(6.0));
        assert!(
            measure(&binary, &silent_path, Duration::from_secs(10))
                .await
                .is_none()
        );
        // Completed old Telegram FLAC and current ALAC/M4A represent the
        // same PCM here. Both must work without feeding a custom packet
        // transport to the scanner or changing their stored bytes.
        for (name, codec) in [("old-cache.flac", "flac"), ("current-cache.m4a", "alac")] {
            let encoded = directory.path().join(name);
            let status = Command::new(&binary)
                .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
                .arg(&loud_path)
                .args(["-c:a", codec])
                .arg(&encoded)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .unwrap();
            assert!(status.success());
            let before = fs::read(&encoded).await.unwrap();
            let result = measure(&binary, &encoded, Duration::from_secs(10))
                .await
                .unwrap();
            assert_eq!(result.integrated_lufs, loud.integrated_lufs);
            assert_eq!(result.true_peak_dbfs, loud.true_peak_dbfs);
            assert_eq!(fs::read(&encoded).await.unwrap(), before);
        }
        let input = MeasurementInput::new(&loud_path, None, "song", directory.path())
            .await
            .unwrap();
        input.persist(&loud).await;
        let service = Arc::new(LoudnessService::new(
            "/not/run/when/cached".into(),
            Duration::from_secs(1),
        ));
        let cached = await_result(&service, &loud_path, directory.path()).await;
        assert_eq!(cached.state, LoudnessState::Ready);
        assert_eq!(cached.integrated_lufs, loud.integrated_lufs);
    }
}
