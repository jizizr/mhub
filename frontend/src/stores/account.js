import { defineStore } from "pinia";
import {
  deviceIdentity, getAuthConfig, getMe, logout as logoutRequest, readSyncMeta, writeSyncMeta,
} from "../services/account.js";
import { usePlaylistStore } from "./playlists.js";
import { useQueueStore, trackIdentity } from "./queue.js";
import { useStatusStore } from "./status.js";

// The Yjs engine is a separate chunk: guests never download it.
const loadSync = () => Promise.all([
  import("../services/sync/engine.js"),
  import("../services/sync/library.js"),
  import("../services/sync/merge.js"),
]).then(([engine, library, merge]) => ({ engine, library, merge }));

const FIRST_SYNC_TIMEOUT_MS = 8000;
const PLAYBACK_DEBOUNCE_MS = 2000;
const HANDOFF_QUEUE_LIMIT = 200;
const HANDOFF_FLUSH_QUEUE_LIMIT = 120;

let replica = null;
let modules = null;
let refreshQueued = false;
let playbackTimer = 0;

const makeId = () => globalThis.crypto?.randomUUID?.() || `playlist-${Date.now()}-${Math.random().toString(16).slice(2)}`;

export const useAccountStore = defineStore("account", {
  state: () => ({
    enabled: false,
    ready: false,
    turnstileSiteKey: "",
    user: null,
    /** guest | linked | expired (linked replica, session gone). */
    mode: "guest",
    /** loading | syncing | synced | offline | expired | error */
    sync: "idle",
    syncMessage: "",
    dialogOpen: false,
    dialogView: "login",
    pendingMerge: null,
    handoffs: {},
  }),
  getters: {
    deviceId: () => deviceIdentity().id,
    otherDevices(state) {
      const own = deviceIdentity().id;
      return Object.values(state.handoffs)
        .filter((session) => session.device_id !== own && session.queue?.length)
        .sort((left, right) => (right.updated_at || 0) - (left.updated_at || 0));
    },
  },
  actions: {
    /** Page became visible again: reconnect and refresh hand-off. */
    resume() {
      if (this.mode !== "linked") return;
      replica?.resyncNow();
      void this.fetchHandoffs();
    },

    openDialog(view) {
      this.dialogView = view || (this.mode === "linked" ? "account" : "login");
      this.dialogOpen = true;
    },

    async init() {
      const config = await getAuthConfig();
      this.enabled = Boolean(config.enabled);
      this.turnstileSiteKey = config.turnstile_site_key || "";
      const meta = readSyncMeta();
      // Offline first: show the linked replica before the network answers.
      if (meta) {
        this.mode = "expired";
        this.user = { id: meta.accountId, username: meta.username };
        await this.openReplica(meta.accountId).catch((error) => console.warn("[sync] replica unavailable", error));
      }
      if (!this.enabled) { this.ready = true; return; }
      try {
        const { user } = await getMe();
        if (user) await this.completeLogin(user, { silent: true });
        else if (meta) this.expire();
      } catch (error) {
        // Offline: keep the replica; reconnecting happens on the next visit.
        if (meta) this.sync = "offline";
        console.warn("[account] session check failed", error);
      } finally {
        this.ready = true;
      }
    },

    async openReplica(accountId) {
      modules ||= await loadSync();
      replica = modules.engine.createReplica({
        accountId,
        onState: (state, message = "") => {
          this.sync = state;
          this.syncMessage = message;
          if (state === "expired") this.expire();
        },
        onChange: () => this.queueRefresh(),
        onPlayback: (session) => { this.handoffs = { ...this.handoffs, [session.device_id]: session }; },
      });
      await replica.loaded;
      usePlaylistStore().attachCloud(replica.doc, modules.library);
    },

    queueRefresh() {
      if (refreshQueued) return;
      refreshQueued = true;
      queueMicrotask(() => {
        refreshQueued = false;
        usePlaylistStore().refreshCloud();
      });
    },

    /** Called after any successful sign-in, and at start-up with a valid session. */
    async completeLogin(user, { silent = false } = {}) {
      const status = useStatusStore();
      const meta = readSyncMeta();
      if (meta && meta.accountId !== user.id) {
        // Scenario B: another account's replica. Keep it stored for that
        // account (it is keyed by account id) and switch to this one.
        await this.closeReplica({ clearLocal: false });
        status.toast(`已切换到 ${user.username}，${meta.username} 的离线资料仍保留在此设备`);
      }
      this.user = { id: user.id, username: user.username, has_password: user.has_password, passkeys: user.passkeys };
      this.mode = "linked";
      const sameAccount = meta?.accountId === user.id;
      writeSyncMeta({ accountId: user.id, username: user.username, linkedAt: meta?.linkedAt || Date.now() });
      if (!replica) await this.openReplica(user.id);
      replica.start();
      void this.fetchHandoffs();
      if (sameAccount) {
        // Scenario A: same account — the CRDT merges offline edits by itself.
        if (!silent) status.toast("已重新登录，本设备的离线修改正在合并");
        return;
      }
      // Scenario C: guest data. Wait for the cloud state before comparing.
      await Promise.race([replica.synced, new Promise((resolve) => window.setTimeout(resolve, FIRST_SYNC_TIMEOUT_MS))]);
      const playlists = usePlaylistStore();
      const plan = modules.merge.planGuestMerge(playlists.guestData(), modules.library.readLibrary(replica.doc), user.id);
      if (modules.merge.planIsEmpty(plan)) {
        playlists.markGuestImported(plan.importedIds, user.id);
        if (!silent) status.toast(`欢迎，${user.username}`);
        return;
      }
      this.pendingMerge = plan;
      this.openDialog("merge");
    },

    /** Merge guest data into the account (or keep it local only). */
    resolveMerge(accept) {
      const plan = this.pendingMerge;
      this.pendingMerge = null;
      if (!plan || !replica) return;
      const playlists = usePlaylistStore();
      if (accept) {
        modules.library.applyMergePlan(replica.doc, plan, makeId);
        playlists.refreshCloud();
        useStatusStore().toast(`已合并：新增 ${plan.counts.created} 个歌单，${plan.counts.tracks} 首歌曲`);
      }
      // Either way, these guest playlists are not offered again.
      playlists.markGuestImported(plan.importedIds, this.user.id);
      this.dialogOpen = false;
    },

    expire() {
      if (this.mode === "guest") return;
      const wasLinked = this.mode === "linked";
      this.mode = "expired";
      this.sync = "expired";
      if (wasLinked) useStatusStore().toast("登录已过期：修改会保存在本设备，重新登录后自动合并");
    },

    async closeReplica({ clearLocal }) {
      const current = replica;
      replica = null;
      usePlaylistStore().detachCloud();
      if (current) await current.destroy({ clearLocal });
    },

    async logout({ clearLocal = true } = {}) {
      try { await logoutRequest(); } catch { /* The local sign-out still applies. */ }
      await this.closeReplica({ clearLocal });
      writeSyncMeta(null);
      this.user = null;
      this.mode = "guest";
      this.sync = "idle";
      this.handoffs = {};
      this.dialogOpen = false;
      useStatusStore().toast(clearLocal ? "已退出登录，并清除了此设备上的云端歌单副本" : "已退出登录");
    },

    // --- Playback hand-off ------------------------------------------------

    async fetchHandoffs() {
      if (this.mode !== "linked") return;
      try {
        const response = await fetch("/api/v1/sync/playback", { credentials: "same-origin", cache: "no-store" });
        if (response.status === 401) { this.expire(); return; }
        if (!response.ok) return;
        const { sessions } = await response.json();
        this.handoffs = Object.fromEntries((sessions || []).map((session) => [session.device_id, session]));
      } catch { /* Hand-off is optional. */ }
    },

    playbackSnapshot(limit = HANDOFF_QUEUE_LIMIT) {
      const queue = useQueueStore();
      const status = useStatusStore();
      const index = status.playIndex;
      if (index < 0 || !queue.queueLength) return null;
      // Keep the window around the current song when the queue is long.
      const start = Math.max(0, Math.min(index - 20, queue.queueLength - limit));
      const tracks = queue.queue.slice(start, start + limit).map((track) => ({
        key: trackIdentity(track),
        platform: track.platform,
        source_id: track.source_id || track.selected_id || "",
        title: track.title,
        artist: track.artist,
        cover: track.cover || "",
      }));
      return {
        queue: tracks,
        play_index: index - start,
        position_ms: Math.round(status.position || 0),
        is_playing: status.isPlaying,
        shuffle: status.shuffleMode === "on",
        repeat: status.repeatMode,
        source_playlist: queue.sourcePlaylist?.name || "",
      };
    },

    /** Debounced report of this device's queue; `flush` sends now (page hide). */
    publishPlayback({ flush = false } = {}) {
      if (this.mode !== "linked") return;
      window.clearTimeout(playbackTimer);
      const send = () => {
        // A keepalive body is size-limited. Shrink the window itself so
        // play_index still points at the current song.
        const snapshot = this.playbackSnapshot(flush ? HANDOFF_FLUSH_QUEUE_LIMIT : HANDOFF_QUEUE_LIMIT);
        if (!snapshot) return;
        void fetch(`/api/v1/sync/playback/${encodeURIComponent(deviceIdentity().id)}`, {
          method: "PUT",
          credentials: "same-origin",
          keepalive: flush,
          headers: { "content-type": "application/json", "x-mhub-client": "1" },
          body: JSON.stringify(snapshot),
        }).then((response) => { if (response.status === 401) this.expire(); }).catch(() => {});
      };
      if (flush) send();
      else playbackTimer = window.setTimeout(send, PLAYBACK_DEBOUNCE_MS);
    },
  },
});
