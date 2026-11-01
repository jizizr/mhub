import test from "node:test";
import assert from "node:assert/strict";
import { buildLyricLines } from "../src/utils/lyrics.js";
import { createLyricsService } from "../src/services/lyrics.js";

const payload = {
  available: true,
  plain: "[00:21.961]拦路雨偏似雪花\n[00:26.464]这风褛我给你磨到有襟花",
  translation: "",
  words: [],
  roman: "[00:21.961]lan lou yv pin ci sv fa\n[00:26.464]zei fong lao o ka nei mo dou yao kam fa",
  credits: ["作词：林夕", "作曲：Christopher Chak"],
};

test("romanization attaches to the line it was timed to", () => {
  const lines = buildLyricLines(payload);
  assert.deepEqual(lines.map((line) => line.romanLyric), [
    "lan lou yv pin ci sv fa",
    "zei fong lao o ka nei mo dou yao kam fa",
  ]);
  assert.deepEqual(lines.credits, payload.credits);
});

test("word-timed lines also receive romanization", () => {
  const lines = buildLyricLines({
    ...payload,
    plain: "",
    words: [{ start: 22180, end: 26000, words: [[22180, 22700, "拦"], [22700, 23200, "路"]] }],
    roman: "[00:22.180]lan lou",
  });
  assert.equal(lines[0].romanLyric, "lan lou");
});

test("payloads without credits stay a plain line array", () => {
  const lines = buildLyricLines({ ...payload, credits: [], roman: "" });
  assert.equal(lines.credits, undefined);
  assert.equal(lines[0].romanLyric, "");
});

test("the service keeps romanization and credits through its storage", async () => {
  const store = new Map();
  const storage = () => ({
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, value) => store.set(key, value),
    removeItem: (key) => store.delete(key),
  });
  store.set("music-web-lyrics-v2", "old");
  const service = createLyricsService({
    fetchLyrics: async () => ({ ...payload, credits: [...payload.credits, 7, ""] }),
    parseLyrics: buildLyricLines,
    storage,
  });
  const track = { platform: "netease", source_id: "65766" };
  const lines = await service.load(track);
  assert.deepEqual(lines.credits, payload.credits);
  assert.equal(store.has("music-web-lyrics-v2"), false);
  const restored = createLyricsService({ fetchLyrics: async () => { throw new Error("offline"); }, parseLyrics: buildLyricLines, storage });
  const again = await restored.load(track);
  assert.equal(again[1].romanLyric, "zei fong lao o ka nei mo dou yao kam fa");
  assert.deepEqual(again.credits, payload.credits);
});
