// Convert word-timed lines as a whole so phrase rules (e.g. 头发 → 頭髮) still
// see their context, then hand the characters back to their words. OpenCC
// rarely changes the length; when it does, fall back to word by word.
function convertWords(words, converter) {
  const sources = words.map((word) => [...String(word.word || "")]);
  const converted = [...converter(sources.map((chars) => chars.join("")).join(""))];
  if (converted.length !== sources.reduce((total, chars) => total + chars.length, 0)) {
    return words.map((word) => ({ ...word, word: converter(word.word || "") }));
  }
  let offset = 0;
  return words.map((word, index) => {
    const length = sources[index].length;
    const text = converted.slice(offset, offset + length).join("");
    offset += length;
    return { ...word, word: text };
  });
}

export function convertLyricLinesWith(lines, converter) {
  if (!converter) return lines || [];
  return (lines || []).map((line) => ({
    ...line,
    translatedLyric: converter(line.translatedLyric || ""),
    words: Array.isArray(line.words) ? convertWords(line.words, converter) : line.words,
  }));
}

export function hasLyricScriptDifferenceWith(lines, converter) {
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
