import test from "node:test";
import assert from "node:assert/strict";
import { getRecommendationHome, getContinuationRecommendations, searchTracks } from "../src/apis/music.js";

test("refresh passes recent numeric exclusions and its cancellation signal", async () => {
  const previous = globalThis.fetch;
  const controller = new AbortController();
  try {
    globalThis.fetch = async (path, options) => {
      const url = new URL(path, "https://example.test");
      assert.equal(url.pathname, "/api/v1/recommendations/home");
      assert.equal(url.searchParams.get("refresh"), "true");
      assert.equal(url.searchParams.get("exclude"), "123,456");
      assert.equal(options.signal, controller.signal);
      assert.equal(options.cache, "no-store");
      return Response.json({ tracks: [] });
    };
    assert.deepEqual(await getRecommendationHome({ refresh: true, exclude: [123, "123", "456", "private-name"], signal: controller.signal }), { tracks: [] });
  } finally { globalThis.fetch = previous; }
});

test("continuation keeps large queue exclusions in JSON instead of the URL", async () => {
  const previous = globalThis.fetch;
  const exclude = Array.from({ length: 5000 }, (_, i) => String(1000000000 + i));
  const controller = new AbortController();
  try {
    globalThis.fetch = async (path, options) => {
      assert.equal(path, "/api/v1/recommendations/continue");
      assert.equal(options.method, "POST");
      assert.equal(options.signal, controller.signal);
      assert.equal(options.headers["content-type"], "application/json");
      assert.deepEqual(JSON.parse(options.body), { platform: "applemusic", seed_id: "99", exclude, limit: 10 });
      return Response.json({ source: "applemusic-related", tracks: [] });
    };
    await getContinuationRecommendations({ seedId: "99", exclude: [...exclude, exclude[0]], signal: controller.signal });
  } finally { globalThis.fetch = previous; }
});

test("a cancelled search exposes AbortError without retrying", async () => {
  const previous = globalThis.fetch;
  const controller = new AbortController();
  controller.abort();
  let calls = 0;
  try {
    globalThis.fetch = async (_path, options) => {
      calls += 1;
      options.signal.throwIfAborted();
    };
    await assert.rejects(searchTracks({ platform: "applemusic", q: "test", signal: controller.signal }), { name: "AbortError" });
    assert.equal(calls, 1);
  } finally { globalThis.fetch = previous; }
});
