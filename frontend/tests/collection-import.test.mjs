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

test("imports over 200 tracks with bounded requests and original order", async () => {
  const sizes = [];
  globalThis.fetch = async (_path, options) => {
    const request = JSON.parse(options.body);
    sizes.push(request.items.length);
    return Response.json(matchResponse(request));
  };
  const items = sourceItems(501);
  const result = await matchCollection({ sourcePlatform: "netease", targetPlatform: "netease", items });
  assert.deepEqual(sizes, [200, 200, 101]);
  assert.equal(result.matched_count, 501);
  assert.deepEqual(result.items.map((item) => item.source.id), items.map((item) => item.id));
});

test("never returns a partial import when a later batch fails", async () => {
  let calls = 0;
  globalThis.fetch = async (_path, options) => {
    if (++calls === 2) return Response.json({ detail: "Fixture network failure" }, { status: 503 });
    return Response.json(matchResponse(JSON.parse(options.body)));
  };
  await assert.rejects(matchCollection({
    sourcePlatform: "netease", targetPlatform: "applemusic", items: sourceItems(201),
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
