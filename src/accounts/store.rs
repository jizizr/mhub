//! Persistence for accounts, sessions, the synced library and playback
//! hand-off. MySQL in production; `MemoryStore` keeps tests database-free.

use crate::{ApiError, ApiResult};
use async_trait::async_trait;
use sqlx::{MySqlPool, Row};
use std::{collections::HashMap, sync::Mutex};
use tokio::sync::OnceCell;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub username: String,
    /// WebAuthn user handle; discoverable passkeys identify the user by it.
    pub webauthn_id: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub user_id: i64,
    pub device_id: String,
    pub device_name: String,
    pub created_at: i64,
    pub last_seen: i64,
    pub expires_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredPasskey {
    pub cred_id: String,
    pub data: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredLibrary {
    pub snapshot: Option<Vec<u8>>,
    /// Updates newer than the snapshot, oldest first, with their row id.
    pub updates: Vec<(i64, Vec<u8>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackSession {
    pub device_id: String,
    pub payload: String,
    pub updated_at: i64,
}

#[async_trait]
pub trait AccountStore: Send + Sync {
    /// `Ok(None)` when the username is taken.
    async fn create_user(
        &self,
        username: &str,
        webauthn_id: &str,
        now: i64,
    ) -> ApiResult<Option<User>>;
    async fn user_by_name(&self, username: &str) -> ApiResult<Option<User>>;
    async fn user_by_id(&self, id: i64) -> ApiResult<Option<User>>;
    async fn user_by_webauthn_id(&self, webauthn_id: &str) -> ApiResult<Option<User>>;

    async fn set_password(&self, user_id: i64, hash: &str) -> ApiResult<()>;
    async fn password(&self, user_id: i64) -> ApiResult<Option<String>>;

    async fn add_passkey(
        &self,
        user_id: i64,
        passkey: &StoredPasskey,
        name: &str,
        now: i64,
    ) -> ApiResult<()>;
    async fn passkeys(&self, user_id: i64) -> ApiResult<Vec<StoredPasskey>>;
    async fn update_passkey(&self, passkey: &StoredPasskey) -> ApiResult<()>;

    async fn create_session(&self, token_hash: &str, session: &Session) -> ApiResult<()>;
    async fn session(&self, token_hash: &str) -> ApiResult<Option<Session>>;
    async fn touch_session(
        &self,
        token_hash: &str,
        last_seen: i64,
        expires_at: i64,
    ) -> ApiResult<()>;
    async fn delete_session(&self, token_hash: &str) -> ApiResult<()>;
    async fn sessions(&self, user_id: i64) -> ApiResult<Vec<(String, Session)>>;

    async fn add_recovery_codes(&self, user_id: i64, hashes: &[String]) -> ApiResult<()>;
    /// Marks the code used; `false` when unknown or already used.
    async fn use_recovery_code(&self, user_id: i64, hash: &str, now: i64) -> ApiResult<bool>;

    async fn library(&self, user_id: i64) -> ApiResult<StoredLibrary>;
    async fn append_library_update(&self, user_id: i64, update: &[u8], now: i64) -> ApiResult<i64>;
    /// Replace the snapshot and drop the updates it already contains.
    async fn compact_library(
        &self,
        user_id: i64,
        snapshot: &[u8],
        through_id: i64,
        now: i64,
    ) -> ApiResult<()>;

    async fn put_playback_session(&self, user_id: i64, session: &PlaybackSession) -> ApiResult<()>;
    async fn playback_sessions(&self, user_id: i64) -> ApiResult<Vec<PlaybackSession>>;
}

// ---------------------------------------------------------------------------
// MySQL

pub struct MySqlStore {
    url: String,
    pool: OnceCell<MySqlPool>,
}

impl MySqlStore {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            pool: OnceCell::new(),
        }
    }

    async fn pool(&self) -> ApiResult<&MySqlPool> {
        self.pool
            .get_or_try_init(|| async {
                if self.url.trim().is_empty() {
                    return Err(ApiError::Custom(
                        "账号功能需要配置 db.mysql.url".to_string(),
                    ));
                }
                let pool = sqlx::mysql::MySqlPoolOptions::new()
                    .max_connections(8)
                    .connect(&self.url)
                    .await?;
                for statement in SCHEMA {
                    sqlx::query(statement).execute(&pool).await?;
                }
                Ok::<_, ApiError>(pool)
            })
            .await
    }
}

// Avoid binary (`*_bin`) collations: the MySQL driver reports such text
// columns as VARBINARY and refuses to decode them as strings. Usernames are
// stored lowercase and credential ids as lowercase hex, so case-insensitive
// collations cannot merge distinct values.
const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS mhub_users (
        id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
        username VARCHAR(64) NOT NULL,
        webauthn_id CHAR(36) NOT NULL,
        created_at BIGINT NOT NULL,
        UNIQUE KEY uniq_mhub_users_name (username),
        UNIQUE KEY uniq_mhub_users_webauthn (webauthn_id)
    ) CHARACTER SET utf8mb4",
    "CREATE TABLE IF NOT EXISTS mhub_passwords (
        user_id BIGINT NOT NULL PRIMARY KEY,
        hash VARCHAR(255) NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS mhub_passkeys (
        id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
        user_id BIGINT NOT NULL,
        cred_id VARCHAR(512) NOT NULL,
        data TEXT NOT NULL,
        name VARCHAR(128) NOT NULL,
        created_at BIGINT NOT NULL,
        UNIQUE KEY uniq_mhub_passkeys_cred (cred_id),
        KEY idx_mhub_passkeys_user (user_id)
    ) CHARACTER SET utf8mb4",
    "CREATE TABLE IF NOT EXISTS mhub_sessions (
        token_hash CHAR(64) NOT NULL PRIMARY KEY,
        user_id BIGINT NOT NULL,
        device_id VARCHAR(64) NOT NULL,
        device_name VARCHAR(128) NOT NULL,
        created_at BIGINT NOT NULL,
        last_seen BIGINT NOT NULL,
        expires_at BIGINT NOT NULL,
        KEY idx_mhub_sessions_user (user_id)
    ) CHARACTER SET utf8mb4",
    "CREATE TABLE IF NOT EXISTS mhub_recovery_codes (
        user_id BIGINT NOT NULL,
        code_hash CHAR(64) NOT NULL,
        used_at BIGINT NULL,
        PRIMARY KEY (user_id, code_hash)
    )",
    "CREATE TABLE IF NOT EXISTS mhub_library_snapshots (
        user_id BIGINT NOT NULL PRIMARY KEY,
        state LONGBLOB NOT NULL,
        updated_at BIGINT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS mhub_library_updates (
        id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
        user_id BIGINT NOT NULL,
        data MEDIUMBLOB NOT NULL,
        created_at BIGINT NOT NULL,
        KEY idx_mhub_library_updates_user (user_id, id)
    )",
    "CREATE TABLE IF NOT EXISTS mhub_playback_sessions (
        user_id BIGINT NOT NULL,
        device_id VARCHAR(64) NOT NULL,
        payload MEDIUMTEXT NOT NULL,
        updated_at BIGINT NOT NULL,
        PRIMARY KEY (user_id, device_id)
    ) CHARACTER SET utf8mb4",
];

fn user_from_row(row: sqlx::mysql::MySqlRow) -> ApiResult<User> {
    Ok(User {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        webauthn_id: row.try_get("webauthn_id")?,
        created_at: row.try_get("created_at")?,
    })
}

fn session_from_row(row: &sqlx::mysql::MySqlRow) -> ApiResult<Session> {
    Ok(Session {
        user_id: row.try_get("user_id")?,
        device_id: row.try_get("device_id")?,
        device_name: row.try_get("device_name")?,
        created_at: row.try_get("created_at")?,
        last_seen: row.try_get("last_seen")?,
        expires_at: row.try_get("expires_at")?,
    })
}

const USER_COLUMNS: &str = "SELECT id, username, webauthn_id, created_at FROM mhub_users";

#[async_trait]
impl AccountStore for MySqlStore {
    async fn create_user(
        &self,
        username: &str,
        webauthn_id: &str,
        now: i64,
    ) -> ApiResult<Option<User>> {
        let pool = self.pool().await?;
        let result = sqlx::query(
            "INSERT IGNORE INTO mhub_users (username, webauthn_id, created_at) VALUES (?, ?, ?)",
        )
        .bind(username)
        .bind(webauthn_id)
        .bind(now)
        .execute(pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        Ok(Some(User {
            id: result.last_insert_id() as i64,
            username: username.to_string(),
            webauthn_id: webauthn_id.to_string(),
            created_at: now,
        }))
    }

    async fn user_by_name(&self, username: &str) -> ApiResult<Option<User>> {
        let row = sqlx::query(&format!("{USER_COLUMNS} WHERE username = ?"))
            .bind(username)
            .fetch_optional(self.pool().await?)
            .await?;
        row.map(user_from_row).transpose()
    }

    async fn user_by_id(&self, id: i64) -> ApiResult<Option<User>> {
        let row = sqlx::query(&format!("{USER_COLUMNS} WHERE id = ?"))
            .bind(id)
            .fetch_optional(self.pool().await?)
            .await?;
        row.map(user_from_row).transpose()
    }

    async fn user_by_webauthn_id(&self, webauthn_id: &str) -> ApiResult<Option<User>> {
        let row = sqlx::query(&format!("{USER_COLUMNS} WHERE webauthn_id = ?"))
            .bind(webauthn_id)
            .fetch_optional(self.pool().await?)
            .await?;
        row.map(user_from_row).transpose()
    }

    async fn set_password(&self, user_id: i64, hash: &str) -> ApiResult<()> {
        sqlx::query("INSERT INTO mhub_passwords (user_id, hash) VALUES (?, ?) ON DUPLICATE KEY UPDATE hash = VALUES(hash)")
            .bind(user_id)
            .bind(hash)
            .execute(self.pool().await?)
            .await?;
        Ok(())
    }

    async fn password(&self, user_id: i64) -> ApiResult<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT hash FROM mhub_passwords WHERE user_id = ?")
                .bind(user_id)
                .fetch_optional(self.pool().await?)
                .await?,
        )
    }

    async fn add_passkey(
        &self,
        user_id: i64,
        passkey: &StoredPasskey,
        name: &str,
        now: i64,
    ) -> ApiResult<()> {
        sqlx::query("INSERT INTO mhub_passkeys (user_id, cred_id, data, name, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(user_id)
            .bind(&passkey.cred_id)
            .bind(&passkey.data)
            .bind(name)
            .bind(now)
            .execute(self.pool().await?)
            .await?;
        Ok(())
    }

    async fn passkeys(&self, user_id: i64) -> ApiResult<Vec<StoredPasskey>> {
        let rows =
            sqlx::query("SELECT cred_id, data FROM mhub_passkeys WHERE user_id = ? ORDER BY id")
                .bind(user_id)
                .fetch_all(self.pool().await?)
                .await?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredPasskey {
                    cred_id: row.try_get("cred_id")?,
                    data: row.try_get("data")?,
                })
            })
            .collect()
    }

    async fn update_passkey(&self, passkey: &StoredPasskey) -> ApiResult<()> {
        sqlx::query("UPDATE mhub_passkeys SET data = ? WHERE cred_id = ?")
            .bind(&passkey.data)
            .bind(&passkey.cred_id)
            .execute(self.pool().await?)
            .await?;
        Ok(())
    }

    async fn create_session(&self, token_hash: &str, session: &Session) -> ApiResult<()> {
        sqlx::query("INSERT INTO mhub_sessions (token_hash, user_id, device_id, device_name, created_at, last_seen, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(token_hash)
            .bind(session.user_id)
            .bind(&session.device_id)
            .bind(&session.device_name)
            .bind(session.created_at)
            .bind(session.last_seen)
            .bind(session.expires_at)
            .execute(self.pool().await?)
            .await?;
        Ok(())
    }

    async fn session(&self, token_hash: &str) -> ApiResult<Option<Session>> {
        let row = sqlx::query("SELECT user_id, device_id, device_name, created_at, last_seen, expires_at FROM mhub_sessions WHERE token_hash = ?")
            .bind(token_hash)
            .fetch_optional(self.pool().await?)
            .await?;
        row.as_ref().map(session_from_row).transpose()
    }

    async fn touch_session(
        &self,
        token_hash: &str,
        last_seen: i64,
        expires_at: i64,
    ) -> ApiResult<()> {
        sqlx::query("UPDATE mhub_sessions SET last_seen = ?, expires_at = ? WHERE token_hash = ?")
            .bind(last_seen)
            .bind(expires_at)
            .bind(token_hash)
            .execute(self.pool().await?)
            .await?;
        Ok(())
    }

    async fn delete_session(&self, token_hash: &str) -> ApiResult<()> {
        sqlx::query("DELETE FROM mhub_sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(self.pool().await?)
            .await?;
        Ok(())
    }

    async fn sessions(&self, user_id: i64) -> ApiResult<Vec<(String, Session)>> {
        let rows = sqlx::query("SELECT token_hash, user_id, device_id, device_name, created_at, last_seen, expires_at FROM mhub_sessions WHERE user_id = ? ORDER BY last_seen DESC")
            .bind(user_id)
            .fetch_all(self.pool().await?)
            .await?;
        rows.iter()
            .map(|row| Ok((row.try_get("token_hash")?, session_from_row(row)?)))
            .collect()
    }

    async fn add_recovery_codes(&self, user_id: i64, hashes: &[String]) -> ApiResult<()> {
        let pool = self.pool().await?;
        for hash in hashes {
            sqlx::query("INSERT INTO mhub_recovery_codes (user_id, code_hash) VALUES (?, ?)")
                .bind(user_id)
                .bind(hash)
                .execute(pool)
                .await?;
        }
        Ok(())
    }

    async fn use_recovery_code(&self, user_id: i64, hash: &str, now: i64) -> ApiResult<bool> {
        let result = sqlx::query("UPDATE mhub_recovery_codes SET used_at = ? WHERE user_id = ? AND code_hash = ? AND used_at IS NULL")
            .bind(now)
            .bind(user_id)
            .bind(hash)
            .execute(self.pool().await?)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn library(&self, user_id: i64) -> ApiResult<StoredLibrary> {
        let pool = self.pool().await?;
        let snapshot: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT state FROM mhub_library_snapshots WHERE user_id = ?")
                .bind(user_id)
                .fetch_optional(pool)
                .await?;
        let rows =
            sqlx::query("SELECT id, data FROM mhub_library_updates WHERE user_id = ? ORDER BY id")
                .bind(user_id)
                .fetch_all(pool)
                .await?;
        let updates = rows
            .into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("data")?)))
            .collect::<ApiResult<_>>()?;
        Ok(StoredLibrary { snapshot, updates })
    }

    async fn append_library_update(&self, user_id: i64, update: &[u8], now: i64) -> ApiResult<i64> {
        let result = sqlx::query(
            "INSERT INTO mhub_library_updates (user_id, data, created_at) VALUES (?, ?, ?)",
        )
        .bind(user_id)
        .bind(update)
        .bind(now)
        .execute(self.pool().await?)
        .await?;
        Ok(result.last_insert_id() as i64)
    }

    async fn compact_library(
        &self,
        user_id: i64,
        snapshot: &[u8],
        through_id: i64,
        now: i64,
    ) -> ApiResult<()> {
        let mut transaction = self.pool().await?.begin().await?;
        sqlx::query("INSERT INTO mhub_library_snapshots (user_id, state, updated_at) VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE state = VALUES(state), updated_at = VALUES(updated_at)")
            .bind(user_id)
            .bind(snapshot)
            .bind(now)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM mhub_library_updates WHERE user_id = ? AND id <= ?")
            .bind(user_id)
            .bind(through_id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn put_playback_session(&self, user_id: i64, session: &PlaybackSession) -> ApiResult<()> {
        sqlx::query("INSERT INTO mhub_playback_sessions (user_id, device_id, payload, updated_at) VALUES (?, ?, ?, ?) ON DUPLICATE KEY UPDATE payload = VALUES(payload), updated_at = VALUES(updated_at)")
            .bind(user_id)
            .bind(&session.device_id)
            .bind(&session.payload)
            .bind(session.updated_at)
            .execute(self.pool().await?)
            .await?;
        Ok(())
    }

    async fn playback_sessions(&self, user_id: i64) -> ApiResult<Vec<PlaybackSession>> {
        let rows = sqlx::query("SELECT device_id, payload, updated_at FROM mhub_playback_sessions WHERE user_id = ? ORDER BY updated_at DESC")
            .bind(user_id)
            .fetch_all(self.pool().await?)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(PlaybackSession {
                    device_id: row.try_get("device_id")?,
                    payload: row.try_get("payload")?,
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// In-memory (tests)

#[derive(Default)]
struct MemoryData {
    users: Vec<User>,
    passwords: HashMap<i64, String>,
    passkeys: Vec<(i64, StoredPasskey)>,
    sessions: HashMap<String, Session>,
    recovery: Vec<(i64, String, bool)>,
    snapshots: HashMap<i64, Vec<u8>>,
    updates: Vec<(i64, i64, Vec<u8>)>,
    next_update: i64,
    playback: HashMap<(i64, String), PlaybackSession>,
}

#[derive(Default)]
pub struct MemoryStore {
    data: Mutex<MemoryData>,
}

#[async_trait]
impl AccountStore for MemoryStore {
    async fn create_user(
        &self,
        username: &str,
        webauthn_id: &str,
        now: i64,
    ) -> ApiResult<Option<User>> {
        let mut data = self.data.lock().unwrap();
        if data.users.iter().any(|user| user.username == username) {
            return Ok(None);
        }
        let user = User {
            id: data.users.len() as i64 + 1,
            username: username.into(),
            webauthn_id: webauthn_id.into(),
            created_at: now,
        };
        data.users.push(user.clone());
        Ok(Some(user))
    }

    async fn user_by_name(&self, username: &str) -> ApiResult<Option<User>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .users
            .iter()
            .find(|user| user.username == username)
            .cloned())
    }

    async fn user_by_id(&self, id: i64) -> ApiResult<Option<User>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .users
            .iter()
            .find(|user| user.id == id)
            .cloned())
    }

    async fn user_by_webauthn_id(&self, webauthn_id: &str) -> ApiResult<Option<User>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .users
            .iter()
            .find(|user| user.webauthn_id == webauthn_id)
            .cloned())
    }

    async fn set_password(&self, user_id: i64, hash: &str) -> ApiResult<()> {
        self.data
            .lock()
            .unwrap()
            .passwords
            .insert(user_id, hash.into());
        Ok(())
    }

    async fn password(&self, user_id: i64) -> ApiResult<Option<String>> {
        Ok(self.data.lock().unwrap().passwords.get(&user_id).cloned())
    }

    async fn add_passkey(
        &self,
        user_id: i64,
        passkey: &StoredPasskey,
        _name: &str,
        _now: i64,
    ) -> ApiResult<()> {
        self.data
            .lock()
            .unwrap()
            .passkeys
            .push((user_id, passkey.clone()));
        Ok(())
    }

    async fn passkeys(&self, user_id: i64) -> ApiResult<Vec<StoredPasskey>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .passkeys
            .iter()
            .filter(|(id, _)| *id == user_id)
            .map(|(_, key)| key.clone())
            .collect())
    }

    async fn update_passkey(&self, passkey: &StoredPasskey) -> ApiResult<()> {
        let mut data = self.data.lock().unwrap();
        if let Some((_, existing)) = data
            .passkeys
            .iter_mut()
            .find(|(_, key)| key.cred_id == passkey.cred_id)
        {
            *existing = passkey.clone();
        }
        Ok(())
    }

    async fn create_session(&self, token_hash: &str, session: &Session) -> ApiResult<()> {
        self.data
            .lock()
            .unwrap()
            .sessions
            .insert(token_hash.into(), session.clone());
        Ok(())
    }

    async fn session(&self, token_hash: &str) -> ApiResult<Option<Session>> {
        Ok(self.data.lock().unwrap().sessions.get(token_hash).cloned())
    }

    async fn touch_session(
        &self,
        token_hash: &str,
        last_seen: i64,
        expires_at: i64,
    ) -> ApiResult<()> {
        if let Some(session) = self.data.lock().unwrap().sessions.get_mut(token_hash) {
            session.last_seen = last_seen;
            session.expires_at = expires_at;
        }
        Ok(())
    }

    async fn delete_session(&self, token_hash: &str) -> ApiResult<()> {
        self.data.lock().unwrap().sessions.remove(token_hash);
        Ok(())
    }

    async fn sessions(&self, user_id: i64) -> ApiResult<Vec<(String, Session)>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .sessions
            .iter()
            .filter(|(_, s)| s.user_id == user_id)
            .map(|(k, s)| (k.clone(), s.clone()))
            .collect())
    }

    async fn add_recovery_codes(&self, user_id: i64, hashes: &[String]) -> ApiResult<()> {
        self.data
            .lock()
            .unwrap()
            .recovery
            .extend(hashes.iter().map(|hash| (user_id, hash.clone(), false)));
        Ok(())
    }

    async fn use_recovery_code(&self, user_id: i64, hash: &str, _now: i64) -> ApiResult<bool> {
        let mut data = self.data.lock().unwrap();
        match data
            .recovery
            .iter_mut()
            .find(|(id, code, used)| *id == user_id && code == hash && !*used)
        {
            Some(entry) => {
                entry.2 = true;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn library(&self, user_id: i64) -> ApiResult<StoredLibrary> {
        let data = self.data.lock().unwrap();
        Ok(StoredLibrary {
            snapshot: data.snapshots.get(&user_id).cloned(),
            updates: data
                .updates
                .iter()
                .filter(|(user, _, _)| *user == user_id)
                .map(|(_, id, update)| (*id, update.clone()))
                .collect(),
        })
    }

    async fn append_library_update(
        &self,
        user_id: i64,
        update: &[u8],
        _now: i64,
    ) -> ApiResult<i64> {
        let mut data = self.data.lock().unwrap();
        data.next_update += 1;
        let id = data.next_update;
        data.updates.push((user_id, id, update.to_vec()));
        Ok(id)
    }

    async fn compact_library(
        &self,
        user_id: i64,
        snapshot: &[u8],
        through_id: i64,
        _now: i64,
    ) -> ApiResult<()> {
        let mut data = self.data.lock().unwrap();
        data.snapshots.insert(user_id, snapshot.to_vec());
        data.updates
            .retain(|(user, id, _)| *user != user_id || *id > through_id);
        Ok(())
    }

    async fn put_playback_session(&self, user_id: i64, session: &PlaybackSession) -> ApiResult<()> {
        self.data
            .lock()
            .unwrap()
            .playback
            .insert((user_id, session.device_id.clone()), session.clone());
        Ok(())
    }

    async fn playback_sessions(&self, user_id: i64) -> ApiResult<Vec<PlaybackSession>> {
        let mut sessions = self
            .data
            .lock()
            .unwrap()
            .playback
            .iter()
            .filter(|((user, _), _)| *user == user_id)
            .map(|(_, session)| session.clone())
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(sessions)
    }
}
