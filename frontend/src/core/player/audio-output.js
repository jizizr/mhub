// Keep the user-authorized contexts alive across decks and queue changes.
// Separate rates preserve Hi-Res output when the quality changes mid-song;
// two decks at the same quality always share a render clock.
const contexts = new Map();
const nativeSources = new WeakMap();
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

export function getOutputContext(qualityOrSampleRate = 48000) {
  const rate = requestedRate(qualityOrSampleRate);
  let context = contexts.get(rate);
  if (context && context.state !== "closed") return context;
  const Context = typeof window !== "undefined" && (window.AudioContext || window.webkitAudioContext);
  if (!Context) throw new Error("当前浏览器不支持音频处理");
  try { context = new Context({ sampleRate: rate }); }
  catch { context = new Context(); }
  contexts.set(rate, context);
  return context;
}

// Call synchronously inside a trusted user event. Resume is deliberately not
// awaited: native/ALAC playback retains its own recoverable permission check.
export function prepareOutput(qualityOrSampleRate = 48000) {
  preparePlaybackSession();
  preparedContext = getOutputContext(qualityOrSampleRate);
  try { void Promise.resolve(preparedContext.resume()).catch(() => {}); } catch { /* retry on play */ }
  return preparedContext;
}

function safeGain(value, fallback = 1) {
  const number = Number(value);
  return Number.isFinite(number) ? Math.max(0, number) : fallback;
}

function unitGain(value) { return Math.min(1, safeGain(value)); }

/** Independent normalization, transition and user volume for one deck. */
export function createAlacOutput(context = preparedContext || getOutputContext()) {
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
