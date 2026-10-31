import { test } from "node:test";
import assert from "node:assert/strict";
import { applyWordMode, estimateLineWords, sungUnits } from "../src/utils/word-timing.js";
import { convertLyricLinesWith } from "../src/utils/opencc-lines.js";

const line = (text, startTime, endTime) => ({
  words: [{ word: text, startTime, endTime }], startTime, endTime, translatedLyric: "", romanLyric: "", isBG: false, isDuet: false,
});

test("sung units split CJK by character and Latin by word with trailing punctuation", () => {
  assert.deepEqual(sungUnits("你好，世界").map((unit) => unit.text), ["你", "好，", "世", "界"]);
  assert.deepEqual(sungUnits("\"Hello beautiful world!").map((unit) => [unit.text, unit.weight]), [["\"Hello ", 2], ["beautiful ", 3], ["world!", 1]]);
});

test("estimate spreads a line over its gap without stretching across interludes", () => {
  const fast = estimateLineWords(line("你好世界", 1000, 3000), 3000);
  assert.deepEqual(fast.words.map((word) => word.startTime), [1000, 1450, 1900, 2350]);
  assert.equal(fast.words.at(-1).endTime, 2800);
  assert.equal(fast.estimatedWords, true);
  const slow = estimateLineWords(line("你好", 1000, 20000), 20000);
  assert.deepEqual(slow.words.map((word) => [word.startTime, word.endTime]), [[1000, 1600], [1600, 2200]]);
});

test("word modes keep real timing, estimate only line-only rows, or collapse", () => {
  const real = { ...line("", 0, 1000), words: [{ word: "A ", startTime: 0, endTime: 400 }, { word: "B", startTime: 400, endTime: 1000 }] };
  const bg = { ...line("(ooh)", 500, 900), isBG: true };
  const plain = line("你好", 2000, 4000);
  const auto = applyWordMode([real, bg, plain], "estimate");
  assert.equal(auto[0], real);
  assert.equal(auto[1], bg);
  assert.equal(auto[2].words.length, 2);
  assert.deepEqual(applyWordMode([real, plain], "native"), [real, plain]);
  const collapsed = applyWordMode([real], "line");
  assert.deepEqual(collapsed[0].words, [{ word: "A B", startTime: 0, endTime: 1000 }]);
});

test("script conversion of word-timed lines uses whole-line context", () => {
  const phrase = (text) => text.replace("头发", "頭髮").replace(/发/g, "發");
  const [converted] = convertLyricLinesWith([{ words: [{ word: "头" }, { word: "发" }], translatedLyric: "" }], phrase);
  assert.deepEqual(converted.words.map((word) => word.word), ["頭", "髮"]);
});

test("native word timing is the default and line-only lyrics are left alone", () => {
  const plain = line("你好", 2000, 4000);
  assert.deepEqual(applyWordMode([plain]), [plain]);
});
