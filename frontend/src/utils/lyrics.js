import { parseLrc, parseTTML } from "@applemusic-like-lyrics/lyric";

function detect(text) {
  const source = String(text || "").trim();
  if (/<tt[\s>]/i.test(source)) return "ttml";
  if (/\[\d{1,2}:\d{2}(?:[.:]\d{1,3})?\]/.test(source)) return "lrc";
  return "plain";
}

function parseAny(text) {
  const source = String(text || "").trim();
  if (!source) return [];
  const kind = detect(source);
  if (kind === "ttml") return parseTTML(source).lines;
  if (kind === "lrc") return parseLrc(source);
  return source
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line, index) => {
      const startTime = index * 4000;
      return {
        words: [{ word: line, startTime, endTime: startTime + 3800 }],
        startTime,
        endTime: startTime + 3800,
        translatedLyric: "",
        romanLyric: "",
        isBG: false,
        isDuet: false,
      };
    });
}

function textOf(line) {
  return line.words?.map((word) => word.word).join("") ?? "";
}

function normalize(lines) {
  return lines
    .filter((line) => textOf(line).trim())
    .map((line, index, all) => {
      const startTime = line.words?.[0]?.startTime ?? line.startTime ?? index * 4000;
      const next = all[index + 1]?.words?.[0]?.startTime ?? all[index + 1]?.startTime;
      const endTime = Number.isFinite(line.endTime)
        ? line.endTime
        : Number.isFinite(next)
          ? next
          : startTime + 5000;
      return {
        words: [{ word: textOf(line), startTime, endTime }],
        startTime,
        endTime,
        translatedLyric: line.translatedLyric ?? "",
        romanLyric: line.romanLyric ?? "",
        isBG: Boolean(line.isBG),
        isDuet: Boolean(line.isDuet),
      };
    });
}

// Word-timed lines from the API, in AMLL's shape. Background vocals follow
// the main line they accompany.
function wordLines(lines) {
  return lines
    .map((line) => ({
      words: line.words.map(([startTime, endTime, word]) => ({ word, startTime, endTime })),
      startTime: line.start,
      endTime: line.end,
      translatedLyric: "",
      romanLyric: "",
      isBG: Boolean(line.bg),
      isDuet: false,
    }))
    .filter((line) => textOf(line).trim());
}

// Attach a side track (translation or romanization) to the nearest unused
// line within a second; background vocals keep only their own text.
function mergeSide(lines, sideText, field) {
  const translations = normalize(parseAny(sideText));
  if (!translations.length) return lines;
  const used = new Set();
  return lines.map((line) => {
    if (line.isBG) return line;
    let best = -1;
    let distance = 1001;
    translations.forEach((translation, index) => {
      const delta = Math.abs(translation.startTime - line.startTime);
      if (!used.has(index) && delta < distance) {
        best = index;
        distance = delta;
      }
    });
    if (best >= 0) used.add(best);
    return {
      ...line,
      [field]: line[field] || (best >= 0 ? textOf(translations[best]).trim() : ""),
    };
  });
}

function withSides(lines, payload) {
  const translated = mergeSide(lines, payload.translation, "translatedLyric");
  return payload.roman ? mergeSide(translated, payload.roman, "romanLyric") : translated;
}

function parseLines(payload) {
  try {
    if (Array.isArray(payload.words) && payload.words.length) {
      const lines = wordLines(payload.words);
      if (lines.length) return withSides(lines, payload);
    }
    if (!String(payload.plain || "").trim()) return normalize(parseAny(payload.translation));
    return withSides(normalize(parseAny(payload.plain)), payload);
  } catch (error) {
    console.warn("[lyrics] parse failed", error);
    return normalize(parseAny(payload.plain || payload.translation));
  }
}

/**
 * AMLL lines for a lyrics payload. Credits ("作词：…") travel with the cached
 * lines as an extra `credits` property so every cache layer keeps them.
 */
export function buildLyricLines(payload) {
  if (!payload?.available) return [];
  const lines = parseLines(payload);
  const credits = Array.isArray(payload.credits) ? payload.credits : [];
  if (lines.length && credits.length) lines.credits = credits;
  return lines;
}
