import test from "node:test";
import assert from "node:assert/strict";
import { HANDOFF_SIGNAL_MS, createHandoffMark, handoffField, takeoverFrom } from "../src/services/handoff-signal.js";

const NOW = 1_800_000_000_000;
const session = (handoff, { age = 5, device_name = "Safari · macOS" } = {}) => ({
  device_id: "mac", device_name, updated_at: Math.round(NOW / 1000) - age, handoff,
});

test("the continuing device names the other one for a short while", () => {
  const mark = createHandoffMark("phone", "h1", NOW);
  assert.deepEqual(handoffField(mark, NOW), { handoff: { from: "phone", id: "h1" } });
  assert.deepEqual(handoffField(mark, NOW + HANDOFF_SIGNAL_MS - 1), { handoff: { from: "phone", id: "h1" } });
  assert.deepEqual(handoffField(mark, NOW + HANDOFF_SIGNAL_MS), {});
  assert.deepEqual(handoffField(null, NOW), {});
  assert.equal(createHandoffMark("", "h1", NOW), null);
});

test("the named device stops once while it plays", () => {
  const handled = new Set();
  const options = { ownDeviceId: "phone", handled, wantsPlay: true, now: NOW };
  assert.deepEqual(takeoverFrom(session({ from: "phone", id: "h1" }), options), { id: "h1", device_name: "Safari · macOS" });
  // Later reports of the same hand-off (every 15 s) do not pause it again.
  assert.equal(takeoverFrom(session({ from: "phone", id: "h1" }), options), null);
});

test("a signal seen while not playing is spent, so playing here again is not interrupted", () => {
  const handled = new Set();
  assert.equal(takeoverFrom(session({ from: "phone", id: "h2" }), { ownDeviceId: "phone", handled, wantsPlay: false, now: NOW }), null);
  assert.equal(takeoverFrom(session({ from: "phone", id: "h2" }), { ownDeviceId: "phone", handled, wantsPlay: true, now: NOW }), null);
});

test("other devices, missing or stale signals change nothing", () => {
  const options = { ownDeviceId: "phone", handled: new Set(), wantsPlay: true, now: NOW };
  assert.equal(takeoverFrom(session({ from: "tablet", id: "h3" }), options), null);
  assert.equal(takeoverFrom(session(undefined), options), null);
  assert.equal(takeoverFrom(session({ from: "phone" }), options), null);
  assert.equal(takeoverFrom(null, options), null);
  assert.equal(takeoverFrom(session({ from: "phone", id: "h4" }, { age: HANDOFF_SIGNAL_MS / 1000 + 60 }), options), null, "read long after the fact");
  assert.equal(takeoverFrom(session({ from: "phone", id: "h5" }), { ...options, ownDeviceId: "" }), null);
});
