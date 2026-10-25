use super::{
    Accounts, Authenticated, CLIENT_HEADER, client_ip, generate_recovery_codes, hash_password,
    hash_recovery_code, http_error,
    library::{LibraryEvent, MAX_UPDATE_BYTES},
    normalize_username, now,
    store::{PlaybackSession, StoredPasskey},
    validate_password, verify_password,
};
use crate::ApiResult;
use axum::{
    Json, Router,
    extract::{
        Path, State,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::broadcast::error::RecvError;
use webauthn_rs::prelude::*;
use yrs::{
    StateVector,
    sync::{Message, SyncMessage},
    updates::{decoder::Decode, encoder::Encode},
};

const PLAYBACK_PAYLOAD_LIMIT: usize = 256 * 1024;
const PLAYBACK_MAX_AGE_SECONDS: i64 = 7 * 86_400;
static CONNECTIONS: AtomicU64 = AtomicU64::new(1);

type Shared = Arc<Accounts>;

/// Account and sync routes. With `None` only `/auth/config` answers, so the
/// client can hide its account UI.
pub fn router(accounts: Option<Shared>) -> Router {
    let Some(accounts) = accounts else {
        return Router::new().route(
            "/api/v1/auth/config",
            get(|| async { Json(json!({ "enabled": false })) }),
        );
    };
    Router::new()
        .route("/api/v1/auth/config", get(config))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/auth/register", post(register))
        .route("/api/v1/auth/password/login", post(password_login))
        .route("/api/v1/auth/password", post(set_password))
        .route("/api/v1/auth/recovery/login", post(recovery_login))
        .route(
            "/api/v1/auth/passkey/register/start",
            post(passkey_register_start),
        )
        .route(
            "/api/v1/auth/passkey/register/finish",
            post(passkey_register_finish),
        )
        .route(
            "/api/v1/auth/passkey/login/start",
            post(passkey_login_start),
        )
        .route(
            "/api/v1/auth/passkey/login/finish",
            post(passkey_login_finish),
        )
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/sessions", get(sessions))
        .route("/api/v1/auth/sessions/:id", delete(revoke_session))
        .route("/api/v1/sync/ws", get(sync_socket))
        .route("/api/v1/sync/library", post(sync_library))
        .route("/api/v1/sync/playback", get(playback_sessions))
        .route("/api/v1/sync/playback/:device_id", put(put_playback))
        .with_state(accounts)
}

fn check_client(headers: &HeaderMap) -> ApiResult<()> {
    if headers.get(CLIENT_HEADER).is_some() {
        Ok(())
    } else {
        Err(http_error(StatusCode::FORBIDDEN, "缺少客户端标识"))
    }
}

fn with_cookie(cookie: Option<HeaderValue>, body: Value) -> Response {
    let mut response = Json(body).into_response();
    if let Some(cookie) = cookie {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

fn user_json(auth: &Authenticated) -> Value {
    json!({ "id": auth.user.id, "username": auth.user.username, "device_id": auth.session.device_id })
}

#[derive(Deserialize)]
struct DeviceInfo {
    #[serde(default)]
    device_id: String,
    #[serde(default)]
    device_name: String,
}

async fn config(State(accounts): State<Shared>) -> Json<Value> {
    Json(json!({
        "enabled": true,
        "turnstile_site_key": accounts.settings.turnstile_site_key,
    }))
}

async fn me(State(accounts): State<Shared>, headers: HeaderMap) -> ApiResult<Response> {
    let Some(auth) = accounts.authenticate(&headers).await? else {
        return Ok(Json(json!({ "user": null })).into_response());
    };
    let has_password = accounts.store.password(auth.user.id).await?.is_some();
    let passkeys = accounts.store.passkeys(auth.user.id).await?.len();
    let mut body = json!({ "user": user_json(&auth) });
    body["user"]["has_password"] = json!(has_password);
    body["user"]["passkeys"] = json!(passkeys);
    Ok(with_cookie(auth.refreshed_cookie, body))
}

#[derive(Deserialize)]
struct RegisterRequest {
    username: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    turnstile_token: String,
    #[serde(flatten)]
    device: DeviceInfo,
}

async fn register(
    State(accounts): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<RegisterRequest>,
) -> ApiResult<Response> {
    check_client(&headers)?;
    let ip = client_ip(&headers);
    accounts
        .limiter
        .check(&format!("register:{ip}"), 5, Duration::from_secs(3600))?;
    let username = normalize_username(&request.username)?;
    if let Some(password) = &request.password {
        validate_password(password)?;
    }
    accounts
        .verify_turnstile(&request.turnstile_token, &ip)
        .await?;
    let Some(user) = accounts
        .store
        .create_user(&username, &Uuid::new_v4().to_string(), now())
        .await?
    else {
        return Err(http_error(StatusCode::CONFLICT, "用户名已被使用"));
    };
    if let Some(password) = &request.password {
        accounts
            .store
            .set_password(user.id, &hash_password(password)?)
            .await?;
    }
    let codes = generate_recovery_codes();
    accounts
        .store
        .add_recovery_codes(
            user.id,
            &codes
                .iter()
                .map(|c| hash_recovery_code(c))
                .collect::<Vec<_>>(),
        )
        .await?;
    let cookie = accounts
        .start_session(
            &user,
            &request.device.device_id,
            &request.device.device_name,
        )
        .await?;
    Ok(with_cookie(
        Some(cookie),
        json!({ "user": { "id": user.id, "username": user.username }, "recovery_codes": codes }),
    ))
}

#[derive(Deserialize)]
struct PasswordLogin {
    username: String,
    password: String,
    #[serde(flatten)]
    device: DeviceInfo,
}

async fn password_login(
    State(accounts): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<PasswordLogin>,
) -> ApiResult<Response> {
    check_client(&headers)?;
    let ip = client_ip(&headers);
    let username = request.username.trim().to_ascii_lowercase();
    accounts
        .limiter
        .check(&format!("login-ip:{ip}"), 20, Duration::from_secs(600))?;
    accounts.limiter.check(
        &format!("login-user:{username}"),
        8,
        Duration::from_secs(600),
    )?;
    let user = accounts.store.user_by_name(&username).await?;
    let hash = match &user {
        Some(user) => accounts.store.password(user.id).await?,
        None => None,
    };
    // Verify against a real hash even for unknown users to keep timing flat.
    let hash = hash.unwrap_or_else(|| "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$Q6uGcG6uHDZ0bAbYQhQGJ8GkRyfn2JYM1WRMZ5W7qrQ".to_string());
    match user {
        Some(user) if verify_password(&request.password, &hash) => {
            let cookie = accounts
                .start_session(
                    &user,
                    &request.device.device_id,
                    &request.device.device_name,
                )
                .await?;
            Ok(with_cookie(
                Some(cookie),
                json!({ "user": { "id": user.id, "username": user.username } }),
            ))
        }
        _ => Err(http_error(StatusCode::UNAUTHORIZED, "用户名或密码不正确")),
    }
}

#[derive(Deserialize)]
struct SetPassword {
    password: String,
    #[serde(default)]
    current_password: Option<String>,
}

async fn set_password(
    State(accounts): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<SetPassword>,
) -> ApiResult<Json<Value>> {
    check_client(&headers)?;
    let auth = accounts.require(&headers).await?;
    validate_password(&request.password)?;
    if let Some(existing) = accounts.store.password(auth.user.id).await? {
        // A freshly recovered session may replace a forgotten password.
        let recovered = now() - auth.session.created_at < 900
            && auth.session.device_name.ends_with("（恢复码）");
        if !recovered
            && !request
                .current_password
                .as_deref()
                .is_some_and(|current| verify_password(current, &existing))
        {
            return Err(http_error(StatusCode::FORBIDDEN, "当前密码不正确"));
        }
    }
    accounts
        .store
        .set_password(auth.user.id, &hash_password(&request.password)?)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct RecoveryLogin {
    username: String,
    code: String,
    #[serde(flatten)]
    device: DeviceInfo,
}

async fn recovery_login(
    State(accounts): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<RecoveryLogin>,
) -> ApiResult<Response> {
    check_client(&headers)?;
    let ip = client_ip(&headers);
    let username = request.username.trim().to_ascii_lowercase();
    accounts
        .limiter
        .check(&format!("recover-ip:{ip}"), 10, Duration::from_secs(3600))?;
    accounts.limiter.check(
        &format!("recover-user:{username}"),
        5,
        Duration::from_secs(3600),
    )?;
    let Some(user) = accounts.store.user_by_name(&username).await? else {
        return Err(http_error(StatusCode::UNAUTHORIZED, "用户名或恢复码不正确"));
    };
    if !accounts
        .store
        .use_recovery_code(user.id, &hash_recovery_code(&request.code), now())
        .await?
    {
        return Err(http_error(StatusCode::UNAUTHORIZED, "用户名或恢复码不正确"));
    }
    let name = format!("{}（恢复码）", request.device.device_name.trim());
    let cookie = accounts
        .start_session(&user, &request.device.device_id, &name)
        .await?;
    Ok(with_cookie(
        Some(cookie),
        json!({ "user": { "id": user.id, "username": user.username }, "recovered": true }),
    ))
}

async fn passkey_register_start(
    State(accounts): State<Shared>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    check_client(&headers)?;
    let auth = accounts.require(&headers).await?;
    accounts.prune_ceremonies();
    let existing = accounts
        .store
        .passkeys(auth.user.id)
        .await?
        .iter()
        .filter_map(|key| serde_json::from_str::<Passkey>(&key.data).ok())
        .map(|key| key.cred_id().clone())
        .collect::<Vec<_>>();
    let user_handle = Uuid::parse_str(&auth.user.webauthn_id)
        .map_err(|_| http_error(StatusCode::INTERNAL_SERVER_ERROR, "账号标识无效"))?;
    let (mut options, state) = accounts
        .webauthn
        .start_passkey_registration(
            user_handle,
            &auth.user.username,
            &auth.user.username,
            Some(existing),
        )
        .map_err(|error| {
            http_error(
                StatusCode::BAD_REQUEST,
                &format!("无法创建通行密钥：{error}"),
            )
        })?;
    // Sign-in is username-less (discoverable), so the credential must be
    // stored on the authenticator; the library default discourages that.
    if let Some(selection) = options.public_key.authenticator_selection.as_mut() {
        selection.resident_key = Some(webauthn_rs_proto::ResidentKeyRequirement::Required);
        selection.require_resident_key = true;
    }
    let id = Uuid::new_v4().to_string();
    accounts
        .registrations
        .insert(id.clone(), (state, auth.user.id, Instant::now()));
    Ok(Json(json!({ "id": id, "options": options })))
}

#[derive(Deserialize)]
struct PasskeyRegisterFinish {
    id: String,
    credential: RegisterPublicKeyCredential,
    #[serde(default)]
    name: String,
}

async fn passkey_register_finish(
    State(accounts): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<PasskeyRegisterFinish>,
) -> ApiResult<Json<Value>> {
    check_client(&headers)?;
    let auth = accounts.require(&headers).await?;
    let Some((_, (state, user_id, _))) = accounts.registrations.remove(&request.id) else {
        return Err(http_error(
            StatusCode::BAD_REQUEST,
            "通行密钥请求已过期，请重试",
        ));
    };
    if user_id != auth.user.id {
        return Err(http_error(
            StatusCode::FORBIDDEN,
            "通行密钥请求不属于当前账号",
        ));
    }
    let passkey = accounts
        .webauthn
        .finish_passkey_registration(&request.credential, &state)
        .map_err(|error| {
            http_error(
                StatusCode::BAD_REQUEST,
                &format!("通行密钥验证失败：{error}"),
            )
        })?;
    let stored = StoredPasskey {
        // Lowercase hex: safe under the case-insensitive column collation.
        cred_id: passkey
            .cred_id()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        data: serde_json::to_string(&passkey)?,
    };
    let name: String = request.name.trim().chars().take(64).collect();
    accounts
        .store
        .add_passkey(
            auth.user.id,
            &stored,
            if name.is_empty() {
                "通行密钥"
            } else {
                &name
            },
            now(),
        )
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn passkey_login_start(
    State(accounts): State<Shared>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    check_client(&headers)?;
    accounts.limiter.check(
        &format!("passkey-ip:{}", client_ip(&headers)),
        30,
        Duration::from_secs(600),
    )?;
    accounts.prune_ceremonies();
    let (mut options, state) = accounts
        .webauthn
        .start_discoverable_authentication()
        .map_err(|error| {
            http_error(
                StatusCode::BAD_REQUEST,
                &format!("无法使用通行密钥：{error}"),
            )
        })?;
    // The library defaults to autofill ("conditional") mediation; this is an
    // explicit button, so let the browser show its normal passkey prompt.
    options.mediation = None;
    let id = Uuid::new_v4().to_string();
    accounts
        .authentications
        .insert(id.clone(), (state, Instant::now()));
    Ok(Json(json!({ "id": id, "options": options })))
}

#[derive(Deserialize)]
struct PasskeyLoginFinish {
    id: String,
    credential: PublicKeyCredential,
    #[serde(flatten)]
    device: DeviceInfo,
}

async fn passkey_login_finish(
    State(accounts): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<PasskeyLoginFinish>,
) -> ApiResult<Response> {
    check_client(&headers)?;
    let Some((_, (state, _))) = accounts.authentications.remove(&request.id) else {
        return Err(http_error(
            StatusCode::BAD_REQUEST,
            "登录请求已过期，请重试",
        ));
    };
    let failed = || http_error(StatusCode::UNAUTHORIZED, "通行密钥验证失败");
    let (user_handle, _) = accounts
        .webauthn
        .identify_discoverable_authentication(&request.credential)
        .map_err(|_| failed())?;
    let user = accounts
        .store
        .user_by_webauthn_id(&user_handle.to_string())
        .await?
        .ok_or_else(failed)?;
    let mut stored = accounts
        .store
        .passkeys(user.id)
        .await?
        .into_iter()
        .filter_map(|entry| {
            serde_json::from_str::<Passkey>(&entry.data)
                .ok()
                .map(|passkey| (entry, passkey))
        })
        .collect::<Vec<_>>();
    let keys = stored
        .iter()
        .map(|(_, passkey)| DiscoverableKey::from(passkey))
        .collect::<Vec<_>>();
    let result = accounts
        .webauthn
        .finish_discoverable_authentication(&request.credential, state, &keys)
        .map_err(|_| failed())?;
    for (entry, passkey) in &mut stored {
        // Persist the new signature counter / backup state.
        if passkey.update_credential(&result) == Some(true) {
            entry.data = serde_json::to_string(passkey)?;
            accounts.store.update_passkey(entry).await?;
        }
    }
    let cookie = accounts
        .start_session(
            &user,
            &request.device.device_id,
            &request.device.device_name,
        )
        .await?;
    Ok(with_cookie(
        Some(cookie),
        json!({ "user": { "id": user.id, "username": user.username } }),
    ))
}

async fn logout(State(accounts): State<Shared>, headers: HeaderMap) -> ApiResult<Response> {
    check_client(&headers)?;
    if let Some(auth) = accounts.authenticate(&headers).await? {
        accounts.store.delete_session(&auth.token_hash).await?;
        close_live_session(&accounts, auth.user.id, &auth.token_hash);
    }
    Ok(with_cookie(
        Some(accounts.clear_cookie()),
        json!({ "ok": true }),
    ))
}

async fn sessions(State(accounts): State<Shared>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let auth = accounts.require(&headers).await?;
    let list = accounts
        .store
        .sessions(auth.user.id)
        .await?
        .into_iter()
        .filter(|(_, session)| session.expires_at > now())
        .map(|(hash, session)| {
            json!({
                "id": &hash[..16],
                "device_id": session.device_id,
                "device_name": session.device_name,
                "last_seen": session.last_seen,
                "current": hash == auth.token_hash,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "sessions": list })))
}

async fn revoke_session(
    State(accounts): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    check_client(&headers)?;
    let auth = accounts.require(&headers).await?;
    let target = accounts
        .store
        .sessions(auth.user.id)
        .await?
        .into_iter()
        .find(|(hash, _)| hash.starts_with(&id) && id.len() >= 16);
    let Some((hash, _)) = target else {
        return Err(http_error(StatusCode::NOT_FOUND, "设备不存在"));
    };
    accounts.store.delete_session(&hash).await?;
    close_live_session(&accounts, auth.user.id, &hash);
    Ok(Json(json!({ "ok": true })))
}

/// A removed device loses its live connection now, not at the next check.
fn close_live_session(accounts: &Accounts, user_id: i64, token_hash: &str) {
    if let Some(library) = accounts.library.loaded(user_id) {
        library.publish(LibraryEvent::Revoked {
            token_hash: Arc::new(token_hash.to_string()),
        });
    }
}

// ---------------------------------------------------------------------------
// Library sync

#[derive(Deserialize)]
struct SyncRequest {
    /// Base64 Yjs state vector of the client replica.
    state_vector: String,
    /// Base64 Yjs update the server may be missing.
    #[serde(default)]
    update: Option<String>,
}

#[derive(Serialize)]
struct SyncResponse {
    update: String,
    state_vector: String,
}

/// HTTP fallback for networks without WebSocket: one round trip exchanges
/// both directions' missing updates.
async fn sync_library(
    State(accounts): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<SyncRequest>,
) -> ApiResult<Json<SyncResponse>> {
    check_client(&headers)?;
    let auth = accounts.require(&headers).await?;
    let library = accounts.library.open(auth.user.id).await?;
    let decode = |value: &str| {
        STANDARD
            .decode(value)
            .map_err(|_| http_error(StatusCode::BAD_REQUEST, "同步数据编码无效"))
    };
    if let Some(update) = request.update.as_deref().filter(|value| !value.is_empty()) {
        accounts
            .library
            .apply(&library, decode(update)?, 0, now())
            .await?;
    }
    let update = library.diff(&decode(&request.state_vector)?).await?;
    Ok(Json(SyncResponse {
        update: STANDARD.encode(update),
        state_vector: STANDARD.encode(library.state_vector().await),
    }))
}

async fn sync_socket(
    State(accounts): State<Shared>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    // Browsers always send Origin on WebSocket handshakes; refuse other sites.
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if origin != accounts.settings.origin {
            return Err(http_error(StatusCode::FORBIDDEN, "来源不受信任"));
        }
    }
    let auth = accounts.require(&headers).await?;
    let library = accounts.library.open(auth.user.id).await?;
    let token_hash = auth.token_hash.clone();
    Ok(upgrade
        .max_message_size(MAX_UPDATE_BYTES + 1024)
        .on_upgrade(move |socket| {
            serve_socket(
                socket,
                accounts,
                library,
                token_hash,
                auth.session.device_id,
            )
        }))
}

async fn serve_socket(
    socket: WebSocket,
    accounts: Shared,
    library: Arc<super::library::Library>,
    token_hash: String,
    device_id: String,
) {
    let connection = CONNECTIONS.fetch_add(1, Ordering::Relaxed);
    let mut events = library.subscribe();
    let (mut sender, mut receiver) = socket.split();
    let step1 = Message::Sync(SyncMessage::SyncStep1(
        StateVector::decode_v1(&library.state_vector().await).unwrap_or_default(),
    ));
    if sender
        .send(WsMessage::Binary(step1.encode_v1()))
        .await
        .is_err()
    {
        return;
    }
    let mut revalidate = tokio::time::interval(Duration::from_secs(300));
    revalidate.tick().await;
    loop {
        tokio::select! {
            incoming = receiver.next() => {
                let Some(Ok(message)) = incoming else { break };
                let data = match message {
                    WsMessage::Binary(data) => data,
                    WsMessage::Close(_) => break,
                    _ => continue,
                };
                let Ok(message) = Message::decode_v1(&data) else { continue };
                let reply = match message {
                    Message::Sync(SyncMessage::SyncStep1(sv)) => match library.diff(&sv.encode_v1()).await {
                        Ok(diff) => Some(Message::Sync(SyncMessage::SyncStep2(diff))),
                        Err(_) => None,
                    },
                    Message::Sync(SyncMessage::SyncStep2(update)) | Message::Sync(SyncMessage::Update(update)) => {
                        if let Err(error) = accounts.library.apply(&library, update, connection, now()).await {
                            let text = json!({ "type": "error", "message": error.to_string() }).to_string();
                            if sender.send(WsMessage::Text(text)).await.is_err() { break }
                        }
                        None
                    }
                    _ => None,
                };
                if let Some(reply) = reply {
                    if sender.send(WsMessage::Binary(reply.encode_v1())).await.is_err() { break }
                }
            }
            event = events.recv() => {
                let outgoing = match event {
                    Ok(LibraryEvent::Update { origin, data }) if origin != connection => {
                        WsMessage::Binary(Message::Sync(SyncMessage::Update(data.to_vec())).encode_v1())
                    }
                    Ok(LibraryEvent::Update { .. }) => continue,
                    Ok(LibraryEvent::Playback { device_id: from, payload }) if from != device_id => {
                        WsMessage::Text(json!({ "type": "playback", "session": serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null) }).to_string())
                    }
                    Ok(LibraryEvent::Playback { .. }) => continue,
                    Ok(LibraryEvent::Revoked { token_hash: revoked }) if *revoked == token_hash => {
                        let _ = sender.send(WsMessage::Text(json!({ "type": "session-expired" }).to_string())).await;
                        break;
                    }
                    Ok(LibraryEvent::Revoked { .. }) => continue,
                    // Missed broadcasts: resend the whole state (idempotent).
                    Err(RecvError::Lagged(_)) => WsMessage::Binary(Message::Sync(SyncMessage::SyncStep2(library.full_state().await)).encode_v1()),
                    Err(RecvError::Closed) => break,
                };
                if sender.send(outgoing).await.is_err() { break }
            }
            _ = revalidate.tick() => {
                // Revoked or expired sessions lose their live connection.
                match accounts.store.session(&token_hash).await {
                    Ok(Some(session)) if session.expires_at > now() => {}
                    _ => {
                        let _ = sender.send(WsMessage::Text(json!({ "type": "session-expired" }).to_string())).await;
                        break;
                    }
                }
            }
        }
    }
    drop(events);
    accounts.library.release(&library);
}

// ---------------------------------------------------------------------------
// Playback hand-off

async fn playback_sessions(
    State(accounts): State<Shared>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let auth = accounts.require(&headers).await?;
    let cutoff = now() - PLAYBACK_MAX_AGE_SECONDS;
    let sessions = accounts
        .store
        .playback_sessions(auth.user.id)
        .await?
        .into_iter()
        .filter(|session| session.updated_at >= cutoff)
        .filter_map(|session| serde_json::from_str::<Value>(&session.payload).ok())
        .collect::<Vec<_>>();
    Ok(Json(
        json!({ "device_id": auth.session.device_id, "sessions": sessions }),
    ))
}

async fn put_playback(
    State(accounts): State<Shared>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    body: String,
) -> ApiResult<Json<Value>> {
    check_client(&headers)?;
    let auth = accounts.require(&headers).await?;
    if body.len() > PLAYBACK_PAYLOAD_LIMIT {
        return Err(http_error(StatusCode::PAYLOAD_TOO_LARGE, "播放会话过大"));
    }
    let mut payload: Value = serde_json::from_str(&body)
        .map_err(|_| http_error(StatusCode::BAD_REQUEST, "播放会话格式无效"))?;
    // Session device ids are sanitized the same way at sign-in.
    let device_id = super::clean_device_id(&device_id);
    if !payload.is_object() || device_id != auth.session.device_id {
        return Err(http_error(
            StatusCode::BAD_REQUEST,
            "只能更新本设备的播放会话",
        ));
    }
    let updated_at = now();
    payload["device_id"] = json!(device_id);
    payload["device_name"] = json!(auth.session.device_name);
    payload["updated_at"] = json!(updated_at);
    let payload = payload.to_string();
    accounts
        .store
        .put_playback_session(
            auth.user.id,
            &PlaybackSession {
                device_id: device_id.clone(),
                payload: payload.clone(),
                updated_at,
            },
        )
        .await?;
    if let Ok(library) = accounts.library.open(auth.user.id).await {
        library.publish(LibraryEvent::Playback {
            device_id,
            payload: Arc::new(payload),
        });
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountSettings, store::MemoryStore};
    use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
    use yrs::{Array, Doc, ReadTxn, Transact, Update};

    async fn server() -> (String, Shared) {
        let origin = Url::parse("http://localhost").unwrap();
        let webauthn = WebauthnBuilder::new("localhost", &origin)
            .unwrap()
            .build()
            .unwrap();
        let accounts = Arc::new(Accounts::new(
            webauthn,
            Arc::new(MemoryStore::default()),
            AccountSettings {
                origin: "http://localhost".into(),
                turnstile_site_key: String::new(),
                turnstile_secret: String::new(),
                session_days: 30,
                cookie_secure: false,
            },
            16 * 1024 * 1024,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let app = router(Some(accounts.clone()));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, accounts)
    }

    fn cookie_of(response: &reqwest::Response) -> String {
        response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    async fn post(base: &str, path: &str, cookie: Option<&str>, body: Value) -> reqwest::Response {
        let mut request = reqwest::Client::new()
            .post(format!("http://{base}{path}"))
            .header(CLIENT_HEADER, "1")
            .json(&body);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        request.send().await.unwrap()
    }

    #[tokio::test]
    async fn register_login_session_and_logout() {
        let (base, _) = server().await;
        let created = post(&base, "/api/v1/auth/register", None, json!({ "username": "Alice", "password": "correct horse", "device_id": "dev-a", "device_name": "Edge · macOS" })).await;
        assert_eq!(created.status(), StatusCode::OK);
        let cookie = cookie_of(&created);
        let body: Value = created.json().await.unwrap();
        assert_eq!(body["user"]["username"], "alice");
        assert_eq!(body["recovery_codes"].as_array().unwrap().len(), 8);

        let duplicate = post(
            &base,
            "/api/v1/auth/register",
            None,
            json!({ "username": "alice", "password": "another pass" }),
        )
        .await;
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);

        let me: Value = reqwest::Client::new()
            .get(format!("http://{base}/api/v1/auth/me"))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(me["user"]["username"], "alice");
        assert_eq!(me["user"]["device_id"], "dev-a");
        assert_eq!(me["user"]["has_password"], true);

        let wrong = post(
            &base,
            "/api/v1/auth/password/login",
            None,
            json!({ "username": "alice", "password": "nope nope" }),
        )
        .await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        let second = post(
            &base,
            "/api/v1/auth/password/login",
            None,
            json!({ "username": "ALICE", "password": "correct horse", "device_id": "dev-b" }),
        )
        .await;
        assert_eq!(second.status(), StatusCode::OK);

        let missing_header = reqwest::Client::new()
            .post(format!("http://{base}/api/v1/auth/logout"))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(
            missing_header.status(),
            StatusCode::FORBIDDEN,
            "mutations need the client header"
        );
        assert_eq!(
            post(&base, "/api/v1/auth/logout", Some(&cookie), json!({}))
                .await
                .status(),
            StatusCode::OK
        );
        let after: Value = reqwest::Client::new()
            .get(format!("http://{base}/api/v1/auth/me"))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(after["user"].is_null());
    }

    #[tokio::test]
    async fn two_devices_receive_each_others_updates_live_and_over_http() {
        let (base, _) = server().await;
        let created = post(
            &base,
            "/api/v1/auth/register",
            None,
            json!({ "username": "bob", "password": "correct horse", "device_id": "phone" }),
        )
        .await;
        let cookie = cookie_of(&created);

        let connect = |cookie: String| {
            let base = base.clone();
            async move {
                let mut request = format!("ws://{base}/api/v1/sync/ws")
                    .into_client_request()
                    .unwrap();
                request
                    .headers_mut()
                    .insert(header::COOKIE, cookie.parse().unwrap());
                request
                    .headers_mut()
                    .insert(header::ORIGIN, "http://localhost".parse().unwrap());
                tokio_tungstenite::connect_async(request).await.unwrap().0
            }
        };
        let mut a = connect(cookie.clone()).await;
        let mut b = connect(cookie.clone()).await;
        // Each connection first receives the server's SyncStep1.
        for socket in [&mut a, &mut b] {
            let first = socket.next().await.unwrap().unwrap().into_data();
            assert!(matches!(
                Message::decode_v1(&first).unwrap(),
                Message::Sync(SyncMessage::SyncStep1(_))
            ));
        }

        let doc = Doc::new();
        let liked = doc.get_or_insert_array("liked");
        liked.push_back(&mut doc.transact_mut(), "applemusic:1");
        let update = doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        a.send(tungstenite::Message::Binary(
            Message::Sync(SyncMessage::Update(update)).encode_v1(),
        ))
        .await
        .unwrap();

        let received = tokio::time::timeout(Duration::from_secs(5), b.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_data();
        let Message::Sync(SyncMessage::Update(data)) = Message::decode_v1(&received).unwrap()
        else {
            panic!("expected an update")
        };
        let other = Doc::new();
        other
            .transact_mut()
            .apply_update(Update::decode_v1(&data).unwrap())
            .unwrap();
        assert_eq!(other.get_or_insert_array("liked").len(&other.transact()), 1);

        // HTTP fallback from an empty replica returns the same state.
        let response: Value = post(
            &base,
            "/api/v1/sync/library",
            Some(&cookie),
            json!({ "state_vector": STANDARD.encode(StateVector::default().encode_v1()) }),
        )
        .await
        .json()
        .await
        .unwrap();
        let fresh = Doc::new();
        let bytes = STANDARD
            .decode(response["update"].as_str().unwrap())
            .unwrap();
        fresh
            .transact_mut()
            .apply_update(Update::decode_v1(&bytes).unwrap())
            .unwrap();
        assert_eq!(fresh.get_or_insert_array("liked").len(&fresh.transact()), 1);

        // Another site's page cannot open the socket with this cookie.
        let mut foreign = format!("ws://{base}/api/v1/sync/ws")
            .into_client_request()
            .unwrap();
        foreign
            .headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        foreign
            .headers_mut()
            .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(tokio_tungstenite::connect_async(foreign).await.is_err());
    }

    #[tokio::test]
    async fn logging_out_closes_that_devices_live_connection_immediately() {
        let (base, _) = server().await;
        let created = post(
            &base,
            "/api/v1/auth/register",
            None,
            json!({ "username": "dave", "password": "correct horse", "device_id": "tablet" }),
        )
        .await;
        let cookie = cookie_of(&created);
        let mut request = format!("ws://{base}/api/v1/sync/ws")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        socket.next().await.unwrap().unwrap();
        assert_eq!(
            post(&base, "/api/v1/auth/logout", Some(&cookie), json!({}))
                .await
                .status(),
            StatusCode::OK
        );
        let notice = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(notice.to_text().unwrap()).unwrap()["type"],
            "session-expired"
        );
        let closed = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap();
        assert!(
            !matches!(closed, Some(Ok(tungstenite::Message::Binary(_)))),
            "no further sync after revocation"
        );
    }

    #[tokio::test]
    async fn playback_sessions_are_per_device_and_pushed_to_other_devices() {
        let (base, _) = server().await;
        let phone = cookie_of(&post(&base, "/api/v1/auth/register", None, json!({ "username": "carol", "password": "correct horse", "device_id": "phone", "device_name": "iPhone" })).await);
        let laptop = cookie_of(&post(&base, "/api/v1/auth/password/login", None, json!({ "username": "carol", "password": "correct horse", "device_id": "laptop", "device_name": "Mac" })).await);

        let mut request = format!("ws://{base}/api/v1/sync/ws")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(header::COOKIE, laptop.parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        socket.next().await.unwrap().unwrap();

        let session = json!({ "queue": [{ "key": "applemusic:1", "title": "晴天" }], "play_index": 0, "position_ms": 61000 });
        let put = reqwest::Client::new()
            .put(format!("http://{base}/api/v1/sync/playback/phone"))
            .header(CLIENT_HEADER, "1")
            .header(header::COOKIE, &phone)
            .body(session.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(put.status(), StatusCode::OK);
        let other_device = reqwest::Client::new()
            .put(format!("http://{base}/api/v1/sync/playback/laptop"))
            .header(CLIENT_HEADER, "1")
            .header(header::COOKIE, &phone)
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(
            other_device.status(),
            StatusCode::BAD_REQUEST,
            "a device only writes its own session"
        );

        let pushed = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let event: Value = serde_json::from_str(pushed.to_text().unwrap()).unwrap();
        assert_eq!(event["type"], "playback");
        assert_eq!(event["session"]["device_name"], "iPhone");
        assert_eq!(event["session"]["position_ms"], 61000);

        let listed: Value = reqwest::Client::new()
            .get(format!("http://{base}/api/v1/sync/playback"))
            .header(header::COOKIE, &laptop)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed["device_id"], "laptop");
        assert_eq!(listed["sessions"][0]["device_id"], "phone");
    }
}
