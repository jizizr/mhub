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
    }
}
