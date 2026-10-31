// Word timing for the lyric renderer. Real word timing (YRC/QRC/KRC/TTML from
// the API) is used as is; line-only lyrics can get an estimate that spreads
// each line over its syllables, as HyperLyrics-Enhanced does when splitting
// LRC lines: time is proportional to sung units, not to elapsed line time.

// The estimate is uniform within each line. Measured against real word timing
// (3 songs, 1,138 words) only 32% of its word starts land within 200 ms;
// onset detection on the audio only reached 46%, so neither is good enough to
// be the default and "native" is.
export const WORD_MODES = ["native", "estimate", "line"];

// A sung unit lasts at most this long, so an interlude before the next line
// does not stretch the sweep across the silence.
const MAX_UNIT_MS = 600;
const MIN_UNIT_MS = 120;
// Leave the end of the line gap unsung; singers breathe before the next line.
const SUNG_SHARE = 0.9;

const SYLLABIC = /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/u;
const LETTER = /[\p{L}\p{N}'’]/u;

function syllables(word) {
  const groups = word.toLowerCase().match(/[aeiouyàáâãäåèéêëìíîïòóôõöùúûüāēīōū]+/g);
  return Math.min(4, Math.max(1, groups?.length || 1));
}

// Split a line into sung units: one per CJK/kana/hangul character, one per
// Latin word (weighted by its vowel groups). Spaces and punctuation stay with
// the unit before them, or the first unit when they lead the line.
export function sungUnits(text) {
  const units = [];
  let prefix = "";
  const chars = [...String(text || "")];
  for (let index = 0; index < chars.length; ) {
    const char = chars[index];
    if (SYLLABIC.test(char)) {
      units.push({ text: prefix + char, weight: 1 });
      prefix = "";
      index += 1;
    } else if (LETTER.test(char)) {
      let word = "";
      while (index < chars.length && LETTER.test(chars[index]) && !SYLLABIC.test(chars[index])) {
        word += chars[index];
        index += 1;
      }
      units.push({ text: prefix + word, weight: syllables(word) });
      prefix = "";
    } else {
      if (units.length) units[units.length - 1].text += char;
      else prefix += char;
      index += 1;
    }
  }
  if (prefix) units.push({ text: prefix, weight: 0 });
  return units;
}

export function estimateLineWords(line, nextStart) {
  const text = line.words.map((word) => word.word).join("");
  const units = sungUnits(text);
  const weight = units.reduce((total, unit) => total + unit.weight, 0);
  if (units.length < 2 || weight <= 0) return line;
  const start = line.startTime;
  const limit = Number.isFinite(nextStart) && nextStart > start ? nextStart : line.endTime;
  const available = Math.max(0, limit - start) * SUNG_SHARE;
  const unit = Math.min(MAX_UNIT_MS, Math.max(MIN_UNIT_MS, available / weight));
  let time = start;
  const words = units.map(({ text: word, weight: share }) => {
    const startTime = Math.round(time);
    time += unit * share;
    return { word, startTime, endTime: Math.max(startTime + 1, Math.round(time)) };
  });
  return { ...line, words, estimatedWords: true };
}

// `native`: real word timing only. `estimate`: real word timing, estimated
// for lines that have none. `line`: whole-line highlighting.
export function applyWordMode(lines, mode = "native") {
  if (mode === "line") {
    return lines.map((line) => {
      if (!Array.isArray(line.words) || line.words.length <= 1) return line;
      const word = line.words.map((item) => item.word).join("");
      return { ...line, words: [{ word, startTime: line.startTime, endTime: line.endTime }] };
    });
  }
  if (mode !== "estimate") return lines;
  const mainStarts = lines.map((line) => (line.isBG ? null : line.startTime));
  return lines.map((line, index) => {
    if (line.isBG || !Array.isArray(line.words) || line.words.length !== 1) return line;
    const nextStart = mainStarts.slice(index + 1).find((value) => Number.isFinite(value));
    return estimateLineWords(line, nextStart);
  });
}
