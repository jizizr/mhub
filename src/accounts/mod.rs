//! Accounts (passkeys with password fallback), server-side sessions, the
//! per-account synced library and cross-device playback hand-off.

pub mod library;
mod routes;
pub mod store;

use crate::{ApiError, ApiResult, settings::Auth};
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dashmap::DashMap;
use library::LibraryHub;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use store::{AccountStore, Session, User};
use webauthn_rs::prelude::*;

pub use routes::router;

pub const SESSION_COOKIE: &str = "mhub_session";
/// Mutating requests must carry this header. A cross-site form cannot set
/// it, and a cross-origin script cannot send it without a CORS preflight.
pub const CLIENT_HEADER: &str = "x-mhub-client";
const CEREMONY_TTL: Duration = Duration::from_secs(300);
/// Session expiry slides forward at most once per this interval.
const SESSION_TOUCH_SECONDS: i64 = 6 * 3600;

pub struct Accounts {
    pub store: Arc<dyn AccountStore>,
    pub library: LibraryHub,
    webauthn: Webauthn,
    settings: AccountSettings,
    registrations: DashMap<String, (PasskeyRegistration, i64, Instant)>,
    authentications: DashMap<String, (DiscoverableAuthentication, Instant)>,
    limiter: RateLimiter,
    http: reqwest::Client,
}

#[derive(Clone, Debug)]
pub struct AccountSettings {
    /// Serialized origin, e.g. `https://music.example.com`.
    pub origin: String,
    pub turnstile_site_key: String,
    pub turnstile_secret: String,
    pub session_days: u64,
    pub cookie_secure: bool,
}

pub struct Authenticated {
    pub token_hash: String,
    pub session: Session,
    pub user: User,
    /// Present when the sliding expiry moved; send it back to the browser.
    pub refreshed_cookie: Option<HeaderValue>,
}

impl Accounts {
    /// `None` when accounts are not configured.
    pub fn from_settings(settings: &Auth, store: Arc<dyn AccountStore>) -> Option<Arc<Self>> {
        let origin = settings.rp_origin.trim();
        if origin.is_empty() {
            return None;
        }
        let url = Url::parse(origin)
            .unwrap_or_else(|error| panic!("auth.rp_origin is not a URL: {error}"));
        let rp_id = if settings.rp_id.trim().is_empty() {
            url.host_str()
                .expect("auth.rp_origin needs a host")
                .to_string()
        } else {
            settings.rp_id.trim().to_string()
        };
        let webauthn = WebauthnBuilder::new(&rp_id, &url)
            .and_then(|builder| builder.rp_name(&settings.rp_name).build())
            .unwrap_or_else(|error| panic!("invalid passkey configuration: {error}"));
        Some(Arc::new(Self::new(
            webauthn,
            store,
            AccountSettings {
                origin: url.origin().ascii_serialization(),
                turnstile_site_key: settings.turnstile_site_key.clone(),
                turnstile_secret: settings.turnstile_secret.clone(),
                session_days: settings.session_days.max(1),
                cookie_secure: settings.cookie_secure,
            },
            settings.library_limit_bytes,
        )))
    }

    pub fn new(
        webauthn: Webauthn,
        store: Arc<dyn AccountStore>,
        settings: AccountSettings,
        library_limit_bytes: usize,
    ) -> Self {
        Self {
            library: LibraryHub::new(store.clone(), library_limit_bytes),
            store,
            webauthn,
            settings,
            registrations: DashMap::new(),
            authentications: DashMap::new(),
            limiter: RateLimiter::default(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("http client"),
        }
    }

    fn session_seconds(&self) -> i64 {
        self.settings.session_days as i64 * 86_400
    }

    pub fn session_cookie(&self, token: &str, max_age: i64) -> HeaderValue {
        let secure = if self.settings.cookie_secure {
            "; Secure"
        } else {
            ""
        };
        HeaderValue::from_str(&format!(
            "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
        ))
        .expect("cookie header")
    }

    pub fn clear_cookie(&self) -> HeaderValue {
        self.session_cookie("", 0)
    }

    /// Create a session and return its `Set-Cookie` value.
    async fn start_session(
        &self,
        user: &User,
        device_id: &str,
        device_name: &str,
    ) -> ApiResult<HeaderValue> {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token = URL_SAFE_NO_PAD.encode(bytes);
        let now = now();
        let session = Session {
            user_id: user.id,
            device_id: clean_device_id(device_id),
            device_name: clean_label(device_name, "未命名设备"),
            created_at: now,
            last_seen: now,
            expires_at: now + self.session_seconds(),
        };
        self.store
            .create_session(&hash_token(&token), &session)
            .await?;
        Ok(self.session_cookie(&token, self.session_seconds()))
    }

    /// Resolve the request's session cookie; `None` when absent or expired.
    pub async fn authenticate(&self, headers: &HeaderMap) -> ApiResult<Option<Authenticated>> {
        let Some(token) = session_token(headers) else {
            return Ok(None);
        };
        let token_hash = hash_token(&token);
        let Some(mut session) = self.store.session(&token_hash).await? else {
            return Ok(None);
        };
        let now = now();
        if session.expires_at <= now {
            self.store.delete_session(&token_hash).await?;
            return Ok(None);
        }
        let Some(user) = self.store.user_by_id(session.user_id).await? else {
            return Ok(None);
        };
        let mut refreshed_cookie = None;
        if now - session.last_seen >= SESSION_TOUCH_SECONDS {
            session.last_seen = now;
            session.expires_at = now + self.session_seconds();
            self.store
                .touch_session(&token_hash, now, session.expires_at)
                .await?;
            refreshed_cookie = Some(self.session_cookie(&token, self.session_seconds()));
        }
        Ok(Some(Authenticated {
            token_hash,
            session,
            user,
            refreshed_cookie,
        }))
    }

    pub async fn require(&self, headers: &HeaderMap) -> ApiResult<Authenticated> {
        self.authenticate(headers)
            .await?
            .ok_or_else(|| http_error(StatusCode::UNAUTHORIZED, "请先登录"))
    }

    async fn verify_turnstile(&self, token: &str, ip: &str) -> ApiResult<()> {
        if self.settings.turnstile_secret.is_empty() {
            return Ok(());
        }
        #[derive(serde::Deserialize)]
        struct Verdict {
            success: bool,
        }
        let verdict: Verdict = self
            .http
            .post("https://challenges.cloudflare.com/turnstile/v0/siteverify")
            .form(&[
                ("secret", self.settings.turnstile_secret.as_str()),
                ("response", token),
                ("remoteip", ip),
            ])
            .send()
            .await?
            .json()
            .await?;
        if verdict.success {
            Ok(())
        } else {
            Err(http_error(StatusCode::FORBIDDEN, "人机验证未通过，请重试"))
        }
    }

    fn prune_ceremonies(&self) {
        self.registrations
            .retain(|_, (_, _, at)| at.elapsed() < CEREMONY_TTL);
        self.authentications
            .retain(|_, (_, at)| at.elapsed() < CEREMONY_TTL);
    }
}

/// Small sliding-window limiter for login and sign-up attempts.
#[derive(Default)]
pub struct RateLimiter {
    hits: DashMap<String, VecDeque<Instant>>,
}

impl RateLimiter {
    pub fn check(&self, key: &str, limit: usize, window: Duration) -> ApiResult<()> {
        let now = Instant::now();
        let mut entry = self.hits.entry(key.to_string()).or_default();
        while entry
            .front()
            .is_some_and(|at| now.duration_since(*at) >= window)
        {
            entry.pop_front();
        }
        if entry.len() >= limit {
            return Err(http_error(
                StatusCode::TOO_MANY_REQUESTS,
                "尝试过于频繁，请稍后再试",
            ));
        }
        entry.push_back(now);
        Ok(())
    }
}

pub fn hash_password(password: &str) -> ApiResult<String> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|error| ApiError::Custom(format!("密码处理失败：{error}")))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

pub fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            pair.trim()
                .strip_prefix(&format!("{SESSION_COOKIE}="))
                .map(str::to_string)
        })
        .filter(|token| !token.is_empty())
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

pub fn http_error(status: StatusCode, message: &str) -> ApiError {
    ApiError::Http {
        status,
        message: message.to_string(),
    }
}

/// Lowercase ASCII names keep accounts unambiguous across devices.
pub fn normalize_username(value: &str) -> ApiResult<String> {
    let name = value.trim().to_ascii_lowercase();
    let valid = (3..=32).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'));
    if valid {
        Ok(name)
    } else {
        Err(http_error(
            StatusCode::BAD_REQUEST,
            "用户名需为 3–32 位小写字母、数字或 _ - .",
        ))
    }
}

pub fn validate_password(password: &str) -> ApiResult<()> {
    if (8..=256).contains(&password.chars().count()) {
        Ok(())
    } else {
        Err(http_error(StatusCode::BAD_REQUEST, "密码至少 8 位"))
    }
}

pub(crate) fn clean_device_id(value: &str) -> String {
    let id: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect();
    if id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        id
    }
}

fn clean_label(value: &str, fallback: &str) -> String {
    let label: String = value
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    if label.is_empty() {
        fallback.to_string()
    } else {
        label
    }
}

/// Client address behind the CDN (falls back to a shared bucket).
pub fn client_ip(headers: &HeaderMap) -> String {
    ["cf-connecting-ip", "x-real-ip", "x-forwarded-for"]
        .iter()
        .find_map(|name| headers.get(*name).and_then(|v| v.to_str().ok()))
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn generate_recovery_codes() -> Vec<String> {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    (0..8)
        .map(|_| {
            let mut bytes = [0u8; 10];
            rand::rng().fill_bytes(&mut bytes);
            let code: String = bytes
                .iter()
                .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
                .collect();
            format!("{}-{}", &code[..5], &code[5..])
        })
        .collect()
}

pub fn hash_recovery_code(code: &str) -> String {
    let normalized: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    hash_token(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_passwords_and_codes() {
        assert_eq!(normalize_username(" Alice_01 ").unwrap(), "alice_01");
        assert!(normalize_username("ab").is_err());
        assert!(normalize_username("名字").is_err());
        assert!(validate_password("short").is_err());
        let hash = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong horse", &hash));
        let codes = generate_recovery_codes();
        assert_eq!(codes.len(), 8);
        assert_eq!(
            hash_recovery_code(&codes[0].to_uppercase()),
            hash_recovery_code(&codes[0].replace('-', ""))
        );
    }

    #[test]
    fn session_cookie_is_read_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; mhub_session=tok; b=2"),
        );
        assert_eq!(session_token(&headers).as_deref(), Some("tok"));
    }

    #[test]
    fn limiter_blocks_after_quota() {
        let limiter = RateLimiter::default();
        for _ in 0..3 {
            limiter.check("k", 3, Duration::from_secs(60)).unwrap();
        }
        assert!(limiter.check("k", 3, Duration::from_secs(60)).is_err());
        limiter.check("other", 3, Duration::from_secs(60)).unwrap();
    }
}
