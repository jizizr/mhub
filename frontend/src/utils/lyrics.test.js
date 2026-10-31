import { test } from "node:test";
import assert from "node:assert/strict";
import { buildLyricLines } from "./lyrics.js";

test("translation tolerates source timestamp drift without reuse", () => {
  const lines = buildLyricLines({ available: true, plain: "[00:01.000]Hello\n[00:01.500]World\n[00:05.000]Bye", translation: "[00:01.200]你好\n[00:08.000]再见" });
  assert.deepEqual(lines.map(l => l.translatedLyric), ["你好", "", ""]);
});
test("translation-only response remains visible", () => {
  const lines = buildLyricLines({ available: true, plain: "", translation: "[00:01.000]你好" });
  assert.equal(lines[0].words[0].word, "你好");
});
test("word-timed payload keeps words, background vocals and translation", () => {
  const lines = buildLyricLines({
    available: true,
    plain: "[00:01.000]你好",
    translation: "[00:01.000]Hello",
    words: [
      { start: 1000, end: 2000, words: [[1000, 1500, "你"], [1500, 2000, "好"]] },
      { start: 1600, end: 2200, bg: true, words: [[1600, 2200, "(oh)"]] },
    ],
  });
  assert.equal(lines.length, 2);
  assert.deepEqual(lines[0].words.map((word) => [word.word, word.startTime, word.endTime]), [["你", 1000, 1500], ["好", 1500, 2000]]);
  assert.equal(lines[0].translatedLyric, "Hello");
  assert.equal(lines[1].isBG, true);
  assert.equal(lines[1].translatedLyric, "");
});
