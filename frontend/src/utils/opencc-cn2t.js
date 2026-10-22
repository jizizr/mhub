import OpenCC from "opencc-js/cn2t";
import { convertLyricLinesWith, hasLyricScriptDifferenceWith } from "./opencc-lines.js";

const converter = OpenCC.Converter({ from: "cn", to: "tw" });

export function convertLyricLines(lines) {
  return convertLyricLinesWith(lines, converter);
}

export function hasLyricScriptDifference(lines) {
  return hasLyricScriptDifferenceWith(lines, converter);
}
