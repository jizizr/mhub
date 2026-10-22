import test from "node:test";
import assert from "node:assert/strict";
import { createLyricConverterLoader, lyricConverterModuleUrl, wrapLyricConverter } from "../src/utils/opencc-loader-core.js";

const loadLyricConverter = createLyricConverterLoader({
  simplified: async () => wrapLyricConverter(await import("opencc-js/t2cn"), "simplified"),
  traditional: async () => wrapLyricConverter(await import("opencc-js/cn2t"), "traditional"),
});

const traditional = "愛與夢想，臺灣的音樂。";
const simplified = "爱与梦想，台湾的音乐。";

function lyricLine(text) {
  return {
    startTime: 1200,
    endTime: 4200,
    isBG: false,
    translatedLyric: "隨著風，聽見夢想。",
    romanLyric: "Ai yu meng xiang",
    words: [{ startTime: 1200, endTime: 4200, word: text, romanWord: "Ai yu meng xiang" }],
  };
}

test("directional OpenCC chunks convert both lyric text and translation", async () => {
  const converter = await loadLyricConverter("simplified");
  const [line] = converter.convertLyricLines([lyricLine(traditional)]);
  assert.equal(line.words[0].word, simplified);
  assert.equal(line.translatedLyric, "随着风，听见梦想。");
  assert.equal(converter.hasLyricScriptDifference([lyricLine(traditional)]), true);
});

test("the reverse directional chunk restores traditional Chinese", async () => {
  const converter = await loadLyricConverter("traditional");
  const [line] = converter.convertLyricLines([lyricLine(simplified)]);
  assert.equal(line.words[0].word, traditional);
  assert.equal(converter.hasLyricScriptDifference([lyricLine(simplified)]), true);
});

test("conversion preserves word timing, romanization and the original source", async () => {
  const converter = await loadLyricConverter("simplified");
  const line = lyricLine(traditional);
  Object.freeze(line.words[0]);
  Object.freeze(line.words);
  Object.freeze(line);
  const source = Object.freeze([line]);
  const converted = converter.convertLyricLines(source);
  assert.notEqual(converted, source);
  assert.notEqual(converted[0].words[0], source[0].words[0]);
  assert.equal(source[0].words[0].word, traditional);
  assert.equal(source[0].translatedLyric, "隨著風，聽見夢想。");
  assert.deepEqual(
    { ...converted[0], words: undefined, translatedLyric: undefined },
    { ...source[0], words: undefined, translatedLyric: undefined },
  );
  assert.deepEqual(
    { ...converted[0].words[0], word: undefined },
    { ...source[0].words[0], word: undefined },
  );
});

test("difference detection handles translation-only lyrics and unchanged scripts", async () => {
  const converter = await loadLyricConverter("simplified");
  assert.equal(converter.hasLyricScriptDifference([{ translatedLyric: traditional }]), true);
  assert.equal(converter.hasLyricScriptDifference([{ words: [{ word: simplified }] }]), false);
  assert.equal(converter.hasLyricScriptDifference([{ words: [{ word: "music / ひかり / 음악" }] }]), false);
  assert.equal(converter.hasLyricScriptDifference([]), false);
});

test("original and unknown script settings do not load dictionaries", async () => {
  let calls = 0;
  const load = createLyricConverterLoader({
    simplified: async () => { calls += 1; },
    traditional: async () => { calls += 1; },
  });
  for (const script of ["original", "", undefined, "unexpected", "toString"]) {
    assert.equal(await load(script), null);
  }
  assert.equal(calls, 0);
});

test("renderer and toggle share one request per direction", async () => {
  let simplifiedCalls = 0;
  let traditionalCalls = 0;
  const simplifiedModule = {};
  const traditionalModule = {};
  const load = createLyricConverterLoader({
    simplified: async () => { simplifiedCalls += 1; return simplifiedModule; },
    traditional: async () => { traditionalCalls += 1; return traditionalModule; },
  });
  const renderer = load("simplified");
  const toggle = load("simplified");
  assert.equal(renderer, toggle);
  assert.equal(await renderer, simplifiedModule);
  assert.equal(await load("traditional"), traditionalModule);
  assert.equal(await load("simplified"), simplifiedModule);
  assert.equal(simplifiedCalls, 1);
  assert.equal(traditionalCalls, 1);
});

test("a failed dictionary request is shared then cleared for retry", async () => {
  let calls = 0;
  const attempts = [];
  const module = {};
  const load = createLyricConverterLoader({
    simplified: async (attempt) => {
      calls += 1;
      attempts.push(attempt);
      if (calls === 1) throw new Error("temporary dictionary network error");
      return module;
    },
  });
  const first = load("simplified");
  const simultaneous = load("simplified");
  assert.equal(first, simultaneous);
  await assert.rejects(first, /temporary dictionary network error/);
  const retry = load("simplified");
  assert.notEqual(retry, first);
  assert.equal(await retry, module);
  assert.equal(calls, 2);
  assert.deepEqual(attempts, [0, 1]);
});

test("failure in one script does not discard the other script's dictionary", async () => {
  let calls = 0;
  const module = {};
  const load = createLyricConverterLoader({
    simplified: async () => { throw new Error("unavailable"); },
    traditional: async () => { calls += 1; return module; },
  });
  assert.equal(await load("traditional"), module);
  await assert.rejects(load("simplified"), /unavailable/);
  assert.equal(await load("traditional"), module);
  assert.equal(calls, 1);
});

test("a retry changes only the cache-busting query on the hashed module URL", () => {
  const url = "/assets/t2cn-exampleHash.js";
  assert.equal(lyricConverterModuleUrl(url, 0), url);
  assert.equal(lyricConverterModuleUrl(url, 1), `${url}?opencc_retry=1`);
  assert.equal(lyricConverterModuleUrl(url, 2), `${url}?opencc_retry=2`);
  assert.equal(lyricConverterModuleUrl(`${url}?v=build#module`, 1), `${url}?v=build&opencc_retry=1#module`);
  assert.equal(lyricConverterModuleUrl(url, undefined), url);
});

test("repeated failures advance that direction's URL and successful imports stay cached", async () => {
  const attempts = [];
  const module = {};
  const load = createLyricConverterLoader({
    simplified: async (attempt) => {
      attempts.push(["simplified", attempt]);
      if (attempt < 2) throw new Error("offline");
      return module;
    },
    traditional: async (attempt) => { attempts.push(["traditional", attempt]); return module; },
  });
  await assert.rejects(load("simplified"), /offline/);
  await assert.rejects(load("simplified"), /offline/);
  assert.equal(await load("traditional"), module);
  assert.equal(await load("simplified"), module);
  assert.equal(await load("simplified"), module);
  assert.deepEqual(attempts, [["simplified", 0], ["simplified", 1], ["traditional", 0], ["simplified", 2]]);
});
