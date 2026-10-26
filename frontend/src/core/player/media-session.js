// Media Session supplies the browser's media controls; it does not use the
// Notification API and does not request notification permission. Each optional
// browser operation is isolated so system controls can never break playback.
export function createMediaSessionController({
  readState = () => ({}),
  actions = {},
  navigatorObject = globalThis.navigator,
  Metadata = globalThis.MediaMetadata || globalThis.window?.MediaMetadata,
  baseUrl = globalThis.location?.href || globalThis.window?.location?.href,
  now = () => globalThis.performance?.now?.() ?? Date.now(),
  positionInterval = 1000,
} = {}) {
  let session;
  try { session = navigatorObject?.mediaSession; } catch { /* unavailable */ }
  let registered = false;
  let disposed = false;
  let metadataKey = null;
  let trackKey = null;
  let declaredState = null;
  let lastPosition = null;
  let positionCleared = false;
  const installedActions = new Set();

  function state() {
    try { return readState() || {}; } catch { return {}; }
  }

  function safeCall(callback, ...args) {
    if (disposed || !registered) return;
    try {
      const result = callback?.(...args);
      // A lock-screen play can be rejected by browser autoplay policy. The
      // player owns that error UI; never leave a rejected event promise here.
      if (result?.then) void Promise.resolve(result).catch(() => {});
    } catch { /* the player remains available to its normal UI */ }
  }

  function clampSeek(value) {
    const current = state();
    const duration = Number(current.duration);
    const position = Math.max(0, value);
    return Number.isFinite(duration) && duration > 0 ? Math.min(position, duration) : position;
  }

  function seekRelative(direction, details = {}) {
    const offset = Number(details.seekOffset);
    const seconds = Number.isFinite(offset) && offset > 0 ? offset : 10;
    const rawPosition = Number(state().position);
    const position = Number.isFinite(rawPosition) ? rawPosition : 0;
    safeCall(actions.seek, clampSeek(position + direction * seconds * 1000));
    syncPosition({ force: true });
  }

  function register() {
    if (disposed || registered || typeof session?.setActionHandler !== "function") return;
    const handlers = {
      play: actions.play && (() => safeCall(actions.play)),
      pause: actions.pause && (() => safeCall(actions.pause)),
      previoustrack: actions.previous && (() => safeCall(actions.previous)),
      nexttrack: actions.next && (() => safeCall(actions.next)),
      seekbackward: actions.seek && ((details) => seekRelative(-1, details)),
      seekforward: actions.seek && ((details) => seekRelative(1, details)),
      seekto: actions.seek && ((details = {}) => {
        // Missing/invalid positions are not a request to restart the song.
        if (!Number.isFinite(details.seekTime)) return;
        safeCall(actions.seek, clampSeek(details.seekTime * 1000));
        syncPosition({ force: true });
      }),
      stop: actions.stop && (() => {
        safeCall(actions.stop);
        clear();
      }),
    };
    for (const [action, handler] of Object.entries(handlers)) {
      if (!handler) continue;
      try {
        session.setActionHandler(action, handler);
        installedActions.add(action);
      } catch { /* unsupported actions must not hide supported controls */ }
    }
    registered = true;
  }

  function artworkUrl(value) {
    if (typeof value !== "string" || !value.trim()) return "";
    try {
      const url = new URL(value, baseUrl);
      if (["http:", "https:", "blob:"].includes(url.protocol) || (url.protocol === "data:" && /^data:image\//i.test(value))) {
        return url.href;
      }
    } catch { /* fall through to another available cover */ }
    return "";
  }

  function setMetadata(track) {
    if (typeof Metadata !== "function") return;
    const cover = artworkUrl(track.cover) || artworkUrl(track.cover_url);
    const value = {
      title: String(track.title || track.song || "未知歌曲"),
      artist: String(track.artist || track.singer || "未知艺人"),
      album: String(track.album || ""),
      // The URL can be JPEG, WebP, PNG, or a cached Blob of any size. False
      // PNG/size declarations prevent some mobile browsers showing covers.
      artwork: cover ? [{ src: cover }] : [],
    };
    const key = JSON.stringify(value);
    if (key === metadataKey) return;
    try {
      session.metadata = new Metadata(value);
      metadataKey = key;
    } catch {
      // A browser rejecting the artwork can still display title and artist.
      try {
        session.metadata = new Metadata({ ...value, artwork: [] });
        metadataKey = key;
      } catch { /* metadata support is independent of transport controls */ }
    }
  }

  function playbackState(current) {
    if (current.state === "stopped" || current.state === "ended" || current.state === "error") return "none";
    // Buffering is still the user's active playback session. Declaring it
    // "none" here used to drop the pause control during cold starts/seeks.
    if (current.wantsPlay !== false && (current.state === "playing" || current.state === "buffering" || current.wantsPlay === true)) return "playing";
    return "paused";
  }

  function syncPlaybackState(current) {
    const nextState = playbackState(current);
    let observedState;
    try { observedState = session.playbackState; } catch { /* Some implementations expose only a setter. */ }
    const changed = nextState !== declaredState ||
      (["none", "paused", "playing"].includes(observedState) && observedState !== nextState);
    if (changed) {
      // WebKit can reset this after a detached native element delivers its
      // queued pause event while AudioWorklet is already producing music.
      // Reconcile actual browser state on real position updates, not a timer.
      try { session.playbackState = nextState; declaredState = nextState; } catch { /* optional */ }
    }
    return changed;
  }

  function clearPosition() {
    lastPosition = null;
    if (positionCleared || typeof session?.setPositionState !== "function") return;
    try {
      session.setPositionState();
      positionCleared = true;
    } catch { /* older Media Session implementations may not support reset */ }
  }

  function syncPosition({ force = false } = {}) {
    if (disposed || !registered || !session) return;
    const current = state();
    if (current.track) force = syncPlaybackState(current) || force;
    if (typeof session.setPositionState !== "function") return;
    const duration = Number(current.duration) / 1000;
    if (!current.track || !Number.isFinite(duration) || duration <= 0) {
      clearPosition();
      return;
    }
    const rawPosition = Number(current.position) / 1000;
    const rawRate = Number(current.speed);
    const value = {
      duration,
      playbackRate: Number.isFinite(rawRate) && rawRate > 0 ? rawRate : 1,
      position: Number.isFinite(rawPosition) ? Math.min(Math.max(rawPosition, 0), duration) : 0,
    };
    const timestamp = now();
    const mode = playbackState(current);
    if (!force && lastPosition && lastPosition.mode === mode && value.duration === lastPosition.duration && value.playbackRate === lastPosition.playbackRate) {
      const elapsed = Math.max(0, timestamp - lastPosition.timestamp) / 1000;
      const expected = lastPosition.position + (mode === "playing" ? elapsed * value.playbackRate : 0);
      // Normal time updates are throttled. Seeks, rate/duration changes and
      // pause/resume must reach the OS immediately, even within this window.
      if (timestamp - lastPosition.timestamp < positionInterval && Math.abs(value.position - expected) < 1) return;
    }
    try {
      session.setPositionState(value);
      lastPosition = { ...value, timestamp, mode };
      positionCleared = false;
    } catch { /* a rejected system progress update is never a playback error */ }
  }

  function update(track = state().track) {
    if (disposed || !session) return;
    const current = state();
    if (!track || current.state === "stopped") { clear(); return; }
    register();
    const key = String(track.id || track.playback_id || `${track.platform || ""}:${track.source_id || track.title || ""}:${track.artist || ""}`);
    const changedTrack = key !== trackKey;
    if (changedTrack) {
      trackKey = key;
      clearPosition();
    }
    setMetadata(track);
    const changedState = syncPlaybackState(current);
    syncPosition({ force: changedTrack || changedState });
  }

  function clear() {
    metadataKey = null;
    trackKey = null;
    if (!session) return;
    try { session.metadata = null; } catch { /* optional */ }
    try { session.playbackState = "none"; declaredState = "none"; } catch { /* optional */ }
    clearPosition();
    for (const action of installedActions) {
      try { session.setActionHandler(action, null); } catch { /* optional */ }
    }
    installedActions.clear();
    registered = false;
  }

  function activate(track = state().track) {
    // A new native player can acquire system audio ownership while the JS
    // getters still expose the previous player's metadata/playing state.
    // Republish on real rendering events, without clearing to "none" or
    // unregistering controls in between. Ordinary time updates stay deduped.
    registered = false;
    metadataKey = null;
    declaredState = null;
    update(track);
  }

  return {
    register,
    update,
    activate,
    syncPosition,
    clear,
    dispose() { clear(); disposed = true; },
  };
}
