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
