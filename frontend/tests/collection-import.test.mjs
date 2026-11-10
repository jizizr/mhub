import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { matchCollection, resolveCollection } from "../src/apis/music.js";

const originalFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = originalFetch; });

const sourceItems = (count) => Array.from({ length: count }, (_, index) => ({
  id: String(index + 1), song: `Track ${index + 1}`, singer: "Fixture Artist",
}));

function matchResponse(request) {
  return {
    source_platform: request.source_platform,
    target_platform: request.target_platform,
    matched_count: request.items.length,
    review_count: 0,
    missing_count: 0,
    items: request.items.map((source) => ({
      source, status: "matched", candidates: [{ ...source, platform: request.target_platform }],
    })),
  };
}

test("imports large playlists in small requests and original order", async () => {
  const sizes = [];
  globalThis.fetch = async (_path, options) => {
    const request = JSON.parse(options.body);
    sizes.push(request.items.length);
    return Response.json(matchResponse(request));
  };
  const items = sourceItems(60);
  const progress = [];
  const result = await matchCollection({
    sourcePlatform: "netease", targetPlatform: "netease", items, onProgress: ({ done }) => progress.push(done),
  });
  assert.deepEqual(sizes, [25, 25, 10]);
  assert.equal(result.matched_count, 60);
  assert.equal(progress.at(-1), 60);
  assert.deepEqual(result.items.map((item) => item.source.id), items.map((item) => item.id));
});

test("resends deferred songs after the server's wait and keeps their place", async () => {
  const requests = [];
  const waits = [];
  globalThis.fetch = async (_path, options) => {
    const request = JSON.parse(options.body);
    requests.push(request.items.map((item) => item.id));
    const response = matchResponse(request);
    // The first round can only search the first two songs.
    if (requests.length === 1) {
      response.items.slice(2).forEach((item) => { item.status = "deferred"; item.candidates = []; });
      response.retry_after_ms = 1200;
    }
    return Response.json(response);
  };
  const items = sourceItems(5);
  const result = await matchCollection({
    sourcePlatform: "netease", targetPlatform: "applemusic", items, batchSize: 4,
    wait: async (ms) => { waits.push(ms); },
  });
  assert.deepEqual(requests, [["1", "2", "3", "4"], ["3", "4", "5"]]);
  assert.deepEqual(waits, [1200]);
  assert.equal(result.matched_count, 5);
  assert.equal(result.deferred_count, 0);
  assert.deepEqual(result.items.map((item) => item.source.id), items.map((item) => item.id));
});

test("leaves songs deferred, not missing, while the platform stays throttled", async () => {
  let calls = 0;
  globalThis.fetch = async (_path, options) => {
    calls += 1;
    const response = matchResponse(JSON.parse(options.body));
    response.items.forEach((item) => { item.status = "deferred"; item.candidates = []; });
    return Response.json(response);
  };
  const result = await matchCollection({
    sourcePlatform: "netease", targetPlatform: "applemusic", items: sourceItems(3), maxStalls: 3, wait: async () => {},
  });
  assert.equal(calls, 3);
  assert.equal(result.deferred_count, 3);
  assert.equal(result.missing_count, 0);
  assert.deepEqual(result.items.map((item) => item.status), ["deferred", "deferred", "deferred"]);
});

test("never returns a partial import when a later batch fails", async () => {
  let calls = 0;
  globalThis.fetch = async (_path, options) => {
    if (++calls === 2) return Response.json({ detail: "Fixture network failure" }, { status: 503 });
    return Response.json(matchResponse(JSON.parse(options.body)));
  };
  await assert.rejects(matchCollection({
    sourcePlatform: "netease", targetPlatform: "applemusic", items: sourceItems(30),
  }), /Fixture network failure/);
  assert.equal(calls, 2);
});

test("rejects missing or reordered matching rows", async () => {
  for (const mutate of [(response) => response.items.pop(), (response) => response.items.reverse()]) {
    globalThis.fetch = async (_path, options) => {
      const response = matchResponse(JSON.parse(options.body));
      mutate(response);
      return Response.json(response);
    };
    await assert.rejects(matchCollection({
      sourcePlatform: "netease", targetPlatform: "netease", items: sourceItems(3),
    }), /不完整/);
  }
});

test("complete import omits preview limit while recommendations may request one", async () => {
  const urls = [];
  globalThis.fetch = async (path) => { urls.push(new URL(path, "http://localhost")); return Response.json({ items: [] }); };
  await resolveCollection({ platform: "netease", url: "https://music.163.com/playlist?id=1" });
  await resolveCollection({ platform: "netease", url: "https://music.163.com/playlist?id=1", limit: 40 });
  assert.equal(urls[0].searchParams.has("limit"), false);
  assert.equal(urls[1].searchParams.get("limit"), "40");
});

test("an incomplete playlist is reported by code and can be imported on request", async () => {
  const urls = [];
  globalThis.fetch = async (path) => {
    const url = new URL(path, "http://localhost");
    urls.push(url);
    if (url.searchParams.get("allow_incomplete") === "true") return Response.json({ items: [{ id: "1" }] });
    return Response.json({ detail: "网易云歌单应有 3 首，但接口只返回 2 首", code: "incomplete_collection" }, { status: 409 });
  };
  await assert.rejects(
    resolveCollection({ platform: "netease", url: "https://music.163.com/playlist?id=1" }),
    (error) => error.code === "incomplete_collection" && /只返回 2 首/.test(error.message),
  );
  const collection = await resolveCollection({ platform: "netease", url: "https://music.163.com/playlist?id=1", allowIncomplete: true });
  assert.equal(collection.items.length, 1);
  assert.equal(urls[0].searchParams.has("allow_incomplete"), false);
});
