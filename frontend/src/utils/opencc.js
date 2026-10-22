import OpenCC from "opencc-js";

const toSimplified = OpenCC.Converter({ from: "tw", to: "cn" });
const toTraditional = OpenCC.Converter({ from: "cn", to: "tw" });

function converterFor(script) {
  if (script === "simplified") return toSimplified;
  if (script === "traditional") return toTraditional;
  return null;
}

export function convertLyricText(text, script) {
  const value = String(text || "");
  const converter = converterFor(script);
  return converter && value ? converter(value) : value;
}

export function convertLyricLines(lines, script) {
  const converter = converterFor(script);
  if (!converter) return lines || [];
  return (lines || []).map((line) => ({
    ...line,
    translatedLyric: converter(line.translatedLyric || ""),
    words: Array.isArray(line.words)
      ? line.words.map((word) => ({
          ...word,
          word: converter(word.word || ""),
        }))
      : line.words,
  }));
}

export function hasLyricScriptDifference(lines, script = "simplified") {
  const converter = converterFor(script);
  if (!converter) return false;
  return (lines || []).some((line) => {
    const words = Array.isArray(line.words) ? line.words : [];
    return words.some((word) => {
      const source = String(word.word || "");
      return source && converter(source) !== source;
    }) || (() => {
      const source = String(line.translatedLyric || "");
      return source && converter(source) !== source;
    })();
  });
}
