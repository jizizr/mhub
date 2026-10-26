import { useMediaStore } from "../../stores/media";
import { trackIdentity, useQueueStore } from "../../stores/queue";
import { useStatusStore } from "../../stores/status";
import * as playback from "../../services/playback";
import { trackFromSearch } from "../../utils/track";

let audio = null;
let resolveTrackSource = null;
let waitForCompletedSource = null;
let trustedMediaOrigins = [];
let prefetchTrackSource = null;
let cacheActiveTrackSource = null;
let loadLyricsForTrack = null;
let getContinuationRecommendations = null;
let resolveCachedTrackSource = null;
let outputFactory = null;
let lookupLoudness = null;
let warmTrackLoudness = null;
let loudnessController = null;
const nativeOutputs = new WeakMap();
let spareDeck = null;
let transitionCandidate = null;
let transitionPreparing = null;
let transitionStarting = null;
let transitionGeneration = 0;
let transitionFailedKey = "";
let retiringDeck = null;
let activeLoudnessGain = 1;
let alacPlayerFactory = null;
let alacPlayer = null;
let usingAlacWorklet = false;
let loadToken = 0;
let playControlToken = 0;
let trackToken = 0;
let seekTarget = null;
let pendingSeek = null;
let seekGeneration = 0;
let loadWantsPlay = false;
let lyricGeneration = 0;
let initialized = false;
let systemMedia = null;
let playbackLifecycle = null;
let recoveringAudio = false;
let recoveryReady = true;
let recoveryPosition = 0;
let consecutiveFailures = 0;
let audioErrorTrackToken = -1;
const activePrefetches = new Map();
let prefetchRetryTimer = 0;
let prefetchRetryAttempt = 0;
let activeTrackCache = null;
let autoplayGeneration = 0;
let autoplayRequest = null;
let autoplayAdvance = null;
let autoplayFailedSeed = "";
// A cached next song whose Blob URL is resolved before the current one ends.
// Starting it from the `ended` task leaves no silent gap in which a phone OS
// may freeze the hidden page before the next source is attached.
let armedNext = null;
// Android hides the media notification as soon as the only player reaches
// EOS while the device is locked, and a background browser cannot show it
// again. The armed song therefore preloads on a second element and starts
// muted just before the current song ends, so a player always stays active.
let idleElement = null;
const preparedElements = new WeakSet();
const PREROLL_SECONDS = 1.2;
// One reload per song after a MEDIA_ERR_NETWORK before the queue advances.
let networkRetryKey = "";
const autoplayRecent = new Map();

const MAX_CONSECUTIVE_FAILURES = 5;
const SKIP_ON_ERROR_DELAY_MS = 1000;
const PREFETCH_START_DELAY_MS = 2500;
const PREFETCH_BUFFER_TARGET_SECONDS = 20;
const PREFETCH_RETRY_DELAYS_MS = [5000, 15000, 30000];
const PROGRESS_STORAGE_KEY = "music-web-splayer-progress-v1";

// Visible on every screen size; stores without toast keep the notice only.
function toast(message) {
  const status = useStatusStore();
  if (status.toast) status.toast(message);
  else status.setNotice(message);
}

function stores() {
  return {
    media: useMediaStore(),
    queue: useQueueStore(),
    status: useStatusStore(),
  };
}

// Effects never resolve a cold next-track source. The only extra transport
// reads a complete, validated local Blob after the existing cache gate.
function effectOutput(element = audio, create = false) {
  if (!element) return null;
  let output = nativeOutputs.get(element);
  if (!output && create && outputFactory?.createNative) {
    const source = element.currentSrc || element.src || "";
    try {
      if (source && !source.startsWith("blob:")) {
        const origin = new URL(source, window.location.origin).origin;
        if (origin !== window.location.origin && !trustedMediaOrigins.includes(origin)) return null;
      }
      output = outputFactory.createNative(element); nativeOutputs.set(element, output);
    }
    catch { return null; }
  }
  return output;
}

function currentEffectOutput() {
  return usingAlacWorklet ? alacPlayer : effectOutput(audio);
}

function cachedLoudnessGain(track) {
  if (!useStatusStore().soundCheckEnabled) return 1;
  const value = lookupLoudness?.(track);
  return value?.state === "ready" && Number.isFinite(value.gain_db) ? Math.pow(10, value.gain_db / 20) : 1;
}

function applyActiveLevels({ resetFade = false } = {}) {
  const status = useStatusStore();
  const output = usingAlacWorklet ? alacPlayer : effectOutput(audio, status.crossfadeEnabled || status.soundCheckEnabled);
  if (output) {
    if (!usingAlacWorklet && audio) audio.volume = 1;
    output.setVolume?.(status.volume);
    output.setLoudnessGain?.(activeLoudnessGain);
    if (resetFade) output.cancelFade?.(1);
  } else if (audio) audio.volume = status.volume;
}

function warmActiveLoudness(track, token = trackToken) {
  loudnessController?.abort();
  loudnessController = null;
  const status = useStatusStore();
  if (!status.soundCheckEnabled || !track || !warmTrackLoudness) { status.soundCheckState = "idle"; return; }
  const cached = lookupLoudness?.(track);
  status.soundCheckState = cached?.state === "ready" ? "ready" : "pending";
  if (cached?.state === "ready") return;
  const controller = new AbortController();
  loudnessController = controller;
  void warmTrackLoudness(track, { signal: controller.signal }).then((result) => {
    if (token !== trackToken || controller.signal.aborted || !status.soundCheckEnabled) return;
    // A full-song measurement is cached for the next play. Do not chase the
    // current music's volume or change its dynamics halfway through a song.
    status.soundCheckState = result?.state || "unavailable";
  }).catch((error) => {
    if (error?.name !== "AbortError" && token === trackToken) status.soundCheckState = "unavailable";
  });
}

function stopDeck(deck) {
  if (!deck) return;
  deck.controller?.abort();
  deck.loudnessController?.abort();
  deck.output?.cancelFade?.(0);
  void deck.alac?.stop?.();
  deck.audio?.pause();
  deck.audio?.removeAttribute("src");
  deck.audio?.load();
  if (deck.source?.startsWith("blob:")) URL.revokeObjectURL(deck.source);
  deck.source = "";
}

function finishRetiringDeck({ prepareNext = true } = {}) {
  if (!retiringDeck) return;
  window.clearTimeout(retiringDeck.timer);
  stopDeck(retiringDeck);
  spareDeck = retiringDeck;
  retiringDeck = null;
  useStatusStore().transitionState = "idle";
  // A short/fully cached next download can finish while both decks are busy.
  // Retry its cache-only preparation once the retiring deck becomes free.
  if (prepareNext) void prepareTransition();
}

function cancelTransition({ resetFailure = true } = {}) {
  stopPreroll();
  transitionGeneration += 1;
  if (transitionCandidate) stopDeck(transitionCandidate);
  transitionCandidate = null;
  transitionPreparing = null;
  transitionStarting = null;
  if (resetFailure) transitionFailedKey = "";
  finishRetiringDeck({ prepareNext: false });
  currentEffectOutput()?.cancelFade?.(1);
  useStatusStore().transitionState = "idle";
}

function refreshTransitionPreparation() {
  const status = useStatusStore();
  if (status.crossfadeEnabled && status.state === "playing" && !status.trackLoading && hasAudioSource()) {
    schedulePrefetchNextTrack(trackToken, { immediate: true });
  }
}

export function onQualityChanged() {
  cancelTransition();
  const status = useStatusStore();
  if (status.state === "playing" && !status.trackLoading && hasAudioSource()) {
    schedulePrefetchNextTrack(trackToken, { immediate: true });
  }
}

function nextTransitionTrack() {
  const { queue, status } = stores();
  if (!status.crossfadeEnabled || status.repeatMode === "one" || status.speed !== 1 || status.playIndex < 0) return null;
  // Preserve continuous albums and authored album transitions.
  if (queue.sourcePlaylist?.resource_type === "albums" && status.shuffleMode !== "on") return null;
  const index = nextQueueIndex();
  const track = index >= 0 ? normalizeTrackForQuality(queue.getTrack(index)) : null;
  return track && trackIdentity(track) !== trackIdentity(status.currentTrack) ? track : null;
}

// The decoder chunk is loaded on demand. Factories without `load` (tests and
// eager callers) create synchronously.
function createAlacPlayer() {
  return alacPlayerFactory.create?.() || alacPlayerFactory();
}

async function loadAlacPlayer() {
  await alacPlayerFactory.load?.();
  return createAlacPlayer();
}

function candidateStillValid(candidate) {
  return candidate && candidate.generation === transitionGeneration && candidate.token === trackToken &&
    prefetchKey(nextTransitionTrack()) === candidate.key && !candidate.controller.signal.aborted;
}

function nativeReady(element, signal) {
  if (element.readyState >= 3 && Number.isFinite(element.duration)) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const finish = (error) => {
      window.clearTimeout(timer);
      for (const name of ["canplay", "loadeddata", "error"]) element.removeEventListener(name, check);
      signal.removeEventListener("abort", aborted);
      error ? reject(error) : resolve();
    };
    const check = () => element.error ? finish(new Error("候选音频不可用")) : element.readyState >= 3 && finish();
    const aborted = () => finish(new DOMException("过渡已取消", "AbortError"));
    const timer = window.setTimeout(() => finish(new Error("候选音频准备超时")), 12000);
    for (const name of ["canplay", "loadeddata", "error"]) element.addEventListener(name, check);
    signal.addEventListener("abort", aborted, { once: true });
    if (signal.aborted) aborted(); else check();
  });
}

function prepareAlacCandidate(candidate) {
  // Loading a cached Blob can resolve its startup PCM before the reader has
  // reported EOF. Wait for the real packet duration rather than trusting old
  // task metadata (which commonly still contains duration=0).
  let settle;
  const completed = new Promise((resolve, reject) => {
    let finished = false;
    const finish = (error, duration) => {
      if (finished) return;
      finished = true;
      window.clearTimeout(timer);
      candidate.controller.signal.removeEventListener("abort", abort);
      error ? reject(error) : resolve(duration);
    };
    settle = finish;
    const abort = () => finish(new DOMException("过渡已取消", "AbortError"));
    const timer = window.setTimeout(() => finish(new Error("候选音频准备超时")), 12000);
    candidate.controller.signal.addEventListener("abort", abort, { once: true });
    candidate.alac.onError = (error) => { candidate.failed = true; finish(error || new Error("候选音频不可用")); };
    candidate.alac.onStreamComplete = ({ duration } = {}) => finish(null, duration);
    if (candidate.controller.signal.aborted) abort();
  });
  const loaded = Promise.resolve().then(() => candidate.alac.load(candidate.source, {
    signal: candidate.controller.signal, completed: true, trustedMediaOrigins,
  }));
  return Promise.all([loaded, completed]).then(([metadata, duration]) => duration || metadata?.duration || candidate.alac.duration)
    .finally(() => settle(new DOMException("候选准备已结束", "AbortError")));
}

function startNativeCandidate(candidate) {
  return new Promise((resolve, reject) => {
    let rendered = false;
    let permitted = false;
    let finished = false;
    const element = candidate.audio;
    const finish = (error) => {
      if (finished || (!error && (!rendered || !permitted))) return;
      finished = true;
      window.clearTimeout(timer);
      element.removeEventListener("playing", playing);
      element.removeEventListener("error", failed);
      candidate.controller.signal.removeEventListener("abort", abort);
      error ? reject(error) : resolve();
    };
    const playing = () => { rendered = true; finish(); };
    const failed = () => finish(new Error("候选音频无法播放"));
    const abort = () => finish(new DOMException("过渡已取消", "AbortError"));
    const timer = window.setTimeout(() => finish(new Error("候选未开始播放")), 1500);
    element.addEventListener("playing", playing);
    element.addEventListener("error", failed);
    candidate.controller.signal.addEventListener("abort", abort, { once: true });
    if (candidate.controller.signal.aborted) { abort(); return; }
    // A previously connected native element may belong to another shared
    // sample-rate context. Its media clock alone cannot prove audible output.
    Promise.resolve().then(() => candidate.output.context?.resume?.()).then(() => {
      if (candidate.controller.signal.aborted) throw new DOMException("过渡已取消", "AbortError");
      return element.play();
    })
      .then(() => { permitted = true; finish(); }, finish);
  });
}

async function prepareTransition(myToken = trackToken) {
  const next = nextTransitionTrack();
  const status = useStatusStore();
  if (!next || !resolveCachedTrackSource || status.state !== "playing" || retiringDeck || transitionStarting || myToken !== trackToken) return;
  const key = prefetchKey(next);
  if (transitionCandidate?.key === key || transitionPreparing?.key === key || transitionFailedKey === key) return;
  if (transitionCandidate || transitionPreparing) cancelTransition();
  const generation = transitionGeneration;
  const pending = { key, generation };
  transitionPreparing = pending;
  status.transitionState = "preparing";
  let candidate;
  try {
    if (!await ensureActiveTrackCache(myToken) || myToken !== trackToken || generation !== transitionGeneration) return;
    const resolved = await resolveCachedTrackSource(next);
    if (!resolved?.source) return;
    if (myToken !== trackToken || generation !== transitionGeneration || status.state !== "playing" || prefetchKey(nextTransitionTrack()) !== key) {
      if (resolved.source.startsWith("blob:")) URL.revokeObjectURL(resolved.source);
      return;
    }
    if (!spareDeck) {
      const element = document.createElement("audio");
      element.crossOrigin = "anonymous";
      element.preload = "auto";
      element.hidden = true;
      document.body.appendChild(element);
      attachAudioEvents(element);
      spareDeck = { audio: element, alac: null };
    }
    candidate = { ...spareDeck, key, generation, token: myToken, source: resolved.source,
      track: { ...next, ...resolved.track }, controller: new AbortController(), loudnessController: null, ready: false };
    transitionCandidate = candidate;
    candidate.usingAlac = candidate.track.streaming_mode === "wasm_alac" && Boolean(await alacPlayerFactory?.canHandle?.(candidate.track, candidate.source));
    if (!candidateStillValid(candidate)) throw new DOMException("过渡已取消", "AbortError");
    candidate.loudnessGain = cachedLoudnessGain(candidate.track);
    if (candidate.usingAlac) {
      candidate.alac ||= await loadAlacPlayer();
      // Reused decks may still remember the previous quality's context.
      // prepareAudio() already unlocked this shared rate in the user gesture.
      candidate.alac.prepare?.(["hires", "hi-res"].includes(candidate.track.quality) ? 192000 : 48000);
      candidate.output = candidate.alac;
      candidate.alac.setVolume?.(status.volume);
      candidate.alac.setLoudnessGain?.(candidate.loudnessGain);
      candidate.alac.cancelFade?.(0);
      candidate.alac.onState = () => {};
      candidate.alac.onEnded = () => {};
      candidate.alac.onTime = (position) => { candidate.position = position; };
      candidate.duration = await prepareAlacCandidate(candidate);
    } else {
      candidate.output = effectOutput(candidate.audio, true);
      if (!candidate.output) throw new Error("当前浏览器无法准备过渡");
      candidate.audio.volume = 1;
      candidate.audio.playbackRate = 1;
      candidate.output.setVolume(status.volume);
      candidate.output.setLoudnessGain(candidate.loudnessGain);
      candidate.output.cancelFade(0);
      candidate.audio.src = candidate.source;
      candidate.audio.load();
      await nativeReady(candidate.audio, candidate.controller.signal);
      candidate.duration = candidate.audio.duration * 1000;
    }
    if (!candidateStillValid(candidate) || candidate.failed) throw new DOMException("过渡已取消", "AbortError");
    if (!Number.isFinite(candidate.duration) || candidate.duration <= 0) throw new Error("候选音频时长不可用");
    candidate.ready = true;
    if (status.soundCheckEnabled && warmTrackLoudness) {
      candidate.loudnessController = new AbortController();
      void warmTrackLoudness(candidate.track, { signal: candidate.loudnessController.signal }).catch(() => {});
    }
    status.transitionState = "idle";
  } catch (error) {
    if (candidate && transitionCandidate === candidate) { stopDeck(candidate); transitionCandidate = null; }
    if (generation === transitionGeneration && error?.name !== "AbortError") transitionFailedKey = key;
  } finally {
    if (transitionPreparing === pending) { transitionPreparing = null; if (!retiringDeck) status.transitionState = "idle"; }
  }
}

async function startTransition(candidate, seconds) {
  if (!candidateStillValid(candidate) || transitionStarting || retiringDeck) return;
  transitionStarting = candidate;
  const status = useStatusStore();
  try {
    // Start the new transport muted; only confirmed rendering permits fading
    // out the old song. A rejected/stalled candidate never cuts the old tail.
    if (candidate.usingAlac) {
      await new Promise((resolve, reject) => {
        const finish = (error) => {
          window.clearTimeout(timer);
          candidate.controller.signal.removeEventListener("abort", abort);
          error ? reject(error) : resolve();
        };
        const abort = () => finish(new DOMException("过渡已取消", "AbortError"));
        const timer = window.setTimeout(() => finish(new Error("候选未开始播放")), 1500);
        candidate.alac.onState = (state) => { if (state === "playing") finish(); };
        candidate.controller.signal.addEventListener("abort", abort, { once: true });
        Promise.resolve(candidate.alac.play()).catch(finish);
      });
    } else await startNativeCandidate(candidate);
    if (!candidateStillValid(candidate) || status.state !== "playing" || candidate.failed ||
        (!candidate.usingAlac && (candidate.audio.paused || candidate.audio.readyState < 3))) throw new Error("过渡已失效");
    const remaining = usingAlacWorklet
      ? (status.duration - Math.max(status.position, playback.getCurrentTime?.() || 0)) / 1000
      : audio.duration - audio.currentTime;
    if (!(remaining > 0.05) || (usingAlacWorklet ? alacPlayer?.ended : audio.paused || audio.ended)) throw new Error("当前歌曲已结束");
    seconds = Math.max(0.05, Math.min(seconds, remaining));
    const old = { audio, alac: alacPlayer, usingAlac: usingAlacWorklet, output: currentEffectOutput(), source: status.currentSource, track: useMediaStore().track };
    if (!old.output) throw new Error("当前音频无法过渡");
    const { queue, media } = stores();
    const nextIndex = nextQueueIndex();
    clearPrefetchRetry();
    cancelActiveTrackCache();
    invalidateAutoplay();
    loudnessController?.abort();
    // The candidate transport controller is still owned by its active ALAC
    // load. Cancel only metadata polling and let the active-track analysis
    // lifecycle take over without aborting the playing decoder.
    candidate.loudnessController?.abort();
    candidate.loudnessController = null;
    loadToken += 1;
    trackToken += 1;
    disarmNext();
    playControlToken += 1;
    transitionCandidate = null;
    transitionPreparing = null;
    transitionStarting = null;
    transitionGeneration += 1;
    audio = candidate.audio;
    alacPlayer = candidate.alac;
    usingAlacWorklet = candidate.usingAlac;
    activeLoudnessGain = cachedLoudnessGain(candidate.track);
    candidate.output.setLoudnessGain(activeLoudnessGain);
    status.playIndex = nextIndex;
    // Same identity/quality does not imply the same transport metadata. Keep
    // the completed representation so the next cache gate uses wasm_alac_v2.
    const track = { ...queue.getTrack(nextIndex), ...candidate.track, duration: candidate.duration };
    queue.replaceTrack(nextIndex, track);
    media.setTrack(track);
    media.beginLyricLoad();
    rememberAutoplayTrack(track);
    status.currentSource = candidate.source;
    status.currentSourceTrackId = track.id;
    status.trackLoading = false;
    status.duration = Math.round(candidate.duration);
    status.position = Math.round(candidate.usingAlac ? candidate.position || 0 : candidate.audio.currentTime * 1000);
    status.state = "playing";
    status.transitionState = "mixing";
    playback.setDuration(status.duration);
    playback.setCurrentTime(status.position, { force: true });
    playback.setPlaying(true);
    status.persist();
    if (usingAlacWorklet) setupAlacWorkletCallbacks(trackToken);
    void loadLyrics(track);
    updateMediaSession(track, { activate: true });
    syncMediaSessionPosition();
    warmActiveLoudness(track);
    retiringDeck = old;
    old.output.fadeTo(0, seconds);
    candidate.output.fadeTo(1, seconds);
    old.timer = window.setTimeout(() => { if (retiringDeck === old) finishRetiringDeck(); }, seconds * 1000 + 50);
    spareDeck = null;
    schedulePrefetchNextTrack(trackToken);
    prepareAutoplayRecommendations();
  } catch {
    const stillOwned = transitionStarting === candidate || transitionCandidate === candidate;
    if (transitionCandidate === candidate) { stopDeck(candidate); transitionCandidate = null; }
    if (stillOwned) transitionFailedKey = candidate.key;
    if (transitionStarting === candidate) transitionStarting = null;
    if (stillOwned && !retiringDeck) status.transitionState = "idle";
  }
}

function checkTransition() {
  const status = useStatusStore();
  if (transitionCandidate && !candidateStillValid(transitionCandidate)) cancelTransition();
  if (!status.crossfadeEnabled || status.state !== "playing" || status.trackLoading || retiringDeck || transitionStarting) return;
  const candidate = transitionCandidate;
  if (!candidate?.ready) return;
  const remaining = (status.duration - status.position) / 1000;
  const seconds = Math.min(status.crossfadeSeconds || 4, status.duration / 3000, candidate.duration / 3000);
  if (remaining > 0.15 && remaining <= seconds) void startTransition(candidate, remaining);
}

export function setCrossfadeEnabled(enabled) {
  const status = useStatusStore();
  status.crossfadeEnabled = Boolean(enabled);
  status.persist();
  cancelTransition();
  if (enabled) { prepareAudio(); applyActiveLevels(); schedulePrefetchNextTrack(trackToken, { immediate: true }); }
}

export function setCrossfadeSeconds(seconds) {
  const status = useStatusStore();
  status.crossfadeSeconds = Math.min(12, Math.max(1, Number(seconds) || 4));
  status.persist();
}

export function setSoundCheckEnabled(enabled) {
  const status = useStatusStore();
  status.soundCheckEnabled = Boolean(enabled);
  if (!status.soundCheckEnabled) {
    transitionCandidate?.loudnessController?.abort();
    if (transitionCandidate) transitionCandidate.loudnessController = null;
  }
  status.persist();
  prepareAudio();
  activeLoudnessGain = cachedLoudnessGain(useMediaStore().track);
  applyActiveLevels();
  retiringDeck?.output?.setLoudnessGain?.(cachedLoudnessGain(retiringDeck.track));
  warmActiveLoudness(useMediaStore().track);
  // A candidate's gain is sampled again immediately before its playback.
}

function sourceIdForTrack(track) {
  if (!track) return "";
  if (track.source_id || track.selected_id || track.track_id) {
    return track.source_id || track.selected_id || track.track_id;
  }
  if (typeof track.id === "string" && track.id.includes("--")) {
    return track.id.split("--").slice(1, -1).join("--");
  }
  return track.id || track.query || "";
}

function normalizeTrackForQuality(track) {
  if (!track) return track;
  const quality = useStatusStore().quality || track.quality || "high";
  if (track.quality === quality && track.id) return track;

  const sourceId = sourceIdForTrack(track);
  if (!sourceId) return { ...track, quality };
  return { ...trackFromSearch(
    {
      platform: track.platform,
      id: sourceId,
      song: track.title || track.query || "未知歌曲",
      singer: track.artist || "未知艺人",
      cover: track.cover || track.cover_url || "",
    },
    quality,
  ), ...(track.autoplayRecommended ? { autoplayRecommended: true } : {}) };
}

function replaceQueuedTrackWithQuality(index, track) {
  const queue = useQueueStore();
  const queued = queue.getTrack(index);
  if (!queued || !track || trackIdentity(queued) !== trackIdentity(track)) return track;
  if (queued.id !== track.id || queued.quality !== track.quality) {
    queue.replaceTrack(index, track);
  }
  return queue.getTrack(index) || track;
}

function hasAudioSource() {
  return Boolean(usingAlacWorklet || (useStatusStore().currentSource && (audio?.currentSrc || audio?.src)));
}

function takeIdleElement() {
  if (idleElement) { const element = idleElement; idleElement = null; return element; }
  if (typeof document === "undefined" || !document.createElement) return null;
  const element = document.createElement("audio");
  element.crossOrigin = "anonymous";
  element.preload = "auto";
  element.hidden = true;
  document.body?.appendChild?.(element);
  return element;
}

// Destroys the element's media player; callers ensure another player is
// already active or none is wanted.
function releaseElement(element) {
  if (!element) return;
  element.pause();
  const source = element.currentSrc || element.src;
  if (source) {
    element.removeAttribute("src");
    element.load();
  }
  element.muted = false;
  if (!idleElement && element !== audio) idleElement = element;
}

function stopPreroll() {
  if (!armedNext?.prerolled) return;
  armedNext.prerolled = false;
  armedNext.element?.pause();
}

function disarmNext() {
  const armed = armedNext;
  armedNext = null;
  if (armed?.element) releaseElement(armed.element);
  if (armed?.source?.startsWith("blob:")) URL.revokeObjectURL(armed.source);
}

function armedStillNext(armed = armedNext) {
  const { queue, status } = stores();
  if (!armed || armed.token !== trackToken || autoplayAdvance || status.repeatMode === "one") return false;
  const index = nextQueueIndex();
  return index === armed.index && index === status.playIndex + 1 &&
    prefetchKey(normalizeTrackForQuality(queue.getTrack(index))) === armed.key;
}

// Called from the current native element's time updates.
function checkPreroll() {
  const armed = armedNext;
  if (!armed?.element || usingAlacWorklet || !audio) return;
  const status = useStatusStore();
  // The final timeupdate of a natural EOS already reports paused=true.
  if (audio.ended) return;
  if (!armedStillNext(armed) || status.state !== "playing" || audio.paused || retiringDeck || transitionStarting) {
    stopPreroll();
    return;
  }
  const remaining = (Number.isFinite(audio.duration) ? audio.duration : 0) - audio.currentTime;
  if (armed.prerolled || !(remaining > 0) || remaining > PREROLL_SECONDS) return;
  armed.prerolled = true;
  armed.element.muted = true;
  Promise.resolve(armed.element.play()).catch(() => { if (armedNext === armed) armed.prerolled = false; });
}

async function armNextTrack(myToken = trackToken) {
  if (!resolveCachedTrackSource || myToken !== trackToken) return;
  const { queue, status } = stores();
  if (status.repeatMode === "one") return;
  const index = nextQueueIndex();
  const track = index === status.playIndex + 1 ? normalizeTrackForQuality(queue.getTrack(index)) : null;
  const key = prefetchKey(track);
  if (!key || trackIdentity(track) === trackIdentity(status.currentTrack)) return;
  if (armedNext?.key === key && armedNext.token === myToken) return;
  const resolved = await resolveCachedTrackSource(track).catch(() => null);
  if (!resolved?.source) return;
  // Only a native container can be attached synchronously. ALAC packet
  // streams keep the normal decoder load path.
  const nativeTrack = resolved.track?.streaming_mode !== "wasm_alac";
  if (!nativeTrack || myToken !== trackToken || nextQueueIndex() !== index ||
      prefetchKey(normalizeTrackForQuality(queue.getTrack(index))) !== key) {
    if (resolved.source.startsWith("blob:")) URL.revokeObjectURL(resolved.source);
    return;
  }
  disarmNext();
  armedNext = { key, token: myToken, index, source: resolved.source, track: resolved.track, element: null, prerolled: false };
  const element = takeIdleElement();
  if (!element) return;
  if (!preparedElements.has(element)) { preparedElements.add(element); attachAudioEvents(element); }
  element.muted = false;
  element.src = resolved.source;
  element.load();
  armedNext.element = element;
}

// Natural EOF only. Returns false when the regular resolve path must be used.
function startArmedNext() {
  const armed = armedNext;
  if (!armedStillNext(armed)) return false;
  const status = useStatusStore();
  armedNext = null;
  status.playIndex = armed.index;
  status.persist();
  void loadTrack(status.currentTrack, true, { resolved: { source: armed.source, track: armed.track, element: armed.element } });
  return true;
}

function clearSavedProgress() {
  localStorage.removeItem(PROGRESS_STORAGE_KEY);
}

function clearPrefetchRetry() {
  if (!prefetchRetryTimer) return;
  window.clearTimeout(prefetchRetryTimer);
  prefetchRetryTimer = 0;
}

function cancelActiveTrackCache() {
  if (!activeTrackCache) return;
  activeTrackCache.controller?.abort?.();
  activeTrackCache = null;
}

function releaseNativeSource() {
  if (!audio) return;
  const source = audio.currentSrc || audio.src;
  if (!source) return;
  audio.removeAttribute("src");
  audio.load();
  if (source.startsWith("blob:")) URL.revokeObjectURL(source);
}

function detachCurrentAudio({ keepNativeSource = false, previousCustomSource = usingAlacWorklet ? useStatusStore().currentSource : null } = {}) {
  cancelTransition();
  playControlToken += 1;
  if (alacPlayer) {
    // Stop the custom transport before detaching the native element.  The
    // worklet owns its AudioContext destination and must not keep rendering a
    // previous track after a queue change.
    void alacPlayer.stop?.();
  }
  usingAlacWorklet = false;
  if (previousCustomSource?.startsWith("blob:")) URL.revokeObjectURL(previousCustomSource);
  if (!audio) return;
  audio.pause();
  if (!keepNativeSource) releaseNativeSource();
}

function cancelPendingLoad() {
  const status = useStatusStore();
  if (!status.trackLoading) return false;

  // Invalidate both async pipelines. A cold resolve may still be waiting on
  // the backend, and an attached source may still have a pending play()
  // promise. Neither may resurrect playback after an explicit pause.
  loadToken += 1;
  trackToken += 1;
  disarmNext();
  clearPendingSeek();
  clearPrefetchRetry();
  cancelActiveTrackCache();
  detachCurrentAudio();
  useMediaStore().clearLyric();
  status.trackLoading = false;
  status.currentSource = null;
  status.currentSourceTrackId = null;
  status.state = "paused";
  playback.setPlaying(false);
  seekTarget = null;
  playback.setSeeking(false);
  updateMediaSession();
  return true;
}

function currentBufferLeadSeconds() {
  if (!audio || !Number.isFinite(audio.currentTime)) return 0;
  const currentTime = audio.currentTime;
  for (let index = 0; index < audio.buffered.length; index += 1) {
    const start = audio.buffered.start(index);
    const end = audio.buffered.end(index);
    if (currentTime + 0.25 >= start && currentTime <= end + 0.25) {
      return Math.max(0, end - currentTime);
    }
  }
  return 0;
}

function prefetchNetworkPolicy() {
  const connection = navigator.connection || navigator.mozConnection || navigator.webkitConnection;
  if (!connection) return { disabled: false, targetSeconds: PREFETCH_BUFFER_TARGET_SECONDS };
  const effectiveType = String(connection.effectiveType || "").toLowerCase();
  const downlink = Number(connection.downlink);
  if (connection.saveData || effectiveType === "slow-2g" || effectiveType === "2g") {
    return { disabled: true, targetSeconds: Number.POSITIVE_INFINITY };
  }
  if (effectiveType === "3g" || (Number.isFinite(downlink) && downlink > 0 && downlink < 1.5)) {
    return { disabled: false, targetSeconds: 45 };
  }
  return { disabled: false, targetSeconds: PREFETCH_BUFFER_TARGET_SECONDS };
}

// Apple Music AutoPlay station for the current queue. Like MusicKit, it is
// created once from the queue's latest songs and then reused for refills; a
// replaced or cleared queue, or toggling AutoPlay, starts a new one.
let autoplayStation = "";
const AUTOPLAY_SEEDS = 10;
function resetAutoplayStation() {
  autoplayStation = "";
}

function invalidateAutoplay({ resetFailure = true } = {}) {
  autoplayGeneration += 1;
  autoplayRequest?.controller.abort();
  autoplayRequest = null;
  autoplayAdvance = null;
  if (resetFailure) autoplayFailedSeed = "";
  const status = useStatusStore();
  status.autoplayState = "idle";
  status.autoplayError = "";
}

function rememberAutoplayTrack(track) {
  const key = trackIdentity(track);
  if (!key) return;
  autoplayRecent.delete(key);
  autoplayRecent.set(key, { platform: track.platform, id: sourceIdForTrack(track) });
  if (autoplayRecent.size > 200) autoplayRecent.delete(autoplayRecent.keys().next().value);
}

// This fetches catalog metadata only. Every audio request still goes through
// prefetchNextTrack's successful full-current-track cache gate below.
async function ensureAutoplayRecommendations({ force = false } = {}) {
  const { queue, status } = stores();
  const seed = status.currentTrack;
  if (!status.autoplayEnabled || !seed || !getContinuationRecommendations) return 0;
  if (seed.platform !== "applemusic") {
    status.autoplayState = "empty";
    status.autoplayError = "这个音乐来源暂不支持自动续播";
    return 0;
  }
  if (queue.queueLength - status.playIndex - 1 > 2) return 0;
  const seedKey = trackIdentity(seed);
  if (autoplayRequest?.seedKey === seedKey) return autoplayRequest.promise;
  if (!force && autoplayFailedSeed === seedKey) return 0;
  const generation = autoplayGeneration;
  const controller = new AbortController();
  const isCurrent = () => generation === autoplayGeneration && status.autoplayEnabled &&
    trackIdentity(status.currentTrack) === seedKey && !controller.signal.aborted;
  rememberAutoplayTrack(seed);
  const exclude = [...new Set([
    ...queue.queue.filter((item) => item.platform === seed.platform).map(sourceIdForTrack),
    ...[...autoplayRecent.values()].filter((item) => item.platform === seed.platform).map((item) => item.id),
  ].filter(Boolean))];
  status.autoplayState = "loading";
  status.autoplayError = "";
  const request = { seedKey, controller, promise: null };
  const seedIds = queue.queue.slice(0, Math.max(0, status.playIndex) + 1)
    .filter((item) => item.platform === seed.platform)
    .map(sourceIdForTrack).filter(Boolean).slice(-AUTOPLAY_SEEDS);
  const station = autoplayStation;
  request.promise = Promise.resolve().then(() => getContinuationRecommendations({
    platform: seed.platform, seedId: sourceIdForTrack(seed), seedIds, station, exclude, limit: 10, signal: controller.signal,
  })).then((result) => {
    if (!isCurrent()) return 0;
    if (typeof result?.station === "string" && station === autoplayStation) autoplayStation = result.station;
    const known = new Set([...queue.queue.map(trackIdentity), ...autoplayRecent.keys()]);
    const tracks = [];
    for (const item of result?.tracks || []) {
      if (!item?.id || item.platform !== seed.platform) continue;
      const track = { ...trackFromSearch(item, status.quality || "high"), autoplayRecommended: true };
      const key = trackIdentity(track);
      if (!key || known.has(key)) continue;
      known.add(key);
      tracks.push(track);
      if (tracks.length === 10) break;
    }
    if (!tracks.length) {
      autoplayFailedSeed = seedKey;
      status.autoplayState = "empty";
      status.autoplayError = "暂时没有更多新歌曲，可以重试";
      return 0;
    }
    queue.insertManyToQueue(tracks, queue.queueLength);
    status.autoplayState = "ready";
    autoplayFailedSeed = "";
    if (status.isPlaying && hasAudioSource()) schedulePrefetchNextTrack(trackToken);
    return tracks.length;
  }).catch((error) => {
    if (!isCurrent()) return 0;
    autoplayFailedSeed = seedKey;
    status.autoplayState = "error";
    status.autoplayError = "自动续播暂时不可用，点击重试";
    return 0;
  }).finally(() => {
    if (autoplayRequest === request) autoplayRequest = null;
  });
  autoplayRequest = request;
  return request.promise;
}

function prepareAutoplayRecommendations() {
  const status = useStatusStore();
  if (!status.autoplayEnabled || !status.isPlaying || prefetchNetworkPolicy().disabled) return;
  void ensureAutoplayRecommendations();
}

export function setAutoplayEnabled(enabled) {
  cancelTransition();
  const { queue, status } = stores();
  const wasWaitingAtEnd = Boolean(autoplayAdvance);
  invalidateAutoplay();
  resetAutoplayStation();
  if (wasWaitingAtEnd) {
    status.state = "paused";
    playback.setPlaying(false);
  }
  status.autoplayEnabled = Boolean(enabled);
  if (status.autoplayEnabled) {
    status.repeatMode = "off";
    prepareAutoplayRecommendations();
  } else {
    queue.removeUpcomingRecommendations(status.playIndex);
    clearPrefetchRetry();
  }
  status.persist();
  refreshTransitionPreparation();
}

export function toggleAutoplay() {
  setAutoplayEnabled(!useStatusStore().autoplayEnabled);
}

export async function retryAutoplay() {
  const { queue, status } = stores();
  if (!status.autoplayEnabled || !status.currentTrack || autoplayRequest) return false;
  autoplayFailedSeed = "";
  const transportEnded = usingAlacWorklet ? alacPlayer?.ended : audio?.ended;
  // A warm recommendation request can fail while the last song is still
  // playing. Retrying that metadata must not act like pressing Next or
  // resume a user-paused song. Advance only after the transport reached EOF
  // or the previous end-of-queue attempt explicitly stopped playback.
  if (status.playIndex === queue.queueLength - 1 && (status.state === "stopped" || transportEnded)) return nextTrack(true);
  return Boolean(await ensureAutoplayRecommendations({ force: true }));
}

function canPrefetchWithoutStarvingPlayback() {
  // A WASM ALAC stream has no native `buffered` timeline. Once its reader has
  // reached EOF, the current track is fully consumed and it is safe to fill
  // its local cache before starting the next-track download.
  if (usingAlacWorklet && alacPlayer?.eof) return true;
  if (!audio) return false;
  const policy = prefetchNetworkPolicy();
  if (policy.disabled) return false;
  const source = audio.currentSrc || audio.src || "";
  // A blob URL is already local, so prefetching the next item cannot compete
  // with the active stream for network bandwidth.
  if (source.startsWith("blob:")) return true;

  const lead = currentBufferLeadSeconds();
  if (lead >= policy.targetSeconds) return true;

  // Near the end of a short track there is little remaining audio to protect;
  // allowing prefetch here helps avoid a gap without stealing its startup
  // bandwidth.
  const duration = Number.isFinite(audio.duration) ? audio.duration : 0;
  return (
    audio.readyState >= 2 &&
    audio.currentTime > 0 &&
    duration > 0 &&
    duration - audio.currentTime <= policy.targetSeconds
  );
}

function updateMediaSession(track = useMediaStore().track, { activate = false } = {}) {
  if (activate && systemMedia?.activate) systemMedia.activate(track);
  else systemMedia?.update(track);
  playbackLifecycle?.update();
}

function syncMediaSessionPosition(force = false) {
  systemMedia?.syncPosition({ force });
}

function activeAudioContext() {
  return usingAlacWorklet ? alacPlayer?.context : effectOutput(audio)?.context;
}

function waitingForAudioRecovery() {
  return recoveringAudio && (!recoveryReady || activeAudioContext()?.state !== "running");
}

function confirmRecoveredAudio(position) {
  if (!recoveringAudio || waitingForAudioRecovery() || !useStatusStore().isPlaying || position <= recoveryPosition) return;
  if (usingAlacWorklet ? !alacPlayer?.playing : audio?.paused) return;
  recoveringAudio = false;
  useStatusStore().state = "playing";
  playback.setPlaying(true);
  updateMediaSession();
}

function setupSystemPlayback(options) {
  systemMedia = options.mediaSessionFactory?.({
    readState: () => {
      const { media, status } = stores();
      return { track: media.track, state: status.state, position: status.position,
        duration: status.duration, speed: usingAlacWorklet ? 1 : status.speed,
        wantsPlay: status.isPlaying || (status.trackLoading && loadWantsPlay) };
    },
    actions: { play, pause, previous: prevTrack, next: () => nextTrack(true), seek, stop },
  }) || null;
  systemMedia?.register();
  playbackLifecycle = options.lifecycleFactory?.({
    getSnapshot: () => ({ context: activeAudioContext(), token: trackToken,
      wantsPlay: useStatusStore().isPlaying && !useStatusStore().trackLoading && Boolean(useStatusStore().currentSource) }),
    onInterrupted: () => {
      const status = useStatusStore();
      if (!status.isPlaying) return;
      recoveringAudio = true;
      recoveryReady = false;
      recoveryPosition = usingAlacWorklet ? status.position : Math.round((audio?.currentTime || 0) * 1000);
      status.state = "buffering";
      playback.setPlaying(false);
      playback.setCurrentTime(recoveryPosition, { force: true });
      status.position = recoveryPosition;
      status.setNotice("音频暂时被系统中断，等待恢复…");
      systemMedia?.update();
    },
    // The render processor/native media clock must confirm real output before
    // advancing the displayed time. Resuming a context alone is insufficient.
    onRecovered: () => {
      if (recoveringAudio && useStatusStore().isPlaying && activeAudioContext()?.state === "running") recoveryReady = true;
      systemMedia?.update();
    },
  }) || null;
}

function resetForLoad(duration = 0) {
  const { status } = stores();
  recoveringAudio = false;
  recoveryReady = true;
  // Stop sound immediately, but retain the native player until its source can
  // be replaced directly. Empty load() calls release Android's media owner;
  // repeating them between songs loses notification/background eligibility.
  // currentSource=null below makes the retained source inactive and prevents
  // either events or Play from treating the previous song as the new one.
  status.trackLoading = true;
  const previousCustomSource = usingAlacWorklet ? status.currentSource : null;
  status.currentSource = null;
  status.currentSourceTrackId = null;
  if (audio) detachCurrentAudio({ keepNativeSource: true, previousCustomSource });
  status.position = pendingSeek?.token === trackToken ? pendingSeek.target : 0;
  status.duration = duration;
  status.state = "idle";
  playback.setCurrentTime(status.position, { force: true });
  playback.setDuration(duration);
  playback.setPlaying(false);
  updateMediaSession();
}

function syncPosition(force = false) {
  if (!audio || pendingSeek || waitingForAudioRecovery()) return;
  const pos = Number.isFinite(audio.currentTime) ? Math.round(audio.currentTime * 1000) : 0;
  const adjusted = playback.setCurrentTime(pos, { force });
  useStatusStore().position = Math.round(adjusted);
  confirmRecoveredAudio(pos);
  syncMediaSessionPosition();
  checkTransition();
  checkPreroll();
}

function syncDuration() {
  if (!audio || usingAlacWorklet) return;
  const status = useStatusStore();
  const media = useMediaStore();
  let duration = Number.isFinite(audio.duration)
    ? Math.round(audio.duration * 1000)
    : media.track?.duration || status.duration || 0;
  if (media.track?.status === "streaming" && !pendingSeek?.completed) {
    duration = Math.max(duration, media.track?.duration || 0, status.duration || 0);
  }
  status.duration = duration;
  playback.setDuration(duration);
  syncMediaSessionPosition();
}

function attachAudioEvents(element = audio) {
  if (!element) return;
  const active = () => element === audio && !usingAlacWorklet && Boolean(useStatusStore().currentSource);
  element.addEventListener("timeupdate", () => {
    if (!active()) return;
    const pos = Math.round(audio.currentTime * 1000);
    if (seekTarget !== null && !hasReachedSeekTarget(pos)) return;
    syncPosition();
  });
  element.addEventListener("seeking", () => { if (active()) { cancelTransition(); syncPosition(true); } });
  element.addEventListener("seeked", () => {
    if (!active()) return;
    if (pendingSeek?.applied) finishNativeSeek(pendingSeek);
    else { hasReachedSeekTarget(Math.round(element.currentTime * 1000)); syncPosition(true); }
    refreshTransitionPreparation();
  });
  const retrySeek = () => { if (active()) applyPendingSeek(); };
  element.addEventListener("progress", retrySeek);
  element.addEventListener("canplay", retrySeek);
  element.addEventListener("loadedmetadata", () => { if (active()) { syncDuration(); retrySeek(); } });
  element.addEventListener("durationchange", () => { if (active()) { syncDuration(); retrySeek(); } });
  element.addEventListener("play", () => {
    if (!active() || element.paused || pendingSeek) return;
    const status = useStatusStore();
    // Ignore a queued event from the source detached by resetForLoad().
    if (!status.currentSource || !hasAudioSource()) {
      playback.setPlaying(false);
      return;
    }
    // `play` only means that the element accepted the request.  On a cold
    // source the browser can still be waiting for its first decodable bytes;
    // mark that phase explicitly and let `playing` start the interpolated
    // progress clock once audio is really advancing.
    status.state = "buffering";
    playback.setPlaying(false);
    updateMediaSession();
  });
  element.addEventListener("playing", () => {
    if (!active() || pendingSeek) return;
    const status = useStatusStore();
    // A queued `playing` event can arrive after a user pause on some WebKit
    // versions. Do not restart the interpolated clock unless the element is
    // still actively playing and the current generation has not been paused.
    if (audio.paused || status.state === "paused" || status.state === "stopped" || !status.currentSource || !hasAudioSource()) {
      playback.setPlaying(false);
      return;
    }
    // A queued native event can arrive while the OS audio session is still
    // interrupted, even when the AudioContext reports running.
    if (waitingForAudioRecovery()) { playback.setPlaying(false); return; }
    status.state = "playing";
    recoveringAudio = false;
    networkRetryKey = "";
    playback.setPlaying(true);
    const title = useMediaStore().track?.title;
    if (title) status.setNotice(`${title} 正在播放`);
    syncPosition(true);
    updateMediaSession(undefined, { activate: true });
    // A weak connection may have aborted the background cache while the
    // current stream was stalled. Give it another chance once playback has
    // recovered and the buffer policy permits it.
    schedulePrefetchNextTrack(trackToken);
    prepareAutoplayRecommendations();
  });
  const markBuffering = () => {
    if (!active()) return;
    const status = useStatusStore();
    if (status.state === "playing" || status.state === "buffering") {
      // Never let a full-file cache download compete with a stream that has
      // just started buffering. It will be retried after the next `playing`
      // event with the same buffer/network gates.
      cancelActiveTrackCache();
      status.state = "buffering";
      // Do not let playback.js extrapolate through a network stall.  The
      // next timeupdate/playing event will re-anchor the clock.
      playback.setPlaying(false);
      status.setNotice("正在缓冲音频…");
      updateMediaSession();
    }
  };
  element.addEventListener("waiting", markBuffering);
  element.addEventListener("stalled", markBuffering);
  element.addEventListener("pause", () => {
    if (!active() || !element.paused || pendingSeek) return;
    const status = useStatusStore();
    // Natural completion keeps playback intent through the handoff. A pause
    // task from detaching the previous source may also arrive after the next
    // source was attached but before its play() promise has settled.
    if (element.ended || (status.trackLoading && loadWantsPlay)) return;
    // A source is detached at the beginning of a cold load.  Keep the state
    // idle while that queued pause event is delivered instead of briefly
    // reporting a paused track that has not loaded yet.
    if (status.trackLoading && !status.currentSource) {
      playback.setPlaying(false);
      return;
    }
    status.state = status.state === "stopped" ? "stopped" : "paused";
    playback.setPlaying(false);
    updateMediaSession();
  });
  element.addEventListener("ended", () => {
    // Events queued for an old source can be delivered after this reusable
    // element has been reset. They must not advance the new track again.
    if (!element.ended) return;
    if (retiringDeck?.audio === element && !retiringDeck.usingAlac) finishRetiringDeck();
    if (!active() || pendingSeek) return;
    const status = useStatusStore();
    if (status.trackLoading || !status.currentSource) return;
    if (status.repeatMode === "one") {
      seek(0);
      void play();
      return;
    }
    if (!startArmedNext()) void nextTrack(false);
  });
  element.addEventListener("error", () => {
    if (!active() || !element.error) return;
    const status = useStatusStore();
    if (!status.currentSource || audioErrorTrackToken === trackToken) return;
    audioErrorTrackToken = trackToken;
    // A frozen/backgrounded phone can drop the stream's connection. Reload
    // the same song once at its position instead of skipping to the next.
    const track = status.currentTrack;
    const key = prefetchKey(track);
    if (element.error.code === 2 && track && key && networkRetryKey !== key) {
      networkRetryKey = key;
      const position = Math.round((element.currentTime || 0) * 1000);
      const wantsPlay = status.isPlaying || (status.trackLoading && loadWantsPlay);
      void loadTrack(track, wantsPlay);
      if (position > 0) void seek(position);
      return;
    }
    clearPendingSeek();
    playback.setPlaying(false);
    status.state = "idle";
    status.trackLoading = false;
    toast("当前音频流暂时无法播放，正在尝试下一首");
    updateMediaSession();
    void skipOnFailure(trackToken, () => trackToken);
  });
}

export function initPlayer(options) {
  if (initialized) return;
  initialized = true;
  audio = options.audio;
  resolveTrackSource = options.resolveTrackSource;
  waitForCompletedSource = options.waitForCompletedSource || null;
  trustedMediaOrigins = options.trustedMediaOrigins || [];
  prefetchTrackSource = options.prefetchTrackSource;
  cacheActiveTrackSource = options.cacheActiveTrackSource;
  loadLyricsForTrack = options.loadLyricsForTrack;
  getContinuationRecommendations = options.getContinuationRecommendations || null;
  alacPlayerFactory = options.alacPlayerFactory || null;
  resolveCachedTrackSource = options.resolveCachedTrackSource || null;
  outputFactory = options.outputFactory || null;
  lookupLoudness = options.lookupLoudness || null;
  warmTrackLoudness = options.warmLoudness || null;
  const status = useStatusStore();
  status.autoplayState = "idle";
  status.autoplayError = "";
  if (status.autoplayEnabled) status.repeatMode = "off";
  if (audio) {
    audio.volume = status.volume;
    audio.playbackRate = status.speed;
    attachAudioEvents();
  }
  if (status.currentTrack) useMediaStore().setTrack(status.currentTrack);
  clearSavedProgress();
  setupSystemPlayback(options);
  updateMediaSession();
}

// Call directly from a user event, before provider requests or IndexedDB.
// The same context survives queue changes, so automatic next-track playback
// keeps WebKit's permission even after transient user activation expires.
export function prepareAudio(quality = useStatusStore().quality) {
  try { outputFactory?.prepareSession?.(); } catch { /* optional browser API */ }
  const status = useStatusStore();
  if (status.crossfadeEnabled || status.soundCheckEnabled) {
    try { outputFactory?.prepare?.(quality); } catch { /* Native playback remains available. */ }
  }
  const track = { platform: "applemusic", quality };
  if (!alacPlayerFactory?.canRequest?.(track)) return;
  const sampleRate = ["hires", "hi-res"].includes(quality) ? 192000 : 48000;
  try {
    if (!alacPlayer && alacPlayerFactory.isLoaded?.() === false) {
      // Unlock this rate's shared context inside the gesture. The decoder
      // module downloads in parallel and later reuses that same context.
      alacPlayerFactory.warm?.(sampleRate);
      return;
    }
    if (!alacPlayer) alacPlayer = createAlacPlayer();
    alacPlayer.prepare?.(sampleRate);
  } catch (error) {
    // A browser context/resource limit must not abort the click handler. The
    // normal load path can report a recoverable error if creation still fails.
    console.warn("[player] audio context preparation failed", error);
  }
}

async function skipOnFailure(myToken, getCurrentToken) {
  const queue = useQueueStore();
  consecutiveFailures += 1;
  if (consecutiveFailures >= MAX_CONSECUTIVE_FAILURES || consecutiveFailures >= queue.queueLength) {
    consecutiveFailures = 0;
    await onQueueEnded();
    return;
  }
  window.setTimeout(() => {
    if (myToken === getCurrentToken()) void nextTrack();
  }, SKIP_ON_ERROR_DELAY_MS);
}

function setupAlacWorkletCallbacks(myToken) {
  if (!alacPlayer) return;
  alacPlayer.onState = (state) => {
    if (myToken !== trackToken) return;
    const status = useStatusStore();
    if (state === "playing") {
      if (waitingForAudioRecovery()) return;
      recoveringAudio = false;
      status.state = "playing";
      playback.setPlaying(true);
      syncMediaSessionPosition();
      updateMediaSession(undefined, { activate: true });
      schedulePrefetchNextTrack(myToken);
      prepareAutoplayRecommendations();
    } else if (state === "paused") {
      // EOF is followed synchronously by onEnded. Do not tell the system the
      // user paused in the gap before the next source starts loading.
      if (alacPlayer?.ended) return;
      status.state = "paused";
      playback.setPlaying(false);
      updateMediaSession();
    } else if (state === "buffering") {
      status.state = "buffering";
      playback.setPlaying(false);
      updateMediaSession();
    }
  };
  alacPlayer.onTime = (position) => {
    if (myToken !== trackToken || pendingSeek || waitingForAudioRecovery()) return;
    const safe = Math.max(0, Math.round(position));
    playback.setCurrentTime(safe, { force: true });
    const status = useStatusStore();
    status.position = safe;
    confirmRecoveredAudio(safe);
    syncMediaSessionPosition();
    checkTransition();
  };
  alacPlayer.onEnded = () => {
    if (myToken !== trackToken) return;
    if (useStatusStore().repeatMode === "one") {
      // Worklet play() rewinds its complete packet cache after ended.
      void play();
    } else if (!startArmedNext()) void nextTrack(false);
  };
  alacPlayer.onStreamComplete = ({ duration } = {}) => {
    if (myToken !== trackToken) return;
    if (duration > 0) {
      useStatusStore().duration = Math.round(duration);
      playback.setDuration(Math.round(duration));
      syncMediaSessionPosition();
    }
    // Persist the bytes we already received even under Data Saver. Only the
    // next song's network request is subject to the prefetch policy.
    void ensureActiveTrackCache(myToken).then((ready) => {
      if (myToken === trackToken) schedulePrefetchNextTrack(myToken, { immediate: ready });
    });
  };
  alacPlayer.onError = (error) => {
    if (myToken !== trackToken) return;
    clearPendingSeek();
    const status = useStatusStore();
    usingAlacWorklet = false;
    status.state = "paused";
    playback.setPlaying(false);
    toast(error?.message || "ALAC 解码失败，请稍后重试");
    updateMediaSession();
  };
}

async function tryLoadAlacWorklet(source, autoPlay, track, myToken) {
  if (!alacPlayerFactory || track?.streaming_mode !== "wasm_alac") return null;
  let canUse = false;
  try {
    canUse = Boolean(await alacPlayerFactory.canHandle?.(track, source));
  } catch (error) {
    console.warn("[player] ALAC capability check failed", error);
  }
  if (!canUse || myToken !== trackToken) return null;
  // Only an actual switch to the custom renderer releases the stopped native
  // source. Normal native-to-native queue handoffs never load an empty URL.
  releaseNativeSource();
  if (!alacPlayer) {
    try {
      await alacPlayerFactory.load?.();
    } catch (error) {
      if (myToken !== trackToken) return { ok: false };
      toast("ALAC 解码器加载失败，请检查网络后重试");
      return { ok: false, error };
    }
    if (myToken !== trackToken) return { ok: false };
    if (!alacPlayer) {
      alacPlayer = createAlacPlayer();
      // Reuse the context unlocked by the gesture and fetch the codec and
      // worklet processor in parallel, as an eagerly created player would.
      alacPlayer.prepare?.(["hires", "hi-res"].includes(track.quality) ? 192000 : 48000);
    }
  }
  setupAlacWorkletCallbacks(myToken);
  try {
    usingAlacWorklet = true;
    alacPlayer.setVolume?.(useStatusStore().volume);
    alacPlayer.setLoudnessGain?.(activeLoudnessGain);
    alacPlayer.cancelFade?.(1);
    const metadata = await alacPlayer.load(source, {
      completed: track?.status === "completed",
      trustedMediaOrigins,
      waitForCompletedSource: waitForCompletedSource ? (options) => waitForCompletedSource(track, options) : undefined,
    });
    if (myToken !== trackToken) return { ok: false };
    usingAlacWorklet = true;
    const status = useStatusStore();
    status.currentSource = source;
    status.currentSourceTrackId = track?.id || null;
    if (metadata?.duration > 0) {
      status.duration = Math.round(metadata.duration);
      playback.setDuration(status.duration);
    }
    status.state = autoPlay ? "buffering" : "paused";
    playback.setPlaying(false);
    if (pendingSeek) {
      pendingSeek.resume = autoPlay;
      await applyPendingSeek();
      return { ok: true, playOk: autoPlay, track };
    }
    if (autoPlay) {
      const playOk = await play({ duringLoad: true });
      return { ok: true, playOk, track };
    }
    return { ok: true, playOk: false, track };
  } catch (error) {
    if (myToken !== trackToken) return { ok: false };
    clearPendingSeek();
    // The packet endpoint is intentionally not a native media URL. Falling
    // through to `<audio>` after a decoder failure would produce a confusing
    // "unsupported source" error and can leave a stale worklet attached.
    // Browsers that cannot request wasm_alac never enter this branch and keep
    // the original full-download path.
    usingAlacWorklet = false;
    await alacPlayer.stop?.();
    toast("ALAC 流暂不可用，请稍后重试");
    console.warn("[player] ALAC worklet fallback", error);
    return { ok: false, error };
  }
}

export async function load(source, autoPlay = true, track = useMediaStore().track, { element: preparedElement = null } = {}) {
  if (!audio || !source) return { ok: false };
  const status = useStatusStore();
  const token = ++loadToken;
  loadWantsPlay = autoPlay;
  seekTarget = null;
  playback.setSeeking(false);
  resetForLoad(track?.duration || 0);
  try {
    const alacOutcome = await tryLoadAlacWorklet(source, autoPlay, track, trackToken);
    if (token !== loadToken) return { ok: false };
    if (alacOutcome) {
      consecutiveFailures = 0;
      return alacOutcome;
    }
    const adopted = preparedElement && preparedElement !== audio && (preparedElement.currentSrc || preparedElement.src) === source;
    if (adopted) {
      // The preloaded element may already be rendering muted. Promote it,
      // rewind to the first frame and only then let it be heard. The ended
      // element is released after the new player is active.
      const previous = audio;
      const previousSource = previous.currentSrc || previous.src;
      audio = preparedElement;
      if (audio.currentTime > 0) {
        audio.addEventListener("seeked", () => { if (audio === preparedElement) audio.muted = false; }, { once: true });
        audio.currentTime = 0;
      } else audio.muted = false;
      releaseElement(previous);
      if (previousSource?.startsWith("blob:")) URL.revokeObjectURL(previousSource);
    } else if (preparedElement) releaseElement(preparedElement);
    audio.volume = status.volume;
    audio.playbackRate = status.speed;
    if (!adopted) {
      const previousSource = audio.currentSrc || audio.src;
      audio.src = source;
      if (previousSource !== source && previousSource?.startsWith("blob:")) URL.revokeObjectURL(previousSource);
    }
    applyActiveLevels({ resetFade: true });
    if (!adopted) audio.load();
    if (token !== loadToken) return { ok: false };
    status.currentSource = source;
    status.currentSourceTrackId = track?.id || null;
    applyPendingSeek();
    // Do not report a playing state before the media element has emitted its
    // `playing` event.  This is especially important for a cold stream where
    // `audio.play()` may remain pending while the first range is downloaded.
    status.state = autoPlay ? "buffering" : "paused";
    playback.setPlaying(false);
    consecutiveFailures = 0;
    // `loadTrack` keeps trackLoading=true until the source has been attached;
    // allow this internal autoplay attempt while preventing a user click from
    // accidentally resuming the previous source during an in-flight load.
    const playOk = autoPlay ? await play({ duringLoad: true }) : true;
    // A promoted element emitted `playing` while it was still inactive.
    if (adopted && playOk && token === loadToken) updateMediaSession(track, { activate: true });
    return { ok: true, playOk, track };
  } catch (error) {
    if (token === loadToken) {
      clearPendingSeek();
      status.state = "idle";
      toast(error?.message || "加载音频失败");
    }
    return { ok: false, error };
  } finally {
    if (token === loadToken) { status.trackLoading = false; applyPendingSeek(); updateMediaSession(); }
  }
}

async function loadTrack(track, autoPlay = true, { resolved: preResolved = null } = {}) {
  if (!track || !resolveTrackSource) return false;
  disarmNext();
  invalidateAutoplay();
  rememberAutoplayTrack(track);
  if (autoPlay) prepareAudio();
  const { media, status } = stores();
  track = normalizeTrackForQuality(track);
  if (status.playIndex >= 0) {
    track = replaceQueuedTrackWithQuality(status.playIndex, track);
  }
  clearPrefetchRetry();
  cancelActiveTrackCache();
  clearPendingSeek();
  loadWantsPlay = autoPlay;
  const myToken = ++trackToken;
  media.setTrack(track);
  media.beginLyricLoad();
  status.setNotice(`${track.title || "音乐"} 正在准备`);
  resetForLoad(track.duration || 0);
  // Metadata is independent of the audio task. Start immediately even when
  // the provider still needs to prepare or restore the song's audio.
  void loadLyrics(track);
  let resolved = null;
  try {
    // A pre-resolved local source continues without yielding to a task, so
    // the native element receives its new src and play() inside `ended`.
    resolved = preResolved || await resolveTrackSource(track);
  } catch (error) {
    if (myToken === trackToken) {
      clearPendingSeek();
      status.currentSource = null;
      status.currentSourceTrackId = null;
      status.state = "idle";
      status.trackLoading = false;
      media.clearLyric();
      toast(error?.message || "音频准备失败");
      updateMediaSession();
    }
    return false;
  }
  if (myToken !== trackToken) return false;
  if (!resolved?.source) {
    clearPendingSeek();
    status.currentSource = null;
    status.currentSourceTrackId = null;
    status.state = "idle";
    status.trackLoading = false;
    media.clearLyric();
    updateMediaSession();
    await skipOnFailure(myToken, () => trackToken);
    return false;
  }
  const nextTrackData = { ...track, ...resolved.track };
  if (pendingSeek?.token === myToken) pendingSeek.track = nextTrackData;
  activeLoudnessGain = cachedLoudnessGain(nextTrackData);
  warmActiveLoudness(nextTrackData, myToken);
  media.setTrack(nextTrackData);
  updateMediaSession(nextTrackData);
  status.setNotice(`${nextTrackData.title || "音乐"} 已就绪`);
  void loadLyrics(nextTrackData);
  const outcome = await load(resolved.source, autoPlay, nextTrackData, { element: resolved.element });
  if (myToken !== trackToken) return false;
  if (!outcome.ok) {
    await skipOnFailure(myToken, () => trackToken);
    return false;
  }
  // A source can be valid even when the browser rejected this particular
  // autoplay request. Keep it available for a manual retry, but don't start
  // competing full-file work while no playback has actually begun.
  if (!autoPlay || outcome.playOk) schedulePrefetchNextTrack(myToken);
  if (autoPlay && outcome.playOk) prepareAutoplayRecommendations();
  return true;
}

async function loadLyrics(track) {
  if (!loadLyricsForTrack) return;
  const media = useMediaStore();
  const myToken = trackToken;
  const request = ++lyricGeneration;
  if (!media.lyricLines?.length) media.beginLyricLoad();
  try {
    const lines = await loadLyricsForTrack(track);
    if (myToken === trackToken && request === lyricGeneration) media.setLyric(lines);
  } catch (error) {
    console.warn("[player] lyric failed", error);
    if (myToken === trackToken && request === lyricGeneration) media.clearLyric();
  }
}

function nextQueueIndex() {
  const { queue, status } = stores();
  if (!queue.queueLength || status.playIndex < 0) return -1;
  if (status.playIndex < queue.queueLength - 1) return status.playIndex + 1;
  if (status.repeatMode === "list" || status.repeatMode === "one") return 0;
  return -1;
}

function prefetchKey(track) {
  if (!track) return "";
  return [trackIdentity(track), track.quality || "high"].filter(Boolean).join(":");
}

// A full cache for the active song is the gate for the next-song cache. This
// keeps two full-file requests from competing on a slow connection while
// still allowing the current audio element to start from its first playable
// bytes.
function ensureActiveTrackCache(myToken) {
  if (!cacheActiveTrackSource || myToken !== trackToken) return Promise.resolve(false);
  const { status } = stores();
  // Keep the actual active source quality for this cache gate. If the user
  // changes the preference mid-song, the current stream is still the old
  // source; only the next queue item should be normalized to the new setting.
  const track = useMediaStore().track || status.currentTrack;
  const key = prefetchKey(track);
  if (!track || !key) return Promise.resolve(false);
  if (activeTrackCache?.key === key && activeTrackCache.token === myToken) {
    return activeTrackCache.promise;
  }

  cancelActiveTrackCache();
  const controller = new AbortController();
  const promise = Promise.resolve()
    .then(() => cacheActiveTrackSource(track, {
      signal: controller.signal,
      blob: usingAlacWorklet ? alacPlayer?.completeBlob : undefined,
    }))
    .then((result) => {
      const ready = Boolean(result);
      if (ready && myToken === trackToken && useStatusStore().soundCheckEnabled && resolveCachedTrackSource) {
        void resolveCachedTrackSource(track).then((cached) => {
          if (!cached) return;
          if (cached.source?.startsWith("blob:")) URL.revokeObjectURL(cached.source);
          if (myToken === trackToken) warmActiveLoudness(cached.track || track, myToken);
        }).catch(() => {});
      }
      if (!ready && activeTrackCache?.token === myToken && activeTrackCache.key === key && activeTrackCache.controller === controller) {
        // Allow a later retry to create a fresh request after a transient
        // Telegram/provider failure instead of reusing a settled false.
        activeTrackCache = null;
      }
      return ready;
    })
    .catch((error) => {
      if (activeTrackCache?.token === myToken && activeTrackCache.key === key && activeTrackCache.controller === controller) {
        activeTrackCache = null;
      }
      if (error?.name !== "AbortError") console.warn("[player] active-track cache failed", error);
      return false;
    });
  activeTrackCache = { key, token: myToken, controller, promise };
  return promise;
}

/**
 * Warm the IndexedDB cache for the next queue item as soon as the current
 * source is ready. This does not move the play index or touch the audio
 * element, so the current stream remains uninterrupted.
 */
async function prefetchNextTrack(myToken = trackToken) {
  if (!prefetchTrackSource || myToken !== trackToken) return true;
  if (!canPrefetchWithoutStarvingPlayback()) return false;
  const { queue, status } = stores();
  if (status.repeatMode === "one") return true;

  // Do not start the next full-file request until the current song has been
  // completely persisted to IndexedDB. A large buffer alone is insufficient.
  const activeCacheReady = await ensureActiveTrackCache(myToken);
  if (myToken !== trackToken || !activeCacheReady) return false;

  const index = nextQueueIndex();
  if (index < 0 || index >= queue.queueLength) return true;
  const track = normalizeTrackForQuality(queue.getTrack(index));
  const key = prefetchKey(track);
  if (!track || !key || trackIdentity(track) === trackIdentity(status.currentTrack)) return true;
  replaceQueuedTrackWithQuality(index, track);
  if (activePrefetches.has(key)) return activePrefetches.get(key);

  const task = Promise.resolve()
    // Let the current <audio> element finish its own load before competing
    // for the connection, while still starting in the same playback turn.
    .then(() => new Promise((resolve) => window.setTimeout(resolve, 0)))
    .then(() => {
      if (myToken !== trackToken) return false;
      // Queue edits can happen during the cooperative yield above without
      // changing the current track token. Do not start the old next song
      // after an explicit next-up insertion or a reorder.
      if (status.repeatMode === "one" || prefetchKey(normalizeTrackForQuality(queue.getTrack(nextQueueIndex()))) !== key) return false;
      return prefetchTrackSource(track);
    })
    .then((ready) => {
      if (ready && myToken === trackToken) {
        void prepareTransition(myToken);
        void armNextTrack(myToken);
      }
      return ready;
    })
    .catch((error) => {
      console.warn("[player] next-track prefetch failed", error);
      return false;
    })
    .finally(() => activePrefetches.delete(key));
  activePrefetches.set(key, task);
  return task;
}

function schedulePrefetchNextTrack(myToken, { immediate = false } = {}) {
  clearPrefetchRetry();
  prefetchRetryAttempt = 0;
  // The ALAC worklet consumes the only active packet stream. Wait until that
  // reader reaches EOF, then persist the complete packet stream before the
  // next-track cache is allowed to start.
  if ((usingAlacWorklet && !alacPlayer?.eof) || !prefetchTrackSource || myToken !== trackToken) return;

  const attempt = () => {
    prefetchRetryTimer = 0;
    if (myToken !== trackToken || !prefetchTrackSource) return;
    // Respect the browser's data saver and very slow network hints. A full
    // next-track download would compete with the current stream and increase
    // the chance of an audible stall; a later `playing` event will schedule it
    // again when conditions improve.
    if (prefetchNetworkPolicy().disabled) return;
    if (canPrefetchWithoutStarvingPlayback()) {
      void prefetchNextTrack(myToken).then((ready) => {
        if (ready || myToken !== trackToken || !prefetchTrackSource) return;
        const retryIndex = Math.min(
          prefetchRetryAttempt,
          PREFETCH_RETRY_DELAYS_MS.length - 1,
        );
        prefetchRetryAttempt += 1;
        prefetchRetryTimer = window.setTimeout(attempt, PREFETCH_RETRY_DELAYS_MS[retryIndex]);
      });
      return;
    }
    // A slow stream should get the connection first. Re-check after the
    // buffer has had time to grow instead of starting a competing full-file
    // download immediately after playback begins.
    prefetchRetryTimer = window.setTimeout(attempt, PREFETCH_START_DELAY_MS);
  };

  if (immediate) attempt();
  else prefetchRetryTimer = window.setTimeout(attempt, PREFETCH_START_DELAY_MS);
}

export async function play(options = {}) {
  prepareAudio();
  const status = useStatusStore();
  if (pendingSeek?.token === trackToken) {
    pendingSeek.resume = true;
    status.state = "buffering";
    playback.setPlaying(false);
    applyPendingSeek();
    updateMediaSession();
    return true;
  }
  if (status.trackLoading && !options.duringLoad) {
    status.setNotice("正在准备音频，请稍候");
    return false;
  }
  if (!hasAudioSource()) {
    const track = status.currentTrack || useMediaStore().track;
    if (track && !status.trackLoading) {
      return Boolean(await loadTrack(track));
    }
    status.setNotice("请先选择一首音乐");
    return false;
  }
  const prev = status.state;
  const requestToken = loadToken;
  const controlToken = ++playControlToken;
  status.state = "buffering";
  playback.setPlaying(false);
  updateMediaSession();
  try {
    if (usingAlacWorklet && alacPlayer) {
      await alacPlayer.play();
      if (requestToken !== loadToken || controlToken !== playControlToken) return false;
      // The processor confirms actual rendered samples. During an underrun
      // the UI clock stays frozen instead of advancing through silence.
      // The native element is detached while the worklet owns playback. Its
      // currentTime is always zero, so syncing from it here would rewind the
      // progress bar immediately after a successful ALAC play() call.
      syncMediaSessionPosition();
      updateMediaSession();
      return true;
    }
    const nativeOutput = effectOutput(audio);
    // Start the authorized native element immediately. Waiting for Web Audio
    // first can leave Android's background media session without an active
    // transport while a suspended effects context waits to resume.
    const nativePlay = audio.play();
    const resumeOutput = async () => {
      if (!nativeOutput?.context?.resume || nativeOutput.context.state === "running") return;
      let resumeTimer;
      try {
        await Promise.race([
          nativeOutput.context.resume(),
          new Promise((_, reject) => {
            resumeTimer = window.setTimeout(() => reject(new DOMException("音频已就绪，点击播放以允许浏览器播放声音", "NotAllowedError")), 1500);
          }),
        ]);
      } finally { window.clearTimeout(resumeTimer); }
    };
    await Promise.all([nativePlay, resumeOutput()]);
    // A new track may have replaced this element while the old play request
    // was pending.  Never let the stale promise mutate the new track's state.
    if (requestToken !== loadToken || controlToken !== playControlToken) return false;
    // Browsers normally emit `playing` before resolving play().  Keep a small
    // fallback for WebKit versions that resolve first so an uncached track
    // cannot remain stuck in the buffering state forever.
    if (
      !audio.paused &&
      audio.readyState >= 3 &&
      status.state === "buffering" &&
      !waitingForAudioRecovery()
    ) {
      status.state = "playing";
      playback.setPlaying(true);
      syncPosition(true);
    }
    return true;
  } catch (error) {
    if (requestToken !== loadToken || controlToken !== playControlToken) return false;
    // The element and effects resume in parallel. If either fails, leave no
    // native transport running behind a paused UI (or through a silent graph).
    if (!usingAlacWorklet && !audio.paused) audio.pause();
    playback.setPlaying(false);
    if (error?.name === "NotAllowedError") {
      status.state = "paused";
      status.setNotice("音频已就绪，点击播放");
      return false;
    }
    if (error?.name === "AbortError") {
      status.state = prev === "playing" || prev === "buffering" ? "paused" : prev;
      return false;
    }
    if (error?.name === "NotSupportedError") {
      status.state = "paused";
      toast("浏览器暂时无法解码该音频流，请稍后重试");
      return false;
    }
    // A failed play request must never restore the optimistic buffering or
    // playing state. The media element is not producing audio at this point.
    status.state = "paused";
    toast(error?.message || "播放失败");
    return false;
  } finally {
    if (requestToken === loadToken && controlToken === playControlToken) updateMediaSession();
  }
}

export function togglePlay() {
  if (useStatusStore().isPlaying) pause();
  else void play();
}

export function pause() {
  cancelTransition();
  invalidateAutoplay();
  playControlToken += 1;
  if (!audio) return;
  const status = useStatusStore();
  if (cancelPendingLoad()) return;
  if (pendingSeek) pendingSeek.resume = false;
  else { seekGeneration += 1; playback.setSeeking(false); }
  status.state = "paused";
  recoveringAudio = false;
  recoveryReady = true;
  playback.setPlaying(false);
  updateMediaSession();
  syncMediaSessionPosition(true);
  if (usingAlacWorklet && alacPlayer) {
    alacPlayer.pause();
    return;
  }
  audio.pause();
}

// The system Stop action releases both audio engines while retaining the
// queue, so a later explicit Play can resolve the same song normally.
export function stop() {
  pause();
  loadToken += 1;
  trackToken += 1;
  disarmNext();
  clearPendingSeek();
  clearPrefetchRetry();
  cancelActiveTrackCache();
  detachCurrentAudio();
  const status = useStatusStore();
  status.state = "stopped";
  status.trackLoading = false;
  status.currentSource = null;
  status.currentSourceTrackId = null;
  status.position = 0;
  playback.reset();
  systemMedia?.clear();
  playbackLifecycle?.update();
}

export function destroyPlayer() {
  if (!initialized) return;
  stop();
  systemMedia?.dispose();
  playbackLifecycle?.dispose();
  systemMedia = playbackLifecycle = null;
  void alacPlayer?.destroy?.();
  alacPlayer = null;
  audio = null;
  initialized = false;
}

function clearPendingSeek() {
  pendingSeek?.controller?.abort();
  pendingSeek = null;
  seekGeneration += 1;
  seekTarget = null;
  playback.setSeeking(false);
}

function withinTimeRanges(ranges, target) {
  for (let index = 0; index < (ranges?.length || 0); index += 1) {
    if (target >= ranges.start(index) - 0.05 && target <= ranges.end(index) + 0.05) return true;
  }
  return false;
}

function finishNativeSeek(request) {
  if (pendingSeek !== request || request.token !== trackToken || !request.applied || audio?.seeking) return;
  const actual = Math.round(audio.currentTime * 1000);
  if (Math.abs(actual - request.target) >= 1000) {
    // A growing resource can clamp currentTime to the last available frame.
    // Retain the user's target until a later progress event makes it usable.
    request.applied = false;
    monitorNativeSeek(request);
    return;
  }
  pendingSeek = null;
  request.controller?.abort();
  seekTarget = null;
  playback.setCurrentTime(actual, { force: true });
  playback.setSeeking(false);
  const status = useStatusStore();
  status.position = actual;
  status.state = request.resume ? "buffering" : "paused";
  if (request.resume) void play({ duringLoad: true });
  syncMediaSessionPosition();
}

function monitorNativeSeek(request) {
  if (request.monitoring || Date.now() < (request.retryAt || 0) || !waitForCompletedSource || !request.track || request.track.status !== "streaming") return;
  request.monitoring = true;
  request.controller = new AbortController();
  void waitForCompletedSource(request.track, { signal: request.controller.signal }).then((source) => {
    if (pendingSeek !== request || request.token !== trackToken || request.controller.signal.aborted) return;
    if (!source) {
      request.monitoring = false;
      request.retryAt = Date.now() + 3000;
      useStatusStore().setNotice("目标位置尚未就绪，正在等待音频…");
      return;
    }
    // The completed native container exposes a real seek table/byte ranges.
    // Reattach only for an explicit unavailable seek; ordinary streaming is
    // never interrupted and no second complete download is started here.
    request.completed = true;
    request.applied = false;
    const status = useStatusStore();
    status.currentSource = source;
    audio.src = source;
    audio.load();
    applyPendingSeek();
  }).catch(() => {
    // Still listen to progress on the original stream after a failed poll.
    if (pendingSeek === request) { request.monitoring = false; request.retryAt = Date.now() + 3000; }
  });
}

function applyPendingSeek() {
  const request = pendingSeek;
  if (!request || request.token !== trackToken || request.applied) return;
  const status = useStatusStore();
  if (!status.currentSource) return;
  if (usingAlacWorklet) {
    if (!alacPlayer?.canSeek) return;
    pendingSeek = null;
    const decoder = alacPlayer;
    return decoder.seek(request.target).then(() => {
      if (request.id !== seekGeneration || request.token !== trackToken) return;
      playback.setSeeking(false);
      if (request.resume) return play({ duringLoad: true });
    }).catch((error) => {
      if (request.id !== seekGeneration || request.token !== trackToken) return;
      playback.setSeeking(false);
      if (error?.name !== "AbortError") toast(error?.message || "跳转失败");
    }).finally(() => {
      if (request.id === seekGeneration && request.token === trackToken) refreshTransitionPreparation();
    });
  }
  if (!audio || audio.readyState < 1) return;
  const knownDuration = Number.isFinite(audio.duration) ? Math.round(audio.duration * 1000) : 0;
  if (request.completed && knownDuration > 0) request.target = Math.min(request.target, knownDuration);
  const seconds = request.target / 1000;
  if (request.track?.status === "streaming" && !request.completed &&
      !withinTimeRanges(audio.seekable, seconds) && !withinTimeRanges(audio.buffered, seconds)) {
    monitorNativeSeek(request);
    return;
  }
  try {
    request.applied = true;
    seekTarget = request.target;
    audio.currentTime = seconds;
    // Same-position seeks need not emit seeked. Real async seeks finish from
    // that event; this also handles immediate, already cached positioning.
    queueMicrotask(() => finishNativeSeek(request));
  } catch {
    request.applied = false;
    monitorNativeSeek(request);
  }
}

export function seek(posMs) {
  cancelTransition();
  const { status, media } = stores();
  if (!media.track) return;
  const duration = status.duration || media.track.duration || 0;
  const safe = Math.min(Number.isFinite(posMs) ? Math.max(0, posMs) : 0, duration > 0 ? duration : Infinity);
  if (recoveringAudio) recoveryPosition = safe;
  const resume = pendingSeek?.resume ?? (status.isPlaying || (status.trackLoading && loadWantsPlay));
  clearPendingSeek();
  // Invalidate any play() still resuming an AudioContext for the old time.
  playControlToken += 1;
  pendingSeek = { id: seekGeneration, token: trackToken, target: safe, resume,
    track: media.track, applied: false, completed: false, monitoring: false };
  playback.setPlaying(false);
  playback.setSeeking(true);
  playback.setCurrentTime(safe, { force: true });
  status.position = safe;
  status.state = resume ? "buffering" : "paused";
  status.setNotice("正在跳转到所选位置…");
  if (!usingAlacWorklet) audio?.pause();
  syncMediaSessionPosition();
  updateMediaSession();
  return applyPendingSeek();
}

export function hasReachedSeekTarget(position) {
  if (seekTarget === null) return true;
  if (Math.abs(position - seekTarget) < 1000) {
    seekTarget = null;
    playback.setSeeking(false);
    return true;
  }
  return false;
}

export function setVolume(volume) {
  const status = useStatusStore();
  const safe = Number.isFinite(volume) ? Math.max(0, Math.min(1, volume)) : 1;
  status.volume = safe;
  applyActiveLevels();
  retiringDeck?.output?.setVolume?.(safe);
  transitionCandidate?.output?.setVolume?.(safe);
  status.persist();
}

export async function playFrom(items, startIndex = 0, sourcePlaylist = null, { keepStart = false } = {}) {
  cancelTransition();
  if (!items.length) return false;
  invalidateAutoplay();
  resetAutoplayStation();
  const { media, queue, status } = stores();
  const normalizedItems = items.map((item) => ({ ...normalizeTrackForQuality(item), autoplayRecommended: false }));
  const index = Math.max(0, Math.min(startIndex, normalizedItems.length - 1));
  queue.setQueue(normalizedItems, sourcePlaylist);
  status.playIndex = queue.findTrackIndex(normalizedItems[index]);
  if (status.playIndex === -1) status.playIndex = 0;
  if (status.shuffleMode === "on") {
    // Starting a playlist while shuffle is enabled should choose a random
    // first item as well. Keeping the requested first track here made
    // "随机播放" deterministic whenever callers passed startIndex = 0.
    // Continuing a specific song (hand-off) keeps it and shuffles the rest.
    const firstIndex = keepStart ? status.playIndex : Math.floor(Math.random() * queue.queueLength);
    queue.shuffleQueue(firstIndex);
    status.playIndex = 0;
  }
  // The queue may have been shuffled above, so compare against the final
  // active item rather than the caller's pre-shuffle start index.
  const sameTrack = media.track?.id === status.currentTrack?.id;
  status.persist();
  if (sameTrack) {
    if (!status.currentSource || !hasAudioSource()) await loadTrack(status.currentTrack);
    else if (!status.isPlaying) await play();
    prepareAutoplayRecommendations();
    refreshTransitionPreparation();
  } else {
    await loadTrack(status.currentTrack);
  }
  return true;
}

export async function playAtIndex(index) {
  const { media, queue, status } = stores();
  if (index < 0 || index >= queue.queueLength) return false;
  if (index === status.playIndex) {
    const track = replaceQueuedTrackWithQuality(index, normalizeTrackForQuality(status.currentTrack));
    if (track && !media.track) media.setTrack(track);
    const sameQuality = media.track?.quality === track?.quality;
    if (track && (!status.currentSource || !hasAudioSource() || !sameQuality)) return await loadTrack(track);
    else if (!status.isPlaying && track) return await play();
    return Boolean(track);
  }
  status.playIndex = index;
  status.persist();
  return await loadTrack(normalizeTrackForQuality(status.currentTrack));
}

export async function prepareAtIndex(index) {
  const { media, queue, status } = stores();
  if (index < 0 || index >= queue.queueLength) return false;
  status.playIndex = index;
  status.persist();
  const track = replaceQueuedTrackWithQuality(index, normalizeTrackForQuality(status.currentTrack));
  if (!track) return false;
  if (status.currentSourceTrackId === track.id && status.currentSource && hasAudioSource() && media.track?.quality === track.quality) return true;
  return await loadTrack(track, false);
}

export async function prevTrack() {
  const { queue, status } = stores();
  if (queue.queueLength === 0) return;
  status.playIndex = status.playIndex > 0 ? status.playIndex - 1 : queue.queueLength - 1;
  status.persist();
  await loadTrack(status.currentTrack);
}

export async function nextTrack(manual = false) {
  const { queue, status } = stores();
  if (queue.queueLength === 0) return;
  if (autoplayAdvance) return autoplayAdvance;
  if (status.playIndex >= queue.queueLength - 1) {
    if (status.autoplayEnabled) {
      const generation = autoplayGeneration;
      const currentKey = trackIdentity(status.currentTrack);
      playback.setPlaying(false);
      if (usingAlacWorklet) alacPlayer?.pause?.();
      else audio?.pause();
      status.state = "buffering";
      const advance = (async () => {
        await ensureAutoplayRecommendations();
        if (generation !== autoplayGeneration || !status.autoplayEnabled || trackIdentity(status.currentTrack) !== currentKey) return false;
        // Manual queue edits made while the request was pending take priority;
        // the next item is resolved from the current queue, never a stale index.
        if (status.playIndex < queue.queueLength - 1) {
          status.playIndex += 1;
          status.persist();
          return loadTrack(status.currentTrack);
        }
        await onQueueEnded();
        toast(status.autoplayError || "暂时没有更多新歌曲，可以重试");
        return false;
      })();
      autoplayAdvance = advance;
      try { return await advance; }
      finally { if (autoplayAdvance === advance) autoplayAdvance = null; }
    }
    if (status.repeatMode === "list" || status.repeatMode === "one" || manual) {
      if (status.shuffleMode === "on" && queue.queueLength > 1) {
        queue.shuffleQueue(status.playIndex);
        status.playIndex = 1;
      } else {
        status.playIndex = 0;
      }
    } else {
      await onQueueEnded();
      return;
    }
  } else {
    status.playIndex += 1;
  }
  status.persist();
  await loadTrack(status.currentTrack);
}

async function onQueueEnded() {
  cancelTransition();
  clearPendingSeek();
  const status = useStatusStore();
  playback.setPlaying(false);
  playback.reset();
  if (usingAlacWorklet) alacPlayer?.pause?.();
  else if (audio) audio.pause();
  status.state = "stopped";
  status.position = status.duration;
  recoveringAudio = false;
  systemMedia?.clear();
  playbackLifecycle?.update();
}

export function cycleRepeatMode() {
  cancelTransition();
  const status = useStatusStore();
  const cycle = ["list", "one", "off"];
  status.repeatMode = cycle[(cycle.indexOf(status.repeatMode) + 1) % cycle.length];
  if (status.repeatMode !== "off") {
    setAutoplayEnabled(false);
    status.shuffleMode = "off";
  }
  status.persist();
  refreshTransitionPreparation();
}

export function toggleShuffleMode() {
  cancelTransition();
  const { queue, status } = stores();
  if (status.shuffleMode === "on") {
    const track = status.currentTrack;
    status.playIndex = queue.unshuffleQueue(track, { recommendationsLast: status.autoplayEnabled });
    status.shuffleMode = "off";
  } else {
    queue.shuffleQueue(status.playIndex, { recommendationsLast: status.autoplayEnabled });
    status.shuffleMode = "on";
    status.playIndex = 0;
  }
  if (status.autoplayEnabled) status.repeatMode = "off";
  status.persist();
  refreshTransitionPreparation();
}

export function playbackMode() {
  const status = useStatusStore();
  if (status.shuffleMode === "on") return "shuffle";
  if (status.repeatMode === "one") return "one";
  if (status.repeatMode === "list") return "list";
  return "sequence";
}

export function setPlaybackMode(mode) {
  cancelTransition();
  const status = useStatusStore();
  if (mode === "shuffle") {
    status.shuffleMode = "on";
    status.repeatMode = status.autoplayEnabled ? "off" : "list";
    const queue = useQueueStore();
    queue.shuffleQueue(status.playIndex, { recommendationsLast: status.autoplayEnabled });
    status.playIndex = 0;
  } else {
    if (mode === "one" || mode === "list") setAutoplayEnabled(false);
    if (status.shuffleMode === "on") {
      const queue = useQueueStore();
      status.playIndex = queue.unshuffleQueue(status.currentTrack, { recommendationsLast: status.autoplayEnabled });
    }
    status.shuffleMode = "off";
    status.repeatMode = mode === "one" || mode === "list" ? mode : "off";
  }
  status.persist();
  refreshTransitionPreparation();
}

export function insertToQueue(item, afterIndex = useStatusStore().playIndex + 1) {
  cancelTransition();
  const { queue, status } = stores();
  item = { ...item, autoplayRecommended: false };
  const existing = queue.findTrackIndex(item);
  if (existing !== -1) {
    queue.replaceTrack(existing, item);
    moveInQueue(existing, Math.max(0, Math.min(afterIndex, queue.queueLength - 1)));
    return queue.findTrackIndex(item);
  }
  const safeAt = Math.max(0, Math.min(afterIndex, queue.queueLength));
  queue.insertToQueue(item, safeAt);
  if (safeAt <= status.playIndex) status.playIndex += 1;
  status.persist();
  refreshTransitionPreparation();
  return safeAt;
}

export function insertManyToQueue(items, afterIndex = useStatusStore().playIndex + 1) {
  cancelTransition();
  const { queue, status } = stores();
  let insertion = Math.max(0, Math.min(afterIndex, queue.queueLength));
  // User additions belong before the automatically generated tail.
  const firstAutomatic = queue.queue.findIndex((item, index) => index > status.playIndex && item.autoplayRecommended);
  if (firstAutomatic !== -1) insertion = Math.min(insertion, firstAutomatic);
  let added = 0;
  for (const item of items || []) {
    if (!item) continue;
    const existing = queue.findTrackIndex(item);
    if (existing !== -1) {
      if (existing > status.playIndex && queue.getTrack(existing).autoplayRecommended) {
        insertToQueue(item, insertion);
        insertion += 1;
        added += 1;
      }
      continue;
    }
    queue.insertToQueue({ ...item, autoplayRecommended: false }, insertion);
    if (insertion <= status.playIndex) status.playIndex += 1;
    insertion += 1;
    added += 1;
  }
  if (added) status.persist();
  refreshTransitionPreparation();
  return added;
}

export function queueNextMany(items) {
  cancelTransition();
  const { queue, status } = stores();
  const currentTrack = status.currentTrack;
  const normalizedItems = (items || []).filter(Boolean).map((item) => ({ ...normalizeTrackForQuality(item), autoplayRecommended: false }));
  const { count, currentIndex } = queue.queueNextMany(normalizedItems, currentTrack);
  if (!count) return 0;
  status.playIndex = currentIndex;
  if (status.repeatMode === "one") status.repeatMode = "list";
  status.persist();
  // Keep the current transport, progress and cache gate intact. A pending
  // next-song timer must now target the newly arranged next item instead.
  clearPrefetchRetry();
  if (currentTrack && hasAudioSource()) schedulePrefetchNextTrack(trackToken);
  return count;
}

export async function playNow(item) {
  const { status } = stores();
  item = normalizeTrackForQuality(item);
  if (status.currentSourceTrackId === item.id && status.currentSource && hasAudioSource() && useMediaStore().track?.quality === item.quality) {
    if (!status.isPlaying) await play();
    return true;
  }
  status.playIndex = insertToQueue(item);
  status.persist();
  return await loadTrack(item);
}

export async function prepareNow(item) {
  const { status } = stores();
  item = normalizeTrackForQuality(item);
  if (status.currentSourceTrackId === item.id && status.currentSource && hasAudioSource() && useMediaStore().track?.quality === item.quality) return true;
  status.playIndex = insertToQueue(item);
  status.persist();
  return await loadTrack(item, false);
}

export function moveInQueue(fromIndex, toIndex) {
  cancelTransition();
  const { queue, status } = stores();
  const moved = queue.getTrack(fromIndex);
  if (moved?.autoplayRecommended) queue.replaceTrack(fromIndex, { ...moved, autoplayRecommended: false });
  queue.moveInQueue(fromIndex, toIndex);
  if (status.playIndex === fromIndex) status.playIndex = toIndex;
  else if (fromIndex < status.playIndex && toIndex >= status.playIndex) status.playIndex -= 1;
  else if (fromIndex > status.playIndex && toIndex <= status.playIndex) status.playIndex += 1;
  status.persist();
  refreshTransitionPreparation();
}

export async function removeFromQueue(index) {
  cancelTransition();
  const { queue, status } = stores();
  if (index < 0 || index >= queue.queueLength) return;
  const isCurrent = index === status.playIndex;
  if (queue.getTrack(index)?.autoplayRecommended) rememberAutoplayTrack(queue.getTrack(index));
  queue.removeFromQueue(index);
  if (index < status.playIndex) status.playIndex -= 1;
  else if (isCurrent) {
    if (queue.queueLength === 0) {
      await clearQueue();
      return;
    }
    if (status.playIndex >= queue.queueLength) status.playIndex = 0;
    await loadTrack(status.currentTrack);
  }
  status.persist();
  refreshTransitionPreparation();
}

export async function clearQueue() {
  const { queue, status, media } = stores();
  invalidateAutoplay();
  resetAutoplayStation();
  loadToken += 1;
  trackToken += 1;
  disarmNext();
  clearPendingSeek();
  clearPrefetchRetry();
  cancelActiveTrackCache();
  detachCurrentAudio();
  queue.clearQueue();
  status.playIndex = -1;
  status.trackLoading = false;
  status.currentSource = null;
  status.currentSourceTrackId = null;
  media.setTrack(null);
  media.clearLyric();
  status.persist();
  await onQueueEnded();
}

export function tickPlaybackTime() {
  checkTransition();
  // A hidden page shows no clock. Media events keep the store and Media
  // Session current, so skip these 4 Hz reactive updates in the background.
  if (typeof document !== "undefined" && document.hidden) return;
  const status = useStatusStore();
  const nextPosition = Math.round(playback.getCurrentTime());
  const nextDuration = Math.round(playback.getDuration());
  if (nextPosition !== status.position) status.position = nextPosition;
  if (nextDuration !== status.duration) status.duration = nextDuration;
}
