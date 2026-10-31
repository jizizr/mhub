/**
 * Detect an AudioContext whose render clock no longer keeps wall time while
 * it reports "running" (for example Chromium's AudioWorklet bypass-buffering
 * fault, which renders every other device buffer). Every song on such a
 * context plays slowly or stalls and seeking cannot help, only a new context
 * does. Sample from a foreground timer; a throttled or sleeping page restarts
 * the measurement instead of reporting drift.
 */
export function createRenderClockWatch({ windowMs = 3000, tolerance = 0.1, strikes = 2, now = () => performance.now() } = {}) {
  let context = null;
  let baseline = null;
  let misses = 0;
  let warmed = false;
  function reset(next = null) {
    context = next;
    baseline = null;
    misses = 0;
    warmed = false;
  }
  return {
    reset,
    /** True once the clock has drifted for `strikes` consecutive windows. */
    sample(next) {
      if (!next || next.state !== "running") { reset(); return false; }
      if (next !== context) reset(next);
      const wall = now();
      const clock = Number(next.currentTime) || 0;
      if (!baseline) { baseline = { wall, clock }; return false; }
      const elapsed = wall - baseline.wall;
      if (elapsed < windowMs) return false;
      const rate = (clock - baseline.clock) * 1000 / elapsed;
      baseline = { wall, clock };
      if (elapsed > windowMs * 3) { misses = 0; return false; }
      // A freshly resumed clock starts late (about 0.8 over its first window
      // in Chromium); judge only from the second window on.
      if (!warmed) { warmed = true; return false; }
      misses = Math.abs(rate - 1) > tolerance ? misses + 1 : 0;
      if (misses < strikes) return false;
      reset();
      return true;
    },
  };
}
