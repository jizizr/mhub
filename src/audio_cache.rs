//! Eviction for the local copies of completed songs under `data/downloads`.
//!
//! Each playback generation owns one directory (`<playback id>--<revision>`)
//! holding the audio, its cover and lazily derived files (ALAC packets, FLAC
//! for browsers). Telegram and the platforms keep the real copy, so a
//! directory is only a cache: it is removed after it has been idle for a
//! while, and the least recently used go first when the total grows too big.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{LazyLock, Mutex},
    time::{Duration, SystemTime},
};
use tokio::fs;

/// Never touch a directory written or requested this recently: it may be
/// streaming, uploading to Telegram or being repaired by another request.
pub(crate) const ACTIVE_GRACE: Duration = Duration::from_secs(30 * 60);

static LAST_ACCESS: LazyLock<Mutex<HashMap<String, SystemTime>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record a request for a playback generation. Reads do not change file
/// times on most mounts (noatime/relatime), so requests are tracked here and
/// file modification times cover the time before a restart.
pub(crate) fn touch(work_id: &str) {
    LAST_ACCESS
        .lock()
        .unwrap()
        .insert(work_id.to_string(), SystemTime::now());
}

pub(crate) fn last_access(work_id: &str) -> Option<SystemTime> {
    LAST_ACCESS.lock().unwrap().get(work_id).copied()
}

pub(crate) fn forget(work_id: &str) {
    LAST_ACCESS.lock().unwrap().remove(work_id);
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Policy {
    pub idle: Duration,
    pub max_bytes: u64,
}

impl Policy {
    pub(crate) fn from_settings(settings: &crate::settings::AudioCache) -> Self {
        Self {
            idle: Duration::from_secs(settings.idle_hours.max(1) * 3600),
            max_bytes: settings.max_gib.max(1) * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub name: String,
    pub bytes: u64,
    pub last_used: SystemTime,
}

/// List generation directories with their size and newest file time.
pub(crate) async fn scan(root: &Path) -> std::io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut dirs = match fs::read_dir(root).await {
        Ok(dirs) => dirs,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(error),
    };
    while let Some(dir) = dirs.next_entry().await? {
        let Ok(meta) = dir.metadata().await else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let mut bytes = 0;
        let mut last_used = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if let Ok(mut files) = fs::read_dir(dir.path()).await {
            while let Ok(Some(file)) = files.next_entry().await {
                if let Ok(meta) = file.metadata().await {
                    bytes += meta.len();
                    if let Ok(modified) = meta.modified() {
                        last_used = last_used.max(modified);
                    }
                }
            }
        }
        entries.push(Entry {
            name: dir.file_name().to_string_lossy().into_owned(),
            bytes,
            last_used,
        });
    }
    Ok(entries)
}

/// Pick the directories to remove: every idle one, then the least recently
/// used until the rest fits in 90% of the limit, so a full cache does not
/// evict one song per sweep. Protected and recently used ones always stay.
pub(crate) fn select(
    mut entries: Vec<Entry>,
    now: SystemTime,
    policy: Policy,
    protected: &HashSet<String>,
) -> Vec<String> {
    let age = |entry: &Entry| now.duration_since(entry.last_used).unwrap_or_default();
    // The limit counts everything on disk, including what has to stay.
    let mut total: u64 = entries.iter().map(|entry| entry.bytes).sum();
    entries.retain(|entry| !protected.contains(&entry.name) && age(entry) >= ACTIVE_GRACE);
    entries.sort_by_key(|entry| entry.last_used);
    let (idle, mut rest): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|entry| age(entry) >= policy.idle);
    total -= idle.iter().map(|entry| entry.bytes).sum::<u64>();
    let mut evicted: Vec<String> = idle.into_iter().map(|entry| entry.name).collect();
    if total > policy.max_bytes {
        let target = policy.max_bytes / 10 * 9;
        for entry in rest.drain(..) {
            if total <= target {
                break;
            }
            total -= entry.bytes;
            evicted.push(entry.name);
        }
    }
    evicted
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn entry(name: &str, gib: u64, hours_ago: u64, now: SystemTime) -> Entry {
        Entry {
            name: name.into(),
            bytes: gib * GIB,
            last_used: now - Duration::from_secs(hours_ago * 3600),
        }
    }

    fn policy(max_gib: u64) -> Policy {
        Policy {
            idle: Duration::from_secs(72 * 3600),
            max_bytes: max_gib * GIB,
        }
    }

    #[test]
    fn idle_directories_are_removed_and_recent_ones_kept() {
        let now = SystemTime::now();
        let entries = vec![
            entry("old", 1, 100, now),
            entry("yesterday", 1, 24, now),
            entry("playing", 1, 0, now),
        ];
        assert_eq!(select(entries, now, policy(20), &HashSet::new()), ["old"]);
    }

    #[test]
    fn over_limit_evicts_least_recent_down_to_ninety_percent() {
        let now = SystemTime::now();
        // 12 GiB against a 10 GiB limit: drop oldest until <= 9 GiB.
        let entries = (1..=12)
            .map(|hours| entry(&format!("s{hours}"), 1, hours, now))
            .collect();
        let mut evicted = select(entries, now, policy(10), &HashSet::new());
        evicted.sort();
        assert_eq!(evicted, ["s10", "s11", "s12"]);
    }

    #[test]
    fn protected_and_active_directories_survive_even_over_limit() {
        let now = SystemTime::now();
        let entries = vec![
            entry("busy", 8, 200, now),
            entry("streaming", 8, 0, now),
            entry("old", 1, 200, now),
        ];
        let protected = HashSet::from(["busy".to_string()]);
        assert_eq!(select(entries, now, policy(10), &protected), ["old"]);
    }

    #[test]
    fn under_limit_nothing_recent_is_removed() {
        let now = SystemTime::now();
        let entries = vec![entry("a", 2, 5, now), entry("b", 2, 10, now)];
        assert!(select(entries, now, policy(10), &HashSet::new()).is_empty());
    }

    #[tokio::test]
    async fn scan_reports_size_and_newest_file() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("gen");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("audio.m4a"), vec![0; 300]).unwrap();
        std::fs::write(dir.join("cover.jpg"), vec![0; 20]).unwrap();
        std::fs::write(root.path().join("stray.tmp"), b"x").unwrap();
        let entries = scan(root.path()).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "gen");
        assert_eq!(entries[0].bytes, 320);
        assert!(scan(&root.path().join("missing")).await.unwrap().is_empty());
    }
}
