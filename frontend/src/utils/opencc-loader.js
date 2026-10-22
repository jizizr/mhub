// These package entries are self-contained ESM files with no child imports.
// Vite emits content-hashed URLs, without downloading either dictionary until
// conversion is requested. A retry changes the URL to bypass failed module maps.
import simplifiedUrl from "opencc-js/t2cn?url";
import traditionalUrl from "opencc-js/cn2t?url";
import { createLyricConverterLoader, lyricConverterModuleUrl, wrapLyricConverter } from "./opencc-loader-core.js";

async function importConverter(url, script, attempt) {
  const OpenCC = await import(/* @vite-ignore */ lyricConverterModuleUrl(url, attempt));
  return wrapLyricConverter(OpenCC, script);
}

// The toggle and renderer share the same lazy request and initialized dictionary.
export const loadLyricConverter = createLyricConverterLoader({
  simplified: (attempt) => importConverter(simplifiedUrl, "simplified", attempt),
  traditional: (attempt) => importConverter(traditionalUrl, "traditional", attempt),
});
