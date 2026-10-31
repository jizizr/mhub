let currentTimeMs = 0;
let totalDurationMs = 0;
let lastSyncAt = 0;
let playing = false;
let seeking = false;
let speed = 1.0;

const SYNC_TOLERANCE_MS = 1000;

export function getCurrentTime() {
  if (!playing || seeking) return currentTimeMs;
  const elapsed = performance.now() - lastSyncAt;
  return Math.min(currentTimeMs + elapsed * speed, totalDurationMs || Number.MAX_SAFE_INTEGER);
}

export function getDuration() {
  return totalDurationMs;
}

export function isPlaying() {
  return playing;
}

export function setCurrentTime(ms, options = {}) {
  const safe = Number.isFinite(ms) ? Math.max(0, ms) : 0;
  if (!options.force && !seeking && totalDurationMs > 0) {
    const interpolated = getCurrentTime();
    if (Math.abs(interpolated - safe) < SYNC_TOLERANCE_MS) {
      currentTimeMs = interpolated;
      lastSyncAt = performance.now();
      return interpolated;
    }
  }
  currentTimeMs = safe;
  lastSyncAt = performance.now();
  return safe;
}

export function setSeeking(value) {
  seeking = value;
  if (!value) lastSyncAt = performance.now();
}

export function setDuration(ms) {
  totalDurationMs = Number.isFinite(ms) ? Math.max(0, ms) : 0;
}

export function setPlaying(value) {
  if (value === playing) return;
  if (!value) currentTimeMs = getCurrentTime();
  lastSyncAt = performance.now();
  playing = value;
}

export function setSpeed(value) {
  const safe = Number.isFinite(value) ? Math.max(0.5, Math.min(2.0, value)) : 1.0;
  if (safe === speed) return;
  if (playing && !seeking) {
    currentTimeMs = getCurrentTime();
    lastSyncAt = performance.now();
  }
  speed = safe;
}

// How long rendered audio takes to reach the speakers. Native <audio>
// currentTime already reports what is being heard; the ALAC player reports
// what it has rendered, so the player sets the context's output latency here.
let outputDelayMs = 0;

export function setOutputDelay(ms) {
  outputDelayMs = Number.isFinite(ms) ? Math.min(400, Math.max(0, ms)) : 0;
}

export function getOutputDelay() {
  return outputDelayMs;
}

export function reset() {
  currentTimeMs = 0;
  totalDurationMs = 0;
  lastSyncAt = 0;
  playing = false;
  seeking = false;
  speed = 1.0;
}
