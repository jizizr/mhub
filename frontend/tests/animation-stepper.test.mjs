import { test } from "node:test";
import assert from "node:assert/strict";
import { createAnimationStepper } from "../src/utils/animation-stepper.js";

const animation = (currentTime, end = 1000, playbackRate = 1) => ({
  currentTime, playbackRate, playState: "running",
  effect: { getComputedTiming: () => ({ endTime: end }) },
  pause() { this.playState = "paused"; },
});

test("running word animations are paused and stepped from the playback clock at a capped rate", () => {
  const forward = animation(200);
  const reversed = animation(500, 1000, -1);
  const stepper = createAnimationStepper(() => ({ getAnimations: () => [forward, reversed] }), { interval: 33 });
  stepper.step(10_000, 0);
  assert.equal(forward.playState, "paused");
  assert.equal(reversed.playState, "running");
  stepper.step(10_100, 10);
  assert.equal(forward.currentTime, 200, "steps faster than the interval are skipped");
  stepper.step(10_300, 40);
  assert.equal(forward.currentTime, 500);
  stepper.step(11_500, 80);
  assert.equal(forward.currentTime, 1000);
  assert.equal(stepper.size, 0, "finished animations are released");
});

test("animations restarted by the renderer are recaptured with their new offset", () => {
  const word = animation(0);
  const stepper = createAnimationStepper(() => ({ getAnimations: () => [word] }), { interval: 0 });
  stepper.step(5_000, 0);
  word.currentTime = 700;
  word.playState = "running";
  stepper.step(9_000, 1);
  stepper.step(9_100, 2);
  assert.equal(word.currentTime, 800);
});
