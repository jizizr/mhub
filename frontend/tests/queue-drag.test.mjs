import test from "node:test";
import assert from "node:assert/strict";
import { queueDropIndex, queueScrollStep } from "../src/utils/queue-drag.js";

const rows = [0, 70, 140, 210].map(top => ({ top, height: 60 }));
test("queue target works in both directions without moving until a neighboring midpoint is crossed", () => {
  assert.equal(queueDropIndex(rows, 1, 90), 1);
  assert.equal(queueDropIndex(rows, 1, 169), 1);
  assert.equal(queueDropIndex(rows, 1, 171), 2);
  assert.equal(queueDropIndex(rows, 2, 101), 2);
  assert.equal(queueDropIndex(rows, 2, 99), 1);
  assert.equal(queueDropIndex(rows, 3, -20), 0);
  assert.equal(queueDropIndex(rows, 0, 400), 3);
});
test("the source row's translated geometry does not move the drop target", () => {
  const translated = rows.map((row, index) => index === 1 ? { ...row, top: 1000 } : row);
  assert.equal(queueDropIndex(translated, 1, 171), 2);
});
test("edge scrolling follows the visible viewport and clamps delayed frames", () => {
  assert.equal(queueScrollStep(200, 50, 400), 0);
  assert.ok(queueScrollStep(60, 50, 400) < 0);
  assert.ok(queueScrollStep(390, 50, 400) > 0);
  assert.equal(queueScrollStep(500, 50, 400, 1000), 25.6);
  assert.equal(queueScrollStep(-20, 50, 400, 16), -12.8);
  assert.equal(queueScrollStep(200, 250, 200), 0);
});
