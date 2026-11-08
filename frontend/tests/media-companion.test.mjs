import test from "node:test";
import assert from "node:assert/strict";
import { createMediaCompanion, silentWav } from "../src/core/player/media-companion.js";

class FakeAudio {
  constructor() { this.paused = true; this.ended = false; this.listeners = {}; this.plays = 0; this.pauses = 0; this.attributes = {}; }
  setAttribute(name, value) { this.attributes[name] = value; }
  addEventListener(type, listener) { (this.listeners[type] ||= []).push(listener); }
  emit(type) { for (const listener of this.listeners[type] || []) listener(); }
  play() { this.plays += 1; this.paused = false; this.emit("play"); return Promise.resolve(); }
  pause() { this.pauses += 1; this.paused = true; this.emit("pause"); }
}

function companion(overrides = {}) {
  const elements = [];
  const events = [];
  const value = createMediaCompanion({
    createElement: () => { const element = new FakeAudio(); elements.push(element); return element; },
    createSourceUrl: () => "blob:silence",
    onExternalPause: () => events.push("pause"),
    onExternalPlay: () => events.push("play"),
    ...overrides,
  });
  return { value, elements, events };
}

test("the silent clip is a valid long WAV of zero-level samples", () => {
  const wav = silentWav(6, 8000);
  const view = new DataView(wav.buffer);
  assert.equal(new TextDecoder().decode(wav.slice(0, 4)), "RIFF");
  assert.equal(new TextDecoder().decode(wav.slice(8, 12)), "WAVE");
  assert.equal(view.getUint32(24, true), 8000);
  assert.equal(view.getUint16(34, true), 8);
  assert.equal(view.getUint32(40, true), 48000);
  assert.equal(wav.length, 44 + 48000);
  assert.ok(wav.slice(44).every((sample) => sample === 0x80));
  // Chromium treats media under 5 s as transient and shows no notification.
  assert.ok(silentWav().length - 44 >= 8000 * 5);
});

test("plays a looping unmuted element only while playback is wanted", () => {
  const h = companion();
  h.value.sync(false);
  assert.equal(h.elements.length, 0, "no element before the first wanted playback");
  h.value.sync(true);
  const [element] = h.elements;
  assert.equal(element.loop, true);
  assert.equal(element.muted, undefined);
  assert.equal(element.src, "blob:silence");
  assert.equal(element.plays, 1);
  h.value.sync(true);
  h.value.sync(true);
  assert.equal(element.plays, 1, "repeated syncs do not restart the clip");
  h.value.sync(false);
  assert.equal(element.pauses, 1);
  h.value.sync(true);
  assert.equal(h.elements.length, 1, "the element is reused");
  assert.equal(element.plays, 2);
  assert.deepEqual(h.events, [], "its own play/pause is never reported as the browser's");
});

test("a browser pause (focus loss) pauses the music and its resume plays it again", () => {
  const h = companion();
  h.value.sync(true);
  const [element] = h.elements;
  // Audio focus moves to a call: the browser pauses the session's player.
  element.paused = true;
  element.emit("pause");
  assert.deepEqual(h.events, ["pause"]);
  // The player pauses in response.
  h.value.sync(false);
  assert.equal(element.pauses, 0, "already paused by the browser");
  // The call ends and the browser resumes the session.
  element.paused = false;
  element.emit("play");
  assert.deepEqual(h.events, ["pause", "play"]);
  element.emit("play");
  assert.deepEqual(h.events, ["pause", "play"], "only one resume per browser pause");
});

test("a play refused by autoplay policy is retried on the next sync", async () => {
  let refuse = true;
  const h = companion({
    createElement: () => {
      const element = new FakeAudio();
      element.play = function play() {
        this.plays += 1;
        if (refuse) return Promise.reject(new DOMException("blocked", "NotAllowedError"));
        this.paused = false;
        return Promise.resolve();
      };
      h.elements.push(element);
      return element;
    },
  });
  h.value.sync(true);
  await new Promise((resolve) => setTimeout(resolve));
  refuse = false;
  h.value.sync(true);
  assert.equal(h.elements[0].plays, 2);
  assert.equal(h.value.playing, true);
});

test("a missing or failing element never breaks playback", () => {
  const missing = createMediaCompanion({ createElement: () => null });
  missing.sync(true);
  missing.sync(false);
  const failing = createMediaCompanion({ createElement: () => { throw new Error("no media"); } });
  failing.sync(true);
  assert.equal(failing.playing, false);
  const h = companion();
  h.value.sync(true);
  h.value.dispose();
  assert.equal(h.elements[0].paused, true);
  h.value.sync(true);
  assert.equal(h.elements.length, 1, "nothing restarts after dispose");
});
