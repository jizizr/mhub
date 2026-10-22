import OpenCC from "opencc-js/t2cn";
import { convertLyricLinesWith, hasLyricScriptDifferenceWith } from "./opencc-lines.js";

const converter = OpenCC.Converter({ from: "tw", to: "cn" });

export function convertLyricLines(lines) {
  return convertLyricLinesWith(lines, converter);
}

export function hasLyricScriptDifference(lines) {
  return hasLyricScriptDifferenceWith(lines, converter);
}
