// Shared checks behind the lyric script (简体/繁體) toggles.

const HAN = /\p{Script=Han}/u;
// Japanese lyrics also use Han characters; kana marks them as not Chinese.
const KANA = /[\p{Script=Hiragana}\p{Script=Katakana}]/u;
// Under "original" no dictionary is downloaded just to label a toggle:
// frequent Traditional-only characters tell Traditional lyrics apart.
const TRADITIONAL_ONLY = /[這們個來說會為與對時國學還進過裡體麼後發開關問題經點應當見愛聽讓從誰樣頭邊壞夢歲謝謊輕嘗給濃歡樣溫護擔擁淚憶戀遠離總覺讀寫話語]/u;

export function lyricLineSource(line) {
  const words = Array.isArray(line?.words) ? line.words.map((word) => String(word.word || "")) : [];
  return [...words, String(line?.translatedLyric || "")].join("");
}

export function lyricsAreChinese(lines) {
  const text = (lines || []).map(lyricLineSource).join("");
  return HAN.test(text) && !KANA.test(text);
}

export function lyricsLookTraditional(lines) {
  return (lines || []).some((line) => TRADITIONAL_ONLY.test(lyricLineSource(line)));
}

// The script a toggle switches to from the current one.
export function lyricToggleTarget(script, looksTraditional) {
  if (script === "simplified") return "traditional";
  if (script === "traditional") return "simplified";
  return looksTraditional ? "simplified" : "traditional";
}
