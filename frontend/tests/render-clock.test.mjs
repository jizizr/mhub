import test from "node:test";
import assert from "node:assert/strict";
import { createRenderClockWatch } from "../src/core/player/render-clock.js";

function run(watch, context, { rate, steps, stepMs = 250, clock }) {
  let fired = false;
  for (let index = 0; index < steps; index += 1) {
    clock.wall += stepMs;
    context.currentTime += rate * stepMs / 1000;
    if (watch.sample(context)) fired = true;
  }
  return fired;
}

test("a late-starting clock and a context rendering in real time are never reported", () => {
  const clock = { wall: 0 };
  const watch = createRenderClockWatch({ now: () => clock.wall });
  const context = { state: "running", currentTime: 0 };
  assert.equal(run(watch, context, { rate: 0.75, steps: 12, clock }), false);
  assert.equal(run(watch, context, { rate: 1.002, steps: 200, clock }), false);
});

test("slow-motion and stalled render clocks are reported after two windows", () => {
  for (const rate of [0.33, 0.85, 0]) {
    const clock = { wall: 0 };
    const watch = createRenderClockWatch({ now: () => clock.wall });
    const context = { state: "running", currentTime: 0 };
    assert.equal(run(watch, context, { rate, steps: 24, clock }), false, `warm-up and one window at ${rate}`);
    assert.equal(run(watch, context, { rate, steps: 13, clock }), true, `two windows at ${rate}`);
  }
});

test("a suspended context or a throttled timer gap restarts the measurement", () => {
  const clock = { wall: 0 };
  const watch = createRenderClockWatch({ now: () => clock.wall });
  const context = { state: "running", currentTime: 0 };
  run(watch, context, { rate: 0, steps: 25, clock });
  context.state = "suspended";
  assert.equal(watch.sample(context), false);
  context.state = "running";
  assert.equal(run(watch, context, { rate: 0, steps: 25, clock }), false);
  // The page slept for a minute: the frozen clock over that gap is not drift.
  clock.wall += 60000;
  assert.equal(watch.sample(context), false);
  assert.equal(run(watch, context, { rate: 1, steps: 40, clock }), false);
});
