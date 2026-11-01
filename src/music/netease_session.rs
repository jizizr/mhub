//! NetEase login state: the configured Cookie, an optional refreshed copy and
//! a read-only "is the login still valid" probe.
//!
//! Refreshing is opt-in (`music.netease.refresh_hours`). NetEase may rotate
//! `MUSIC_U` on refresh, so the refreshed Cookie is kept in memory and in a
//! private session file under the data directory. That file is tied to a
//! fingerprint of the configured Cookie: replacing the Cookie in the config
//! makes the old session file stale instead of silently overriding it.

use crate::settings::SETTINGS;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{LazyLock, RwLock},
    time::Duration,
};

const SESSION_FILE: &str = "netease-session.json";
/// Cookie keys a refresh may replace; anything else stays as configured.
const REFRESHED_KEYS: &[&str] = &["MUSIC_U", "__csrf", "MUSIC_A_T", "MUSIC_R_T"];

static REFRESHED: LazyLock<RwLock<Option<String>>> = LazyLock::new(|| RwLock::new(None));

#[derive(Serialize, Deserialize)]
struct SessionFile {
    /// SHA-256 of the configured Cookie this session was refreshed from.
    config_fingerprint: String,
    cookie: String,
}

/// The configured Cookie (config file, then `NETEASE_MUSIC_COOKIE`).
pub(super) fn configured_cookie() -> Option<String> {
    let configured = SETTINGS.music.netease.cookie.trim();
    if configured.is_empty() {
        std::env::var("NETEASE_MUSIC_COOKIE")
            .ok()
            .map(|cookie| cookie.trim().to_string())
            .filter(|cookie| !cookie.is_empty())
    } else {
        Some(configured.to_string())
    }
}

/// The Cookie requests should send: a refreshed one when present.
pub(super) fn current_cookie() -> Option<String> {
    REFRESHED
        .read()
        .ok()
        .and_then(|cookie| cookie.clone())
        .or_else(configured_cookie)
}

fn fingerprint(cookie: &str) -> String {
    format!("{:x}", Sha256::digest(cookie.trim().as_bytes()))
}

/// Replace `REFRESHED_KEYS` in `cookie` with values from `Set-Cookie` headers.
/// Deleted (`deleted`/empty) values are ignored rather than logging out.
pub(super) fn merge_set_cookies<'a>(
    cookie: &str,
    set_cookies: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let mut pairs = cookie
        .split(';')
        .filter_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect::<Vec<_>>();
    let mut changed = false;
    for header in set_cookies {
        let Some((name, value)) = header
            .split(';')
            .next()
            .and_then(|pair| pair.trim().split_once('='))
        else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if !REFRESHED_KEYS.contains(&name) || value.is_empty() || value == "deleted" {
            continue;
        }
        match pairs.iter_mut().find(|(key, _)| key == name) {
            Some((_, current)) if current == value => {}
            Some((_, current)) => {
                *current = value.to_string();
                changed = true;
            }
            None => {
                pairs.push((name.to_string(), value.to_string()));
                changed = true;
            }
        }
    }
    changed.then(|| {
        pairs
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    })
}

fn session_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SESSION_FILE)
}

/// A stored session only applies to the Cookie it was refreshed from.
fn load_session(data_dir: &Path, configured: &str) -> Option<String> {
    let text = std::fs::read_to_string(session_path(data_dir)).ok()?;
    let session = serde_json::from_str::<SessionFile>(&text).ok()?;
    (session.config_fingerprint == fingerprint(configured) && !session.cookie.trim().is_empty())
        .then_some(session.cookie)
}

async fn save_session(data_dir: &Path, configured: &str, cookie: &str) -> std::io::Result<()> {
    let body = serde_json::to_vec(&SessionFile {
        config_fingerprint: fingerprint(configured),
        cookie: cookie.to_string(),
    })?;
    let path = session_path(data_dir);
    let temporary = path.with_extension("json.tmp");
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary).await?;
    tokio::io::AsyncWriteExt::write_all(&mut file, &body).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(temporary, path).await
}

/// Whether the current Cookie is still logged in (`None` when unknown).
pub(super) async fn logged_in() -> Option<bool> {
    current_cookie()?;
    let response: serde_json::Value =
        super::netease::netease_eapi("/api/nuser/account/get", &serde_json::json!({}))
            .await
            .ok()?;
    (response["code"].as_i64() == Some(200)).then(|| !response["profile"].is_null())
}

async fn refresh_once(data_dir: &Path, configured: &str) -> Result<bool, String> {
    let Some(cookie) = current_cookie() else {
        return Ok(false);
    };
    let (body, set_cookies): (serde_json::Value, _) = super::netease::netease_eapi_with_cookies(
        "/api/login/token/refresh",
        &serde_json::json!({}),
    )
    .await
    .map_err(|error| error.to_string())?;
    if body["code"].as_i64() != Some(200) {
        return Err(format!("code {}", body["code"]));
    }
    let Some(refreshed) = merge_set_cookies(&cookie, set_cookies.iter().map(String::as_str)) else {
        return Ok(false);
    };
    if let Ok(mut slot) = REFRESHED.write() {
        *slot = Some(refreshed.clone());
    }
    save_session(data_dir, configured, &refreshed)
        .await
        .map_err(|error| format!("会话文件写入失败：{error}"))?;
    Ok(true)
}

/// Report the login once at startup and, when enabled, refresh periodically.
pub fn spawn_keeper(data_dir: PathBuf) {
    let Some(configured) = configured_cookie() else {
        return;
    };
    let refresh_hours = SETTINGS.music.netease.refresh_hours;
    if refresh_hours > 0
        && let Some(session) = load_session(&data_dir, &configured)
        && let Ok(mut slot) = REFRESHED.write()
    {
        *slot = Some(session);
    }
    tokio::spawn(async move {
        match logged_in().await {
            Some(true) => tracing::info!("NetEase login is valid"),
            Some(false) => {
                tracing::warn!("NetEase Cookie is no longer logged in; member tracks will fail")
            }
            None => tracing::warn!("NetEase login state could not be checked"),
        }
        if refresh_hours == 0 {
            return;
        }
        let interval = Duration::from_secs(refresh_hours.saturating_mul(3600));
        loop {
            tokio::time::sleep(interval).await;
            match refresh_once(&data_dir, &configured).await {
                Ok(true) => tracing::info!("NetEase login refreshed"),
                Ok(false) => tracing::info!("NetEase login refresh returned no new Cookie"),
                Err(error) => tracing::warn!(%error, "NetEase login refresh failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_only_session_keys_from_set_cookie() {
        let merged = merge_set_cookies(
            "MUSIC_U=old; __csrf=a; NMTID=device; os=pc",
            [
                "MUSIC_U=new; Max-Age=1296000; Path=/; HTTPOnly",
                "__csrf=b; Path=/",
                "NMTID=other; Path=/",
                "MUSIC_A_T=deleted; Path=/",
            ],
        )
        .unwrap();
        assert_eq!(merged, "MUSIC_U=new; __csrf=b; NMTID=device; os=pc");
        assert_eq!(
            merge_set_cookies("MUSIC_U=same", ["MUSIC_U=same; Path=/"]),
            None
        );
        assert_eq!(merge_set_cookies("MUSIC_U=a", ["MUSIC_U=; Path=/"]), None);
    }

    #[tokio::test]
    async fn session_file_only_applies_to_the_cookie_it_came_from() {
        let directory = tempfile::tempdir().unwrap();
        save_session(directory.path(), "MUSIC_U=configured", "MUSIC_U=refreshed")
            .await
            .unwrap();
        assert_eq!(
            load_session(directory.path(), " MUSIC_U=configured ").as_deref(),
            Some("MUSIC_U=refreshed")
        );
        assert_eq!(load_session(directory.path(), "MUSIC_U=replaced"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(session_path(directory.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "session file must not be group/world readable"
            );
        }
    }
}
