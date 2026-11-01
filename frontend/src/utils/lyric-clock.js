// The lyric clock trails the audio clock by a fixed delay so each word lights
// just after it is heard. That delay must only apply to continuous playback:
// right after a seek (a lyric tap or the progress bar) the trailing time is
// still before the target, so the previous line stayed current and the tapped
// line kept its blur until the delay had passed. A seek therefore sets a floor
// at its target, held until the trailing clock catches up with it.

// A jump forward larger than the elapsed wall time allows, or a step back
// beyond clock-sync jitter, is a seek rather than playback.
const JUMP_SLACK_MS = 500;
const BACK_SLACK_MS = 100;
const MAX_RATE = 2;

export function createLyricClock(getAudioTime, getDelay, now = () => performance.now()) {
  let floor = null;
  let lastAudio = null;
  let lastWall = 0;

  function seekTo(ms) {
    if (Number.isFinite(ms)) floor = Math.max(0, ms);
  }

  function time() {
    const audio = getAudioTime();
    const wall = now();
    if (lastAudio !== null) {
      const ahead = audio - lastAudio;
      if (ahead < -BACK_SLACK_MS || ahead > (wall - lastWall) * MAX_RATE + JUMP_SLACK_MS) floor = audio;
    }
    lastAudio = audio;
    lastWall = wall;
    const delayed = Math.max(0, audio - getDelay());
    if (floor === null) return delayed;
    if (delayed >= floor) {
      floor = null;
      return delayed;
    }
    return floor;
  }

  return { time, seekTo };
}
