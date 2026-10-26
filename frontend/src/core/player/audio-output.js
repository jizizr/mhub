// Keep the user-authorized contexts alive across decks and queue changes.
// Separate rates preserve Hi-Res output when the quality changes mid-song;
// two decks at the same quality always share a render clock.
const contexts = new Map();
const nativeSources = new WeakMap();
// Contexts that must keep running: native <audio> graphs and live ALAC decks.
const nativeContexts = new WeakSet();
const deckUsers = new Map();
let preparedContext = null;

// WebKit uses this category both for background audio and Now Playing
// eligibility. It is an optional playback API, not notification permission.
export function preparePlaybackSession() {
  try {
    const session = globalThis.navigator?.audioSession;
    if (session && session.type !== "playback") session.type = "playback";
  } catch { /* Unsupported/embedded browsers keep their normal media path. */ }
}

function requestedRate(quality) {
  if (typeof quality === "number") return quality > 48000 ? 192000 : 48000;
  return ["hires", "hi-res"].includes(String(quality || "").toLowerCase()) ? 192000 : 48000;
}

/**
 * The render rate for a source: 44.1/48 kHz songs play on the 48 kHz context
 * even when Hi-Res is selected, instead of being upsampled to 192 kHz and
 * then resampled again by the browser; only real Hi-Res sources use 192 kHz.
 */
export function outputRateFor(sourceRate) {
  return Number(sourceRate) > 48000 ? 192000 : 48000;
}

export function getOutputContext(qualityOrSampleRate = 48000) {
  const rate = requestedRate(qualityOrSampleRate);
  let context = contexts.get(rate);
  if (context && context.state !== "closed") return context;
  const Context = typeof window !== "undefined" && (window.AudioContext || window.webkitAudioContext);
  if (!Context) throw new Error("当前浏览器不支持音频处理");
  try { context = new Context({ sampleRate: rate }); }
  catch {
    // Browsers that reject the rate share one default-rate context.
    context = [...contexts.values()].find((existing) => existing.state !== "closed" && existing.defaultRate) || new Context();
    context.defaultRate = true;
  }
  contexts.set(rate, context);
  return context;
}

// Call synchronously inside a trusted user event. Resume is deliberately not
// awaited: native/ALAC playback retains its own recoverable permission check.
export function prepareOutput(qualityOrSampleRate = 48000) {
  preparePlaybackSession();
  const rate = requestedRate(qualityOrSampleRate);
  // Hi-Res queues usually mix 44.1/48 kHz and higher-rate songs. Unlock both
  // rates in this gesture so each song can use its own (see outputRateFor);
  // the unused one is suspended again once a deck picks (suspendIdleOutputs).
  const rates = rate === 192000 ? [48000, 192000] : [rate];
  for (const each of rates) {
    const context = getOutputContext(each);
    try { void Promise.resolve(context.resume()).catch(() => {}); } catch { /* retry on play */ }
    if (each === rate) preparedContext = context;
  }
  return preparedContext;
}

const running = (context) => context && context.state === "running";

/**
 * A running context for a decoded source rate. A context suspended to save
 * power is resumed; if that fails outside a user gesture (WebKit), playback
 * falls back to a context that is already running rather than stalling.
 */
export async function acquireOutputContext(sourceRate, fallbacks = []) {
  const preferred = contexts.get(outputRateFor(sourceRate));
  if (preferred && preferred.state !== "closed" && !running(preferred)) {
    try {
      await Promise.race([preferred.resume(), new Promise((resolve) => setTimeout(resolve, 400))]);
    } catch { /* fall back below */ }
  }
  const open = (context) => context && context.state !== "closed";
  const candidates = [preferred, ...fallbacks, preparedContext, ...contexts.values()].filter(open);
  // Nothing confirmed running: keep the gesture-prepared context, as before.
  return candidates.find(running) || fallbacks.find(open) || (open(preparedContext) && preparedContext) || candidates[0] || getOutputContext(sourceRate);
}

/** Suspend unlocked contexts nothing renders on (an idle 192 kHz clock costs power). */
export function suspendIdleOutputs(keep) {
  for (const context of contexts.values()) {
    if (context === keep || !running(context) || nativeContexts.has(context) || (deckUsers.get(context) || 0) > 0) continue;
    try { void Promise.resolve(context.suspend()).catch(() => {}); } catch { /* optional */ }
  }
}

function safeGain(value, fallback = 1) {
  const number = Number(value);
  return Number.isFinite(number) ? Math.max(0, number) : fallback;
}

function unitGain(value) { return Math.min(1, safeGain(value)); }

/** Independent normalization, transition and user volume for one deck. */
export function createAlacOutput(context = preparedContext || getOutputContext()) {
  deckUsers.set(context, (deckUsers.get(context) || 0) + 1);
  const normalizationGain = context.createGain();
  const fadeGain = context.createGain();
  const volumeGain = context.createGain();
  normalizationGain.connect(fadeGain);
  fadeGain.connect(volumeGain);
  volumeGain.connect(context.destination);
  let disposed = false;
  let volumeInitialized = false;
  let loudnessInitialized = false;
  let fade = { from: 1, to: 1, start: context.currentTime, end: context.currentTime };
  const now = () => Number(context.currentTime) || 0;
  const getFadeLevel = (at = now()) => {
    if (at < fade.start) return fade.from;
    if (at >= fade.end || fade.end <= fade.start) return fade.to;
    return fade.from + (fade.to - fade.from) * ((at - fade.start) / (fade.end - fade.start));
  };
  const hold = (parameter, value, at) => {
    parameter.cancelScheduledValues(at);
    parameter.setValueAtTime(value, at);
  };
  const cancelFade = (value) => {
    const at = now();
    const level = value === undefined ? getFadeLevel(at) : unitGain(value);
    if (!disposed) hold(fadeGain.gain, level, at);
    fade = { from: level, to: level, start: at, end: at };
    return level;
  };
  return {
    context,
    input: normalizationGain,
    normalizationGain,
    fadeGain,
    volumeGain,
    get disposed() { return disposed; },
    setVolume(value) {
      if (disposed) return;
      const parameter = volumeGain.gain;
      parameter.cancelScheduledValues(now());
      if (volumeInitialized) parameter.setTargetAtTime(unitGain(value), now(), 0.01);
      else parameter.setValueAtTime(unitGain(value), now());
      volumeInitialized = true;
    },
    setLoudnessGain(value) {
      if (disposed) return;
      const parameter = normalizationGain.gain;
      parameter.cancelScheduledValues(now());
      if (loudnessInitialized) parameter.setTargetAtTime(safeGain(value), now(), 0.05);
      else parameter.setValueAtTime(safeGain(value), now());
      loudnessInitialized = true;
    },
    getFadeLevel,
    cancelFade,
    fadeTo(target, durationSeconds = 0, { at } = {}) {
      if (disposed) return;
      const from = cancelFade();
      const start = Number.isFinite(at) ? Math.max(now(), at) : now();
      const requestedDuration = Number(durationSeconds);
      const duration = Number.isFinite(requestedDuration) ? Math.max(0, requestedDuration) : 0;
      const to = unitGain(target);
      fade = { from, to, start, end: start + duration };
      fadeGain.gain.setValueAtTime(from, start);
      if (duration > 0) fadeGain.gain.linearRampToValueAtTime(to, fade.end);
      else fadeGain.gain.setValueAtTime(to, start);
    },
    dispose() {
      if (disposed) return;
      cancelFade(0);
      disposed = true;
      deckUsers.set(context, Math.max(0, (deckUsers.get(context) || 1) - 1));
      normalizationGain.disconnect();
      fadeGain.disconnect();
      volumeGain.disconnect();
      // The shared, user-authorized Context belongs to the output module.
    },
  };
}

/**
 * The caller checks same-origin/blob eligibility before connecting a native
 * element. Once connected, disabling effects must keep this graph at unity;
 * an element cannot be detached back to its original native destination.
 */
export function createNativeOutput(audioElement) {
  if (!audioElement) throw new Error("音频元素不可用");
  let entry = nativeSources.get(audioElement);
  if (entry?.output && !entry.output.disposed) return entry.output;
  if (!entry) {
    const context = preparedContext || getOutputContext();
    entry = { context, source: context.createMediaElementSource(audioElement), output: null };
    nativeContexts.add(context);
    nativeSources.set(audioElement, entry);
  }
  const output = createAlacOutput(entry.context);
  const setVolume = output.setVolume;
  output.setVolume = (volume) => {
    // All attenuation belongs to the graph, including on iOS where changing
    // HTMLMediaElement.volume is not a reliable way to implement fades.
    audioElement.volume = 1;
    setVolume(volume);
  };
  const dispose = output.dispose;
  output.dispose = () => {
    if (output.disposed) return;
    entry.source.disconnect(output.input);
    dispose();
  };
  // Preserve the current level before attaching the graph: enabling an
  // effect while already playing must not briefly restore full volume.
  output.setVolume(audioElement.volume);
  entry.source.connect(output.input);
  entry.output = output;
  return output;
}
