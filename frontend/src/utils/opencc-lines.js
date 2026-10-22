export function convertLyricLinesWith(lines, converter) {
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
