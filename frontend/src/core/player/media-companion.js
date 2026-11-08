// Android's system media notification belongs to a playing media element.
// Web Audio (the WASM ALAC decoder's output) is not one, and Media Session
// metadata alone never creates the notification. While the user wants music,
// a looping element plays real silence beside whatever produces the sound, so
// the session always has a player: across WASM songs, across the loading gap
// before an uncached next song, and across native/WASM hand-offs.
//
// Chromium treats media shorter than 5 s as transient (no notification), and
// a player reaching EOS leaves the session, so the clip is long and loops.
// It plays unmuted at full volume: its samples are zero, and the session
// must not depend on how a browser treats muted players.

const SAMPLE_RATE = 8000;
const SILENCE_SECONDS = 60;

/** A mono 8-bit PCM WAV of `seconds` of silence (0x80 is the zero level). */
export function silentWav(seconds = SILENCE_SECONDS, sampleRate = SAMPLE_RATE) {
  const samples = Math.max(1, Math.round(seconds * sampleRate));
  const bytes = new Uint8Array(44 + samples);
  const view = new DataView(bytes.buffer);
  const ascii = (offset, text) => { for (let i = 0; i < text.length; i += 1) bytes[offset + i] = text.charCodeAt(i); };
  ascii(0, "RIFF");
  view.setUint32(4, 36 + samples, true);
  ascii(8, "WAVE");
  ascii(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate, true); // byte rate
  view.setUint16(32, 1, true); // block align
  view.setUint16(34, 8, true); // bits per sample
  ascii(36, "data");
  view.setUint32(40, samples, true);
  bytes.fill(0x80, 44);
  return bytes;
}

export function createMediaCompanion({
  createElement = () => globalThis.document?.createElement?.("audio"),
  createSourceUrl = () => URL.createObjectURL(new Blob([silentWav()], { type: "audio/wav" })),
  onExternalPause = () => {},
  onExternalPlay = () => {},
} = {}) {
  let element = null;
  let wanted = false;
  // Set when the browser paused the clip itself (audio focus loss, a call):
  // its own resume afterwards is passed on to the player.
  let pausedByBrowser = false;
  let disposed = false;

  function ensureElement() {
    if (element || disposed) return element;
    try {
      element = createElement();
      if (!element) return null;
      element.loop = true;
      element.preload = "auto";
      element.setAttribute?.("playsinline", "");
      element.addEventListener?.("pause", () => {
        // Our own pause, a loop restart and an ended clip are not the
        // browser taking the session away.
        if (!wanted || element.ended) return;
        pausedByBrowser = true;
        onExternalPause();
      });
      element.addEventListener?.("play", () => {
        if (!pausedByBrowser) return;
        pausedByBrowser = false;
        if (!wanted) onExternalPlay();
      });
      element.src = createSourceUrl();
    } catch {
      element = null;
    }
    return element;
  }

  /** Plays the clip while `wantsPlay`, pauses it otherwise. Idempotent. */
  function sync(wantsPlay) {
    if (disposed) return;
    const target = Boolean(wantsPlay);
    if (target === wanted && (!target || (element && !element.paused))) return;
    wanted = target;
    if (target) pausedByBrowser = false;
    const clip = target ? ensureElement() : element;
    if (!clip) return;
    try {
      if (target) {
        const started = clip.play();
        // Autoplay policy can refuse a start outside a user gesture; the next
        // sync (any later state change) tries again.
        if (started?.catch) void started.catch(() => { if (wanted && clip.paused) wanted = false; });
      } else if (!clip.paused) {
        clip.pause();
      }
    } catch { /* system controls are optional; playback itself carries on */ }
  }

  return {
    sync,
    get playing() { return Boolean(element && !element.paused); },
    dispose() {
      disposed = true;
      wanted = false;
      try { element?.pause?.(); } catch { /* optional */ }
      element = null;
    },
  };
}
