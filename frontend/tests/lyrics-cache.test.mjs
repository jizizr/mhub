import test from "node:test";
import assert from "node:assert/strict";
import { createLyricsService, lyricIdentity } from "../src/services/lyrics.js";
import { getTrackLyrics } from "../src/apis/music.js";
import { buildLyricLines } from "../src/utils/lyrics.js";

const KEY = "music-web-lyrics-v1";
const track = (overrides = {}) => ({ platform: "applemusic", source_id: "123", id: "applemusic--123--high", quality: "high", title: "测试歌曲", artist: "测试艺人", ...overrides });
const payload = { available: true, plain: "[00:00.00]第一句\n[00:12.00]第二句", translation: "[00:00.00]First line" };
const storage = () => {
  const values = new Map();
  return { values, getItem: (key) => values.get(key), setItem: (key, value) => values.set(key, value) };
};
const fixture = (options = {}) => {
  const saved = options.saved || storage();
  const calls = [];
  const service = createLyricsService({ storage: () => saved, retryDelay: 0, fetchLyrics: async (value, request) => { calls.push({ value, request }); return payload; }, ...options });
  return { saved, calls, service };
};

test("lyrics start from source metadata before any playback task exists", async (t) => {
  const requests = [];
  t.mock.method(globalThis, "fetch", async (url, options) => { requests.push({ url, options }); return Response.json(payload); });
  await getTrackLyrics(track({ duration: 90200, album: "测试专辑" }), { priority: "low" });
  const url = new URL(requests[0].url, "https://example.invalid");
  assert.equal(url.pathname, "/api/v1/lyrics");
  assert.equal(url.searchParams.get("track_id"), "123");
  assert.equal(url.searchParams.get("title"), "测试歌曲");
  assert.equal(url.searchParams.get("duration"), "90");
  assert.equal(requests[0].options.priority, "low");
  assert.equal(requests.length, 1);
});

test("legacy task-only tracks retain the playback lyric endpoint", async (t) => {
  const requests = [];
  t.mock.method(globalThis, "fetch", async (url) => { requests.push(url); return Response.json(payload); });
  await getTrackLyrics({ playback_id: "legacy/task" });
  assert.deepEqual(requests, ["/api/v1/playbacks/legacy%2Ftask/lyrics"]);
  assert.deepEqual(await getTrackLyrics(null), { available: false });
});

test("early, resolved and opened lyric callers share one pending request", async () => {
  let release;
  let calls = 0;
  const gate = new Promise((resolve) => { release = resolve; });
  const { service } = fixture({ fetchLyrics: async () => { calls += 1; await gate; return payload; } });
  const early = service.load(track());
  const resolved = service.load(track({ playback_id: "resolved", quality: "hires", streaming_mode: "wasm_alac" }));
  const opened = service.load(track());
  assert.equal(early, resolved);
  assert.equal(resolved, opened);
  assert.equal(calls, 1);
  release();
  const lines = await early;
  assert.equal(lines.length, 2);
  assert.equal(lines[0].translatedLyric, "First line");
  assert.equal((await service.load(track()))[1].startTime, 12000);
  assert.equal(calls, 1);
});

test("persisted original lyrics survive reload, quality changes and script preferences", async () => {
  const first = fixture();
  await first.service.load(track());
  const restored = fixture({ saved: first.saved });
  const lines = await restored.service.load(track({ quality: "hires", id: "new-task", lyricScript: "traditional" }));
  assert.equal(lines[0].words[0].word, "第一句");
  assert.equal(restored.calls.length, 0);
  assert.equal(lyricIdentity(track()), lyricIdentity(track({ selected_id: "other", quality: "hires" })));
  assert.notEqual(lyricIdentity(track()), lyricIdentity(track({ platform: "netease" })));
  assert.notEqual(lyricIdentity(track()), lyricIdentity(track({ source_id: "124" })));
});

test("failed requests retry once and failures never become a permanent empty result", async () => {
  let calls = 0;
  let fail = true;
  const { service, saved } = fixture({ fetchLyrics: async () => { calls += 1; if (fail) throw new Error("offline"); return payload; } });
  await assert.rejects(service.load(track()), /offline/);
  assert.equal(calls, 2);
  assert.equal(saved.values.has(KEY), false);
  fail = false;
  assert.equal((await service.load(track())).length, 2);
  assert.equal(calls, 3);
});

test("a transient provider error recovers without reopening the lyrics", async () => {
  let calls = 0;
  const { service } = fixture({ fetchLyrics: async () => { if (++calls === 1) throw new Error("503"); return payload; } });
  assert.equal((await service.load(track())).length, 2);
  assert.equal(calls, 2);
});

test("a hung lyric request is bounded, aborted and retried independently of audio", async () => {
  let calls = 0;
  let aborted = false;
  const { service } = fixture({ requestTimeout: 10, fetchLyrics: async (_, { signal }) => {
    if (++calls > 1) return payload;
    return new Promise((_, reject) => signal.addEventListener("abort", () => { aborted = true; reject(new Error("timeout")); }, { once: true }));
  } });
  assert.equal((await service.load(track())).length, 2);
  assert.equal(aborted, true);
  assert.equal(calls, 2);
});

test("unavailable lyrics are briefly cached, then can recover", async () => {
  let timestamp = 1000;
  let calls = 0;
  const { service } = fixture({ now: () => timestamp, fetchLyrics: async () => ++calls === 1 ? { available: false } : payload });
  assert.deepEqual(await service.load(track()), []);
  assert.deepEqual(await service.load(track()), []);
  assert.equal(calls, 1);
  timestamp += 5 * 60 * 1000 + 1;
  assert.equal((await service.load(track())).length, 2);
  assert.equal(calls, 2);
});

test("known lyrics expire after thirty days rather than masking later corrections", async () => {
  let timestamp = 1000;
  const { service, calls } = fixture({ now: () => timestamp });
  await service.load(track());
  timestamp += 30 * 24 * 60 * 60 * 1000 + 1;
  await service.load(track());
  assert.equal(calls.length, 2);
});

test("broken storage, invalid versions and malformed payloads safely miss", async () => {
  for (const value of ["{broken", JSON.stringify({ version: 0, entries: [] }), JSON.stringify({ version: 1, entries: [{ key: lyricIdentity(track()), expiresAt: Date.now() + 10000, usedAt: Date.now(), payload: null }] })]) {
    const saved = storage(); saved.values.set(KEY, value);
    const { service, calls } = fixture({ saved });
    assert.equal((await service.load(track())).length, 2);
    assert.equal(calls.length, 1);
  }
});

test("storage denial still leaves a working bounded session cache", async () => {
  const denied = { getItem: () => { throw new Error("denied"); }, setItem: () => { throw new Error("quota"); } };
  const { service, calls } = fixture({ saved: denied });
  await service.load(track());
  assert.equal((await service.load(track())).length, 2);
  assert.equal(calls.length, 1);
});

test("the persistent cache evicts old entries and stays within its size budget", async () => {
  const { service, saved } = fixture({ fetchLyrics: async () => ({ available: true, plain: "[00:00.00]" + "词".repeat(10000) }) });
  for (let index = 0; index < 120; index++) await service.load(track({ source_id: `large-${index}` }));
  const raw = saved.getItem(KEY);
  assert.ok(raw.length <= 500000);
  const entries = JSON.parse(raw).entries;
  assert.ok(entries.length <= 100);
  assert.equal(entries.at(-1).key, lyricIdentity(track({ source_id: "large-119" })));
  assert.equal(entries.some((entry) => entry.key === lyricIdentity(track({ source_id: "large-0" }))), false);
});

test("lyrics request starts immediately while the optional parser loads once", async () => {
  let finishParser;
  const parserGate = new Promise((resolve) => { finishParser = resolve; });
  let parserLoads = 0;
  const { service, calls } = fixture({ loadParser: () => { parserLoads += 1; return parserGate; } });
  assert.equal(parserLoads, 0, "constructing the app must not load the parser");
  assert.equal(service.getCached(track()), null, "a synchronous cold probe must remain synchronous");
  const first = service.load(track());
  assert.equal(calls.length, 1, "the lyric API starts before the parser resolves");
  assert.equal(service.load(track()), first, "opening the player joins the complete pending operation");
  const other = service.load(track({ source_id: "456" }));
  await Promise.resolve();
  assert.equal(parserLoads, 1, "different songs share parser startup");
  finishParser(buildLyricLines);
  assert.deepEqual(await first, buildLyricLines(payload));
  assert.deepEqual(await other, buildLyricLines(payload));
  assert.deepEqual(service.getCached(track()), buildLyricLines(payload));
  assert.equal(calls.length, 2);
});

test("restored lyrics share delayed parsing without making a network request", async () => {
  const initial = fixture();
  await initial.service.load(track());
  let finishParser;
  let parses = 0;
  const parserGate = new Promise((resolve) => { finishParser = resolve; });
  const restored = fixture({ saved: initial.saved, loadParser: () => parserGate });
  const first = restored.service.load(track());
  assert.equal(restored.service.load(track()), first);
  assert.equal(restored.calls.length, 0);
  finishParser((value) => { parses += 1; return buildLyricLines(value); });
  assert.deepEqual(await first, buildLyricLines(payload));
  assert.equal(parses, 1);
  assert.equal(restored.calls.length, 0);
});

test("a failed parser import can recover without caching an empty lyric result", async () => {
  let attempts = 0;
  const { service, saved } = fixture({ loadParser: async () => {
    if (++attempts === 1) throw new Error("parser chunk unavailable");
    return buildLyricLines;
  } });
  await assert.rejects(service.load(track()), /parser chunk unavailable/);
  assert.equal(saved.values.has(KEY), false);
  assert.deepEqual(await service.load(track()), buildLyricLines(payload));
  assert.equal(attempts, 2);
});

test("a parser failure retains coalescing until the already-started lyric request settles", async () => {
  let finishRequest;
  const requestGate = new Promise((resolve) => { finishRequest = resolve; });
  let requests = 0;
  let imports = 0;
  const { service } = fixture({
    fetchLyrics: () => { requests += 1; return requestGate; },
    loadParser: async () => { imports += 1; throw new Error("parser offline"); },
  });
  const first = service.load(track());
  const rejection = assert.rejects(first, /parser offline/);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(service.load(track()), first);
  assert.equal(requests, 1);
  assert.equal(imports, 1);
  finishRequest(payload);
  await rejection;
});

test("an unparseable persisted payload refetches corrected lyrics once", async () => {
  const first = fixture();
  await first.service.load(track());
  const stored = JSON.parse(first.saved.getItem(KEY));
  stored.entries[0].payload.plain = "invalid stored lyric";
  first.saved.setItem(KEY, JSON.stringify(stored));
  const restored = fixture({ saved: first.saved, loadParser: async () => (value) => {
    if (value.plain === "invalid stored lyric") throw new Error("bad old lyrics");
    return buildLyricLines(value);
  } });
  const pending = restored.service.load(track());
  assert.equal(restored.service.load(track()), pending);
  assert.deepEqual(await pending, buildLyricLines(payload));
  assert.equal(restored.calls.length, 1);
  assert.deepEqual(restored.service.getCached(track()), buildLyricLines(payload));
  assert.equal(JSON.parse(first.saved.getItem(KEY)).entries[0].payload.plain, payload.plain);
});
