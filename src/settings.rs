use lazy_static::lazy_static;
use serde::Deserialize;
use std::{env, fs};

lazy_static! {
    pub static ref SETTINGS: Settings = Settings::load();
}

#[derive(Debug, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub bot: Bot,
    #[serde(default)]
    pub db: DB,
    #[serde(default)]
    pub music: Music,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub audio_cache: AudioCache,
}

/// Local copies of completed songs under `data/downloads`. Telegram and the
/// platforms stay the source of truth; these files only make replays, seeking
/// and ALAC packet streams cheap, so they are evicted by idle time and size.
#[derive(Debug, Deserialize)]
pub struct AudioCache {
    /// Remove a song's local files after this many hours without a request.
    #[serde(default = "default_audio_cache_idle_hours")]
    pub idle_hours: u64,
    /// Keep the directory below this size, least recently used first.
    #[serde(default = "default_audio_cache_max_gib")]
    pub max_gib: u64,
}

impl Default for AudioCache {
    fn default() -> Self {
        Self {
            idle_hours: default_audio_cache_idle_hours(),
            max_gib: default_audio_cache_max_gib(),
        }
    }
}

fn default_audio_cache_idle_hours() -> u64 {
    72
}

fn default_audio_cache_max_gib() -> u64 {
    20
}

/// Accounts and cross-device sync. Disabled while `rp_origin` is empty.
#[derive(Debug, Deserialize)]
pub struct Auth {
    /// Public origin users open, e.g. `https://music.example.com`. Passkeys
    /// are bound to it: changing the domain invalidates existing passkeys.
    #[serde(default)]
    pub rp_origin: String,
    /// Defaults to the origin's host.
    #[serde(default)]
    pub rp_id: String,
    #[serde(default = "default_rp_name")]
    pub rp_name: String,
    /// Cloudflare Turnstile for sign-up. Empty keys disable the challenge.
    #[serde(default)]
    pub turnstile_site_key: String,
    #[serde(default)]
    pub turnstile_secret: String,
    #[serde(default = "default_session_days")]
    pub session_days: u64,
    #[serde(default = "default_true")]
    pub cookie_secure: bool,
    #[serde(default = "default_library_limit_bytes")]
    pub library_limit_bytes: usize,
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            rp_origin: String::new(),
            rp_id: String::new(),
            rp_name: default_rp_name(),
            turnstile_site_key: String::new(),
            turnstile_secret: String::new(),
            session_days: default_session_days(),
            cookie_secure: true,
            library_limit_bytes: default_library_limit_bytes(),
        }
    }
}

fn default_rp_name() -> String {
    "MHub".to_string()
}

fn default_session_days() -> u64 {
    30
}

fn default_library_limit_bytes() -> usize {
    16 * 1024 * 1024
}

#[derive(Debug, Deserialize, Default)]
pub struct Bot {
    #[serde(default)]
    pub owner: i64,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub api_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct DB {
    #[serde(default)]
    pub mysql: Mysql,
}

#[derive(Debug, Deserialize, Default)]
pub struct Mysql {
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct Music {
    #[serde(default)]
    pub applemusic: AppleMusic,
    #[serde(default)]
    pub soda: SodaMusic,
    #[serde(default)]
    pub vkeys: VkeysMusic,
    #[serde(default)]
    pub tencent: TencentMusic,
    #[serde(default)]
    pub netease: NeteaseMusic,
    #[serde(default)]
    pub inline_upload_chat_id: i64,
}

#[derive(Debug, Default, Deserialize)]
pub struct SodaMusic {
    #[serde(default)]
    pub cookie: String,
}

#[derive(Debug, Deserialize)]
pub struct VkeysMusic {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_vkeys_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub token: String,
}

impl Default for VkeysMusic {
    fn default() -> Self {
        Self {
            enabled: true,
            base_url: default_vkeys_base_url(),
            token: String::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct TencentMusic {
    #[serde(default)]
    pub cookie: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct NeteaseMusic {
    #[serde(default)]
    pub cookie: String,
}

#[derive(Debug, Deserialize)]
pub struct AppleMusic {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub media_user_token: String,
    #[serde(default = "default_apple_storefront")]
    pub storefront: String,
    #[serde(default = "default_apple_language")]
    pub language: String,
    #[serde(default = "default_apple_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub wrapper_host: String,
    #[serde(default)]
    pub wrapper_hosts: Vec<String>,
    /// Optional wrapper-lite HTTP endpoint, for example
    /// `http://127.0.0.1:12340`. The legacy raw TCP wrapper remains the
    /// default and is used as a fallback when lite is unavailable.
    #[serde(default)]
    pub wrapper_lite_url: String,
    #[serde(default)]
    pub wrapper_lite_urls: Vec<String>,
    #[serde(default)]
    pub wrapper_lite_enabled: bool,
    #[serde(default = "default_apple_wrapper_lite_batch_size")]
    pub wrapper_lite_batch_size: usize,
    #[serde(default = "default_apple_wrapper_track_concurrency")]
    pub wrapper_track_concurrency: usize,
    #[serde(default = "default_apple_wrapper_fragment_concurrency")]
    pub wrapper_fragment_concurrency: usize,
    #[serde(default = "default_apple_wrapper_connection_concurrency")]
    pub wrapper_connection_concurrency: usize,
    #[serde(default)]
    pub wv_device: String,
    #[serde(default)]
    pub wv_client_id: String,
    #[serde(default)]
    pub wv_private_key: String,
}

impl Default for AppleMusic {
    fn default() -> Self {
        Self {
            enabled: true,
            media_user_token: String::new(),
            storefront: default_apple_storefront(),
            language: default_apple_language(),
            timeout: default_apple_timeout(),
            wrapper_host: String::new(),
            wrapper_hosts: Vec::new(),
            wrapper_lite_url: String::new(),
            wrapper_lite_urls: Vec::new(),
            wrapper_lite_enabled: false,
            wrapper_lite_batch_size: default_apple_wrapper_lite_batch_size(),
            wrapper_track_concurrency: default_apple_wrapper_track_concurrency(),
            wrapper_fragment_concurrency: default_apple_wrapper_fragment_concurrency(),
            wrapper_connection_concurrency: default_apple_wrapper_connection_concurrency(),
            wv_device: String::new(),
            wv_client_id: String::new(),
            wv_private_key: String::new(),
        }
    }
}

impl Settings {
    fn load() -> Self {
        let path = env::var("MUSIC_API_CONFIG").unwrap_or_else(|_| "data/conf.toml".to_string());
        match fs::read_to_string(&path) {
            Ok(content) => toml::from_str(&content).unwrap_or_else(|err| {
                panic!("failed to parse config {path}: {err}");
            }),
            Err(_) => Settings::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_vkeys_base_url() -> String {
    "https://api.vkeys.cn".to_string()
}

fn default_apple_storefront() -> String {
    "us".to_string()
}

fn default_apple_language() -> String {
    "en-US".to_string()
}

fn default_apple_timeout() -> u64 {
    30
}

fn default_apple_wrapper_track_concurrency() -> usize {
    1
}

fn default_apple_wrapper_fragment_concurrency() -> usize {
    2
}

fn default_apple_wrapper_connection_concurrency() -> usize {
    2
}

fn default_apple_wrapper_lite_batch_size() -> usize {
    256
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_example_uses_empty_runtime_credentials() {
        let settings: Settings = toml::from_str(include_str!("../config.example.toml")).unwrap();
        assert!(settings.bot.token.is_empty());
        assert!(settings.db.mysql.url.is_empty());
        assert!(settings.music.applemusic.media_user_token.is_empty());
        assert!(settings.music.applemusic.wv_device.is_empty());
        assert!(settings.music.applemusic.wv_client_id.is_empty());
        assert!(settings.music.applemusic.wv_private_key.is_empty());
        assert!(!settings.music.applemusic.wrapper_lite_enabled);
        assert!(settings.auth.rp_origin.is_empty());
        assert!(settings.auth.turnstile_secret.is_empty());
    }
}
