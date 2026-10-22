import { convertLyricLinesWith, hasLyricScriptDifferenceWith } from "./opencc-lines.js";

export function lyricConverterModuleUrl(url, attempt) {
  if (!Number.isSafeInteger(attempt) || attempt <= 0) return url;
  const hashIndex = url.indexOf("#");
  const path = hashIndex < 0 ? url : url.slice(0, hashIndex);
  const hash = hashIndex < 0 ? "" : url.slice(hashIndex);
  return `${path}${path.includes("?") ? "&" : "?"}opencc_retry=${attempt}${hash}`;
}

export function wrapLyricConverter(OpenCC, script) {
  const converter = OpenCC.Converter(script === "simplified"
    ? { from: "tw", to: "cn" }
    : { from: "cn", to: "tw" });
  return {
    convertLyricLines: (lines) => convertLyricLinesWith(lines, converter),
    hasLyricScriptDifference: (lines) => hasLyricScriptDifferenceWith(lines, converter),
  };
}

export function createLyricConverterLoader(importers) {
  const pending = new Map();
  const attempts = new Map();
  return function loadConverter(script) {
    if (script !== "simplified" && script !== "traditional") return Promise.resolve(null);
    if (pending.has(script)) return pending.get(script);

    const attempt = attempts.get(script) || 0;
    const request = Promise.resolve().then(() => importers[script](attempt)).catch((error) => {
      if (pending.get(script) === request) {
        pending.delete(script);
        // Browsers cache rejected module imports by URL. The next explicit
        // request gets a new URL; concurrent consumers still share one request.
        attempts.set(script, attempt + 1);
      }
      throw error;
    });
    pending.set(script, request);
    return request;
  };
}
