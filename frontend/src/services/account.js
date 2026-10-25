// Account API, device identity and WebAuthn (passkey) helpers. Kept small and
// free of Yjs so guests never download the sync engine.

const API = "/api/v1/auth";
const DEVICE_KEY = "music-web-device-v1";
export const SYNC_META_KEY = "music-web-sync-v1";

async function call(path, { method = "GET", body } = {}) {
  const response = await fetch(path, {
    method,
    credentials: "same-origin",
    cache: "no-store",
    headers: {
      "x-mhub-client": "1",
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    const error = new Error(payload.detail || payload.message || `请求失败（${response.status}）`);
    error.status = response.status;
    throw error;
  }
  return payload;
}

function describeDevice() {
  const agent = navigator.userAgent || "";
  const browser = /Edg\//.test(agent) ? "Edge" : /Firefox\//.test(agent) ? "Firefox"
    : /Chrome\//.test(agent) ? "Chrome" : /Safari\//.test(agent) ? "Safari" : "浏览器";
  const system = /iPhone/.test(agent) ? "iPhone" : /iPad/.test(agent) ? "iPad" : /Android/.test(agent) ? "Android"
    : /Mac OS X/.test(agent) ? "macOS" : /Windows/.test(agent) ? "Windows" : /Linux/.test(agent) ? "Linux" : "设备";
  return `${browser} · ${system}`;
}

/** Stable per-browser identity used for sessions and playback hand-off. */
export function deviceIdentity() {
  try {
    const saved = JSON.parse(localStorage.getItem(DEVICE_KEY) || "null");
    if (saved?.id) return saved;
  } catch { /* Recreate below. */ }
  const identity = {
    id: globalThis.crypto?.randomUUID?.() || `device-${Date.now()}-${Math.random().toString(16).slice(2)}`,
    name: describeDevice(),
  };
  try { localStorage.setItem(DEVICE_KEY, JSON.stringify(identity)); } catch { /* Session-only. */ }
  return identity;
}

function device() {
  const { id, name } = deviceIdentity();
  return { device_id: id, device_name: name };
}

export function readSyncMeta() {
  try {
    const meta = JSON.parse(localStorage.getItem(SYNC_META_KEY) || "null");
    return meta?.accountId ? meta : null;
  } catch {
    return null;
  }
}

export function writeSyncMeta(meta) {
  if (meta) localStorage.setItem(SYNC_META_KEY, JSON.stringify(meta));
  else localStorage.removeItem(SYNC_META_KEY);
}

export const getAuthConfig = () => call(`${API}/config`).catch(() => ({ enabled: false }));
export const getMe = () => call(`${API}/me`);
export const register = ({ username, password, turnstileToken }) =>
  call(`${API}/register`, { method: "POST", body: { username, password: password || undefined, turnstile_token: turnstileToken || "", ...device() } });
export const passwordLogin = ({ username, password }) =>
  call(`${API}/password/login`, { method: "POST", body: { username, password, ...device() } });
export const recoveryLogin = ({ username, code }) =>
  call(`${API}/recovery/login`, { method: "POST", body: { username, code, ...device() } });
export const setPassword = ({ password, currentPassword }) =>
  call(`${API}/password`, { method: "POST", body: { password, current_password: currentPassword || undefined } });
export const logout = () => call(`${API}/logout`, { method: "POST", body: {} });
export const listSessions = () => call(`${API}/sessions`);
export const revokeSession = (id) => call(`${API}/sessions/${encodeURIComponent(id)}`, { method: "DELETE" });

// --- Passkeys ---------------------------------------------------------------

export function passkeysSupported() {
  return typeof window !== "undefined" && "PublicKeyCredential" in window && Boolean(navigator.credentials?.create);
}

const fromBase64Url = (value) => {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
  return Uint8Array.from(atob(base64), (char) => char.charCodeAt(0)).buffer;
};

const toBase64Url = (buffer) => btoa(String.fromCharCode(...new Uint8Array(buffer)))
  .replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

export function creationOptions(publicKey) {
  return {
    ...publicKey,
    challenge: fromBase64Url(publicKey.challenge),
    user: { ...publicKey.user, id: fromBase64Url(publicKey.user.id) },
    excludeCredentials: (publicKey.excludeCredentials || []).map((item) => ({ ...item, id: fromBase64Url(item.id) })),
  };
}

export function requestOptions(publicKey) {
  return {
    ...publicKey,
    challenge: fromBase64Url(publicKey.challenge),
    allowCredentials: (publicKey.allowCredentials || []).map((item) => ({ ...item, id: fromBase64Url(item.id) })),
  };
}

export function registrationJSON(credential) {
  return {
    id: credential.id,
    rawId: toBase64Url(credential.rawId),
    type: credential.type,
    response: {
      attestationObject: toBase64Url(credential.response.attestationObject),
      clientDataJSON: toBase64Url(credential.response.clientDataJSON),
      transports: credential.response.getTransports?.() || undefined,
    },
    extensions: credential.getClientExtensionResults?.() || {},
  };
}

export function assertionJSON(credential) {
  const { response } = credential;
  return {
    id: credential.id,
    rawId: toBase64Url(credential.rawId),
    type: credential.type,
    response: {
      authenticatorData: toBase64Url(response.authenticatorData),
      clientDataJSON: toBase64Url(response.clientDataJSON),
      signature: toBase64Url(response.signature),
      userHandle: response.userHandle ? toBase64Url(response.userHandle) : null,
    },
    extensions: credential.getClientExtensionResults?.() || {},
  };
}

/** Create a passkey for the signed-in account. */
export async function addPasskey(name = describeDevice()) {
  const { id, options } = await call(`${API}/passkey/register/start`, { method: "POST", body: {} });
  const credential = await navigator.credentials.create({ publicKey: creationOptions(options.publicKey) });
  return call(`${API}/passkey/register/finish`, { method: "POST", body: { id, credential: registrationJSON(credential), name } });
}

/** Sign in with any passkey saved for this site (no username needed). */
export async function passkeyLogin() {
  const { id, options } = await call(`${API}/passkey/login/start`, { method: "POST", body: {} });
  const credential = await navigator.credentials.get({ publicKey: requestOptions(options.publicKey) });
  return call(`${API}/passkey/login/finish`, { method: "POST", body: { id, credential: assertionJSON(credential), ...device() } });
}
