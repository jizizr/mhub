import test from "node:test";
import assert from "node:assert/strict";
import { getCachedLoudness, warmLoudness } from "../src/services/loudness.js";

const KEY = "music-web-loudness-v1";
const track = (overrides = {}) => ({ playback_id: "song--hires", id: "fallback", quality: "hires", format: "alac", byte_size: 123456, streaming_mode: "wasm_alac", streaming_codec: "alac", streaming_protocol: "alac-packets-v1", ...overrides });
const ready = (overrides = {}) => ({ state: "ready", version: 1, integrated_lufs: -20, true_peak_dbfs: -6, gain_db: 4, ...overrides });
const flush = () => new Promise((resolve) => setImmediate(resolve));

function harness(t, handler = () => ready()) {
  const values = new Map();
  const requests = [];
  t.mock.method(globalThis, "fetch", async (url, options) => {
    requests.push({ url, options });
    const value = await handler(requests.length, options);
    return Response.json(value);
  });
  const previous = globalThis.localStorage;
  globalThis.localStorage = { getItem: (key) => values.get(key), setItem: (key, value) => values.set(key, value) };
  t.after(() => { globalThis.localStorage = previous; });
  return { values, requests };
}

test("existing task analysis is GET-only, cached, and reused without a request", async (t) => {
  const h = harness(t);
  const item = track({ playback_id: "task/with space" });
  assert.equal(getCachedLoudness(item), null);
  assert.deepEqual(await warmLoudness(item), ready());
  assert.equal(h.requests[0].url, "/api/v1/playbacks/task%2Fwith%20space/loudness");
  assert.equal(h.requests[0].options.method, "GET");
  assert.equal(h.requests[0].options.cache, "no-store");
  assert.equal(h.requests[0].options.credentials, "same-origin");
  assert.deepEqual(getCachedLoudness(item), ready());
  assert.deepEqual(await warmLoudness(item), ready());
  assert.equal(h.requests.length, 1);
});

test("quality, format, size, task and packet representation cannot share gains", async (t) => {
  harness(t);
  await warmLoudness(track());
  for (const changed of [
    { quality: "lossless" }, { format: "flac" }, { byte_size: 654321 }, { playback_id: "other" },
    { streaming_mode: "" }, { streaming_codec: "flac" }, { streaming_protocol: "alac-packets-v2" },
    { byte_size: null }, { format: "" },
  ]) assert.equal(getCachedLoudness(track(changed)), null, JSON.stringify(changed));
});

test("an unresolved representation may be analyzed but never poisons the cache", async (t) => {
  const h = harness(t);
  assert.deepEqual(await warmLoudness(track({ byte_size: null })), ready());
  assert.equal(getCachedLoudness(track({ byte_size: null })), null);
  assert.equal(h.values.has(KEY), false);
});

test("corrupt storage, obsolete versions and manipulated gains safely miss", async (t) => {
  const h = harness(t);
  h.values.set(KEY, "not JSON");
  assert.equal(getCachedLoudness(track()), null);
  await warmLoudness(track());
  const valid = h.values.get(KEY);
  for (const mutate of [
    (saved) => { saved.version = 0; },
    (saved) => { saved.entries[0].analysis.version = 0; },
    (saved) => { saved.entries[0].analysis.gain_db = 30; },
    (saved) => { saved.entries[0].analysis.true_peak_dbfs = null; },
    (saved) => { saved.entries[0].analysis.integrated_lufs = "-20"; },
  ]) {
    const saved = JSON.parse(valid); mutate(saved); h.values.set(KEY, JSON.stringify(saved));
    assert.equal(getCachedLoudness(track()), null);
  }
});

test("invalid or mismatched server values are unavailable and never cached", async (t) => {
  const values = [ready({ version: 2 }), ready({ gain_db: 9 }), ready({ integrated_lufs: -70 }), ready({ true_peak_dbfs: null }), { state: "unknown", version: 1 }];
  const h = harness(t, (count) => values[count - 1]);
  for (const _ of values) {
    assert.deepEqual(await warmLoudness(track()), { state: "unavailable", version: 1 });
    assert.equal(getCachedLoudness(track()), null);
  }
  assert.equal(h.values.has(KEY), false);
});

test("pending analysis polls every five seconds and then saves the result", async (t) => {
  const h = harness(t, (count) => count < 3 ? { state: "pending", version: 1 } : ready());
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const pending = warmLoudness(track());
  await flush();
  assert.equal(h.requests.length, 1);
  t.mock.timers.tick(4999); await flush(); assert.equal(h.requests.length, 1);
  t.mock.timers.tick(1); await flush(); assert.equal(h.requests.length, 2);
  t.mock.timers.tick(5000); await flush();
  assert.deepEqual(await pending, ready());
  assert.deepEqual(getCachedLoudness(track()), ready());
});

test("polling stops after sixty seconds without an unhandled rejection", async (t) => {
  const h = harness(t, () => ({ state: "pending", version: 1 }));
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const pending = warmLoudness(track());
  await flush();
  for (let i = 0; i < 12; i += 1) { t.mock.timers.tick(5000); await flush(); }
  assert.deepEqual(await pending, { state: "pending", version: 1 });
  assert.equal(h.requests.length, 12);
  t.mock.timers.tick(100000); await flush(); assert.equal(h.requests.length, 12);
});

test("explicit cancellation remains identifiable and cancels the polling timer", async (t) => {
  const h = harness(t, () => ({ state: "pending", version: 1 }));
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const controller = new AbortController();
  const pending = warmLoudness(track(), { signal: controller.signal });
  const rejected = assert.rejects(pending, { name: "AbortError" });
  await flush(); controller.abort("track changed"); await rejected;
  t.mock.timers.tick(100000); await flush(); assert.equal(h.requests.length, 1);
  await assert.rejects(warmLoudness(track(), { signal: controller.signal }), { name: "AbortError" });
  assert.equal(h.requests.length, 1);
});

test("network failures and unavailable storage never disrupt playback", async (t) => {
  const h = harness(t, () => { throw new TypeError("network unavailable"); });
  assert.deepEqual(await warmLoudness(track()), { state: "unavailable", version: 1 });
  assert.equal(h.requests.length, 1);
  t.mock.method(globalThis, "fetch", async () => Response.json(ready()));
  globalThis.localStorage = { getItem() { throw new Error("disabled"); }, setItem() { throw new Error("full"); } };
  assert.equal(getCachedLoudness(track()), null);
  assert.deepEqual(await warmLoudness(track()), ready());
  assert.deepEqual(await warmLoudness({}), { state: "unavailable", version: 1 });
});

test("persisted analysis is bounded to the most recent 300 representations", async (t) => {
  const h = harness(t);
  for (let index = 0; index < 301; index += 1) await warmLoudness(track({ playback_id: `song-${index}` }));
  assert.equal(JSON.parse(h.values.get(KEY)).entries.length, 300);
  assert.equal(getCachedLoudness(track({ playback_id: "song-0" })), null);
  assert.deepEqual(getCachedLoudness(track({ playback_id: "song-300" })), ready());
});
