import { test } from "node:test";
import assert from "node:assert/strict";
import { createLyricClock } from "../src/utils/lyric-clock.js";

function setup() {
  const state = { audio: 0, wall: 0 };
  const clock = createLyricClock(() => state.audio, () => 150, () => state.wall);
  const advance = (ms) => { state.audio += ms; state.wall += ms; return clock.time(); };
  return { state, clock, advance };
}

test("continuous playback trails the audio by the delay", () => {
  const { clock, advance, state } = setup();
  state.audio = 10_000;
  clock.time();
  assert.equal(advance(16), 10_016 - 150);
});

test("a lyric tap shows the tapped line at once, before and after the seek lands", () => {
  const { clock, advance, state } = setup();
  state.audio = 60_000;
  clock.time();
  clock.seekTo(40_000);
  assert.equal(clock.time(), 60_000 - 150, "the old position stays until the seek lands");
  state.audio = 40_000;
  assert.equal(clock.time(), 40_000, "a backward jump is held at its target");
  assert.equal(advance(100), 40_000);
  assert.equal(advance(100), 40_200 - 150, "the delay resumes once it catches up");
});

test("tapping a later line holds its start while the seek is pending", () => {
  const { clock, advance, state } = setup();
  state.audio = 10_000;
  clock.time();
  clock.seekTo(30_000);
  assert.equal(advance(16), 30_000);
  state.audio = 30_000;
  assert.equal(clock.time(), 30_000);
});

test("paused seeks from the progress bar are not shown before their target", () => {
  const { clock, state } = setup();
  state.audio = 5_000;
  clock.time();
  state.audio = 90_000;
  state.wall += 16;
  assert.equal(clock.time(), 90_000);
});

test("small sync corrections do not count as seeks", () => {
  const { clock, advance, state } = setup();
  state.audio = 10_000;
  clock.time();
  advance(16);
  state.audio -= 30;
  assert.equal(clock.time(), 10_016 - 30 - 150);
});
