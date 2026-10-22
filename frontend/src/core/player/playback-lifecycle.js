/**
 * Recover an interrupted audio context without turning a user pause into play.
 * The owner supplies playback intent separately from the rendered/UI state.
 * Lifecycle events are hints to retry once; no timers or keepalive audio run.
 */
export function createPlaybackLifecycle({ getSnapshot, onInterrupted, onRecovered, resume } = {}) {
  const page = typeof document === "undefined" ? null : document;
  const browser = typeof window === "undefined" ? null : window;
  let session = null;
  try { session = typeof navigator === "undefined" ? null : navigator.audioSession; } catch { /* Optional browser capability. */ }
  let disposed = false;
  let context = null;
  let token;
  let generation = 0;
  let wanted = false;
  let observedState;
  let interrupted = false;
  let flight = null;

  function snapshot() {
    try { return getSnapshot?.() || {}; } catch { return {}; }
  }

  function notify(callback) {
    try { void Promise.resolve(callback?.()).catch(() => {}); } catch { /* Recovery must not break lifecycle dispatch. */ }
  }

  function isCurrent(expectedContext, expectedToken, expectedGeneration) {
    const latest = snapshot();
    return !disposed && generation === expectedGeneration && context === expectedContext &&
      latest.context === expectedContext && latest.token === expectedToken && Boolean(latest.wantsPlay);
  }

  function recover(allowAttempt) {
    if (disposed) return;
    const latest = snapshot();
    if ((latest.context || null) !== context || latest.token !== token || Boolean(latest.wantsPlay) !== wanted) {
      update();
      return;
    }
    if (!context || !wanted || context.state === "closed") return;
    const state = context.state;
    const stateChanged = state !== observedState;
    observedState = state;
    // Some WebKit versions report the session interruption before changing
    // the context state. Keep the rendered clock stopped for that interval.
    if (session?.state === "interrupted") {
      if (!interrupted) { interrupted = true; notify(onInterrupted); }
      return;
    }
    if (state === "running") {
      if (interrupted) {
        interrupted = false;
        notify(onRecovered);
      }
      return;
    }
    if (state !== "suspended" && state !== "interrupted") return;
    if (!interrupted) {
      interrupted = true;
      notify(onInterrupted);
    }
    // A phone call or another app owns the audio session. Do not fight it.
    if ((!allowAttempt && !stateChanged) || flight) return;
    const expectedContext = context, expectedToken = token, expectedGeneration = generation;
    if (!isCurrent(expectedContext, expectedToken, expectedGeneration)) return;
    const pending = {};
    flight = pending;
    let request;
    try { request = resume ? resume() : context.resume?.(); }
    catch { request = Promise.reject(new Error("Audio context resume failed")); }
    Promise.resolve(request).then(() => {
      if (isCurrent(expectedContext, expectedToken, expectedGeneration)) recover(false);
    }).catch(() => {
      // A rejected autoplay/OS request needs a later gesture or lifecycle
      // event. An immediate retry loop would fight the browser's policy.
    }).finally(() => { if (flight === pending) flight = null; });
  }

  function onContextStateChange() { recover(false); }
  function onLifecycle() { recover(true); }

  function update() {
    if (disposed) return;
    const latest = snapshot();
    const nextContext = latest.context || null;
    const nextWanted = Boolean(latest.wantsPlay);
    const changed = nextContext !== context || latest.token !== token;
    const intentChanged = wanted !== nextWanted;
    if (changed) {
      context?.removeEventListener?.("statechange", onContextStateChange);
      // Tracks can share one AudioContext. Its pending resume remains one
      // operation even if the owner advances to a new track in the meantime.
      if (nextContext !== context) flight = null;
      context = nextContext;
      token = latest.token;
      generation += 1;
      observedState = undefined;
      interrupted = false;
      context?.addEventListener?.("statechange", onContextStateChange);
    }
    wanted = nextWanted;
    if (!wanted) {
      // Invalidate callbacks already queued by a resume promise. A later
      // explicit play starts a fresh recovery attempt for the same context.
      if (intentChanged) generation += 1;
      interrupted = false;
      return;
    }
    recover(changed || intentChanged);
  }

  page?.addEventListener?.("visibilitychange", onLifecycle);
  browser?.addEventListener?.("pageshow", onLifecycle);
  // Page Lifecycle `resume` is optional (Chromium); visibility/pageshow
  // cover browsers that do not expose it.
  page?.addEventListener?.("resume", onLifecycle);
  session?.addEventListener?.("statechange", onLifecycle);

  return {
    update,
    dispose() {
      if (disposed) return;
      disposed = true;
      generation += 1;
      context?.removeEventListener?.("statechange", onContextStateChange);
      page?.removeEventListener?.("visibilitychange", onLifecycle);
      browser?.removeEventListener?.("pageshow", onLifecycle);
      page?.removeEventListener?.("resume", onLifecycle);
      session?.removeEventListener?.("statechange", onLifecycle);
      context = null;
      flight = null;
    },
  };
}
