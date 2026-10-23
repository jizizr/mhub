// Regenerates src/data/zh_fold.txt: single-character Traditional (incl. Taiwan
// and Hong Kong variants) -> Simplified pairs from OpenCC's dictionaries, as
// shipped by the frontend's opencc-js dependency. The backend uses it only to
// compare song titles and artist names across platforms.
//
//   node scripts/gen_zh_fold.mjs
import { writeFile } from "node:fs/promises";

const dict = (name) => import(`../frontend/node_modules/opencc-js/dist/esm-lib/dict/${name}.js`).then((module) => module.default);
const pairs = (data) => new Map(data.split("|").map((entry) => {
  const [from, to] = entry.split(" ");
  return [from, to.split(/\s+/)[0]];
}).filter(([from, to]) => [...from].length === 1 && [...to].length === 1));

const toSimplified = pairs(await dict("TSCharacters"));
const variants = new Map([...pairs(await dict("TWVariantsRev")), ...pairs(await dict("HKVariantsRev"))]);
const fold = new Map(toSimplified);
for (const [variant, standard] of variants) {
  const simplified = toSimplified.get(standard) || standard;
  if (simplified !== variant) fold.set(variant, simplified);
}
const body = [...fold].filter(([from, to]) => from !== to)
  .sort(([left], [right]) => left.codePointAt(0) - right.codePointAt(0))
  .map(([from, to]) => from + to)
  .join("");
await writeFile(new URL("../src/data/zh_fold.txt", import.meta.url), `${body}\n`);
console.log(`${fold.size} pairs`);
