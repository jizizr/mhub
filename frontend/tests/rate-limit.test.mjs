import test from "node:test";
import assert from "node:assert/strict";

let fixtureId = 0;
async function fixture(t, answers) {
  const calls = [];
  t.mock.method(globalThis, "fetch", async (url, options) => {
    calls.push({ url, options });
    const answer = answers[Math.min(calls.length - 1, answers.length - 1)];
    return new Response(JSON.stringify(answer.body ?? {}), {
      status: answer.status ?? 200,
      headers: { "content-type": "application/json", ...(answer.headers ?? {}) },
    });
  });
  const api = await import(`../src/apis/music.js?rate-limit-test=${++fixtureId}`);
  return { api, calls };
}

const limited = (seconds) => ({
  status: 429,
  headers: { "retry-after": String(seconds) },
  body: { detail: "请求过于频繁，请稍后再试" },
});
const playback = { playback: { id: "p1", status: "queued" } };
const song = { platform: "applemusic", query: "song", selectedId: "1", quality: "lossless" };

test("Retry-After is read as seconds or an HTTP date, never under a second", async (t) => {
  const { api } = await fixture(t, [{}]);
  assert.equal(api.retryAfterMs("7"), 7000);
  assert.equal(api.retryAfterMs("0"), 1000);
  assert.equal(api.retryAfterMs(null), 1000);
  assert.equal(api.retryAfterMs("garbage"), 1000);
  const now = Date.parse("2026-11-09T12:00:00Z");
  assert.equal(api.retryAfterMs("Mon, 09 Nov 2026 12:00:30 GMT", now), 30000);
});

test("a prefetch gives up on the first 429 and reports the server's wait", async (t) => {
  const { api, calls } = await fixture(t, [limited(7)]);
  await assert.rejects(
    api.createPlayback({ ...song, rateLimit: { maxWaitMs: 0 } }),
    (error) => api.isRateLimited(error) && error.status === 429 && error.retryAfterMs === 7000,
  );
  assert.equal(calls.length, 1);
});

test("a chosen song waits out the quota, says so, and then plays", async (t) => {
  const { api, calls } = await fixture(t, [limited(3), limited(4), { body: playback }]);
  const notices = [];
  const waits = [];
  const result = await api.createPlayback({
    ...song,
    rateLimit: {
      maxWaitMs: 120000,
      onWait: (ms) => notices.push(ms),
      wait: async (ms) => { waits.push(ms); },
    },
  });
  assert.deepEqual(result, playback);
  assert.deepEqual(notices, [3000, 4000]);
  assert.deepEqual(waits, [3000, 4000]);
  assert.equal(calls.length, 3);
  // The retry policy never leaks into fetch().
  assert.equal(calls[0].options.rateLimit, undefined);
  assert.equal(calls[0].options.method, "POST");
});

test("waiting stops once the total would pass the caller's limit", async (t) => {
  const { api, calls } = await fixture(t, [limited(5), limited(5)]);
  await assert.rejects(
    api.createPlayback({ ...song, rateLimit: { maxWaitMs: 8000, wait: async () => {} } }),
    (error) => api.isRateLimited(error) && /约 5 秒/.test(error.message),
  );
  assert.equal(calls.length, 2);
});

test("ordinary reads do not sit through a long quota wait", async (t) => {
  const { api, calls } = await fixture(t, [limited(60)]);
  await assert.rejects(
    api.searchTracks({ platform: "applemusic", q: "x" }),
    (error) => api.isRateLimited(error) && error.retryAfterMs === 60000,
  );
  assert.equal(calls.length, 1);
});

test("other errors keep their server message", async (t) => {
  const { api } = await fixture(t, [{ status: 400, body: { detail: "query 不能为空" } }]);
  await assert.rejects(api.createPlayback(song), (error) => !api.isRateLimited(error) && error.message === "query 不能为空");
});

test("a playlist import waits out a 429 and shows the wait in its progress", async (t) => {
  const items = [{ id: "s1", song: "A", singer: "B" }];
  const matched = {
    source_platform: "netease",
    target_platform: "applemusic",
    items: [{ source: { id: "s1" }, status: "matched", confidence: 1, candidates: [] }],
  };
  const { api, calls } = await fixture(t, [limited(20), { body: matched }]);
  const progress = [];
  const waits = [];
  const result = await api.matchCollection({
    sourcePlatform: "netease",
    targetPlatform: "applemusic",
    items,
    onProgress: (value) => progress.push(value),
    wait: async (ms) => { waits.push(ms); },
  });
  assert.equal(result.matched_count, 1);
  assert.equal(calls.length, 2);
  assert.deepEqual(waits, [20000]);
  assert.ok(progress.some((value) => value.waitMs === 20000));
});
