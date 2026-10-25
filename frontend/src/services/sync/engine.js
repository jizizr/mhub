// Offline-first replica of the account library.
//
// The Y.Doc lives in IndexedDB, so edits made while offline or after the
// session expired are kept and merged by the CRDT on the next connection.
// WebSocket (y-sync protocol) carries live updates; when it cannot connect,
// an HTTP round trip exchanges state vectors instead.
import * as Y from "yjs";
import { IndexeddbPersistence } from "y-indexeddb";
import * as syncProtocol from "y-protocols/sync";
import * as encoding from "lib0/encoding";
import * as decoding from "lib0/decoding";
import { LOCAL, removeDuplicates } from "./library.js";

const MESSAGE_SYNC = 0;
const REMOTE = Symbol("remote");
const HTTP_INTERVAL_MS = 30_000;
const MAX_BACKOFF_MS = 30_000;

const toBase64 = (bytes) => {
  let binary = "";
  for (let index = 0; index < bytes.length; index += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
  }
  return btoa(binary);
};
const fromBase64 = (value) => Uint8Array.from(atob(value), (char) => char.charCodeAt(0));

export function libraryDatabaseName(accountId) {
  return `mhub-library-${accountId}`;
}

/**
 * @param options.accountId     owner of this replica
 * @param options.onState       ("loading"|"syncing"|"synced"|"offline"|"expired"|"error", message?)
 * @param options.onPlayback    another device's playback session
 * @param options.onChange      the library changed (local or remote)
 */
export function createReplica({ accountId, onState = () => {}, onPlayback = () => {}, onChange = () => {} }) {
  const doc = new Y.Doc();
  const persistence = new IndexeddbPersistence(libraryDatabaseName(accountId), doc);
  let socket = null;
  let stopped = false;
  let online = false;
  let retry = 0;
  let retryTimer = 0;
  let httpTimer = 0;
  let serverStateVector = null;
  let resolveSynced;
  const synced = new Promise((resolve) => { resolveSynced = resolve; });
  const loaded = persistence.whenSynced.then(() => onChange());

  const markSynced = () => {
    online = true;
    retry = 0;
    resolveSynced();
    // Concurrent additions of one song on two devices: tidy after merging.
    if (removeDuplicates(doc)) onChange();
    onState("synced");
  };

  const send = (encoder) => {
    if (socket?.readyState === WebSocket.OPEN) socket.send(encoding.toUint8Array(encoder));
  };

  doc.on("update", (update, origin) => {
    onChange();
    if (origin === REMOTE || stopped) return;
    if (socket?.readyState === WebSocket.OPEN) {
      const encoder = encoding.createEncoder();
      encoding.writeVarUint(encoder, MESSAGE_SYNC);
      syncProtocol.writeUpdate(encoder, update);
      send(encoder);
    } else {
      scheduleHttp(1500);
    }
  });

  function handleBinary(data) {
    const decoder = decoding.createDecoder(new Uint8Array(data));
    const encoder = encoding.createEncoder();
    if (decoding.readVarUint(decoder) !== MESSAGE_SYNC) return;
    encoding.writeVarUint(encoder, MESSAGE_SYNC);
    const type = syncProtocol.readSyncMessage(decoder, encoder, doc, REMOTE);
    if (encoding.length(encoder) > 1) send(encoder);
    if (type === syncProtocol.messageYjsSyncStep2) markSynced();
  }

  function handleText(text) {
    let message;
    try { message = JSON.parse(text); } catch { return; }
    if (message.type === "playback" && message.session) onPlayback(message.session);
    else if (message.type === "session-expired") expire();
    else if (message.type === "error") onState("error", message.message);
  }

  function connect() {
    if (stopped || socket) return;
    onState(online ? "syncing" : "loading");
    const url = new URL("/api/v1/sync/ws", window.location.href);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    const current = new WebSocket(url);
    current.binaryType = "arraybuffer";
    socket = current;
    let opened = false;
    current.onopen = () => {
      opened = true;
      const encoder = encoding.createEncoder();
      encoding.writeVarUint(encoder, MESSAGE_SYNC);
      syncProtocol.writeSyncStep1(encoder, doc);
      send(encoder);
    };
    current.onmessage = (event) => {
      if (typeof event.data === "string") handleText(event.data);
      else handleBinary(event.data);
    };
    current.onclose = () => {
      if (socket === current) socket = null;
      if (stopped) return;
      online = false;
      onState("offline");
      // A refused upgrade may be an expired session or a proxy without
      // WebSocket support: an HTTP sync distinguishes both.
      if (!opened) void httpSync();
      const delay = Math.min(MAX_BACKOFF_MS, 1000 * 2 ** retry) * (0.75 + Math.random() / 2);
      retry += 1;
      retryTimer = window.setTimeout(connect, delay);
    };
  }

  async function httpSync() {
    window.clearTimeout(httpTimer);
    httpTimer = 0;
    if (stopped) return;
    try {
      const update = Y.encodeStateAsUpdate(doc, serverStateVector || undefined);
      const response = await fetch("/api/v1/sync/library", {
        method: "POST",
        credentials: "same-origin",
        headers: { "content-type": "application/json", "x-mhub-client": "1" },
        body: JSON.stringify({ state_vector: toBase64(Y.encodeStateVector(doc)), update: toBase64(update) }),
      });
      if (response.status === 401) { expire(); return; }
      if (!response.ok) throw new Error(`同步失败（${response.status}）`);
      const result = await response.json();
      Y.applyUpdate(doc, fromBase64(result.update), REMOTE);
      serverStateVector = fromBase64(result.state_vector);
      if (!socket || socket.readyState !== WebSocket.OPEN) {
        markSynced();
        scheduleHttp(HTTP_INTERVAL_MS);
      }
    } catch (error) {
      if (!stopped) onState("offline", error?.message);
    }
  }

  function scheduleHttp(delay) {
    if (stopped || socket?.readyState === WebSocket.OPEN || (httpTimer && delay > 2000)) return;
    window.clearTimeout(httpTimer);
    httpTimer = window.setTimeout(() => void httpSync(), delay);
  }

  function expire() {
    // Keep the replica and keep accepting local edits; they merge after the
    // user signs in to the same account again.
    stop();
    onState("expired");
  }

  function stop() {
    stopped = true;
    window.clearTimeout(retryTimer);
    window.clearTimeout(httpTimer);
    const current = socket;
    socket = null;
    current?.close();
  }

  return {
    doc,
    loaded,
    synced,
    /** Start (or restart after a new sign-in) network sync. */
    start() {
      stopped = false;
      retry = 0;
      void loaded.then(connect);
    },
    stop,
    resyncNow() {
      if (socket?.readyState === WebSocket.OPEN) return;
      window.clearTimeout(retryTimer);
      socket = null;
      connect();
    },
    async destroy({ clearLocal = false } = {}) {
      stop();
      if (clearLocal) await persistence.clearData();
      else await persistence.destroy();
      doc.destroy();
    },
    LOCAL,
  };
}
