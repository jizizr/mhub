// AMLL plays every word's mask sweep and float as Web Animations for the whole
// active line. Mask position is not compositor-accelerated, so the browser
// restyles and repaints those words at the display rate (120 Hz and above on
// ProMotion and gaming screens), which made word-timed lyrics the largest GPU
// cost of the full player. Each running animation sits at a fixed offset from
// the playback clock, so pause it and seek it ourselves at a capped rate.
// Reversed animations (AMLL settling a finished line) keep running natively.

export function createAnimationStepper(getRoot, { interval = 1000 / 30 } = {}) {
  const owned = new Map();
  let lastStep = -Infinity;

  function endOf(animation) {
    const timing = animation.effect?.getComputedTiming?.();
    return Number(timing?.endTime ?? Infinity);
  }

  function step(playbackTime, frameTime) {
    if (frameTime - lastStep < interval) return;
    lastStep = frameTime;
    for (const [animation, offset] of owned) {
      if (animation.playState === "idle" || animation.playbackRate !== 1 || animation.playState === "running") {
        // Cancelled, reversed, or restarted by AMLL (seek, resume): let go;
        // a restarted forward animation is captured again below.
        owned.delete(animation);
        continue;
      }
      const end = endOf(animation);
      const time = Math.max(0, playbackTime + offset);
      animation.currentTime = Math.min(time, end);
      if (time >= end) owned.delete(animation);
    }
    const root = getRoot();
    if (!root?.getAnimations) return;
    for (const animation of root.getAnimations({ subtree: true })) {
      if (animation.playState !== "running" || animation.playbackRate !== 1 || owned.has(animation)) continue;
      owned.set(animation, (animation.currentTime ?? 0) - playbackTime);
      animation.pause();
    }
  }

  // Hand the animations back, e.g. when playback pauses: AMLL pauses and
  // resumes them itself, and resumed ones are captured again.
  function release() {
    owned.clear();
    lastStep = -Infinity;
  }

  return { step, release, get size() { return owned.size; } };
}
