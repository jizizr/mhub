import assert from "node:assert/strict";
import test from "node:test";
import { displacementMap, supportsLiquidGlass } from "../src/utils/liquid-glass.js";

const pixel = (map, x, y) => {
  const index = (y * map.width + x) * 4;
  return [map.data[index], map.data[index + 1]];
};

test("flat middle of the glass does not bend the backdrop", () => {
  const map = displacementMap(400, 80, { radius: 24, bezel: 18, shift: 12 });
  assert.deepEqual([map.width, map.height, map.scale], [400, 80, 24]);
  const [r, g] = pixel(map, 200, 40);
  assert.ok(Math.abs(r - 128) <= 1 && Math.abs(g - 128) <= 1);
});

test("edges sample inward, strongest at the rim", () => {
  const map = displacementMap(400, 80, { radius: 24, bezel: 18, shift: 12 });
  // Left edge pulls from the right (red above neutral); right edge the reverse.
  assert.ok(pixel(map, 0, 40)[0] > 200);
  assert.ok(pixel(map, 399, 40)[0] < 56);
  // Top edge pulls from below (green above neutral); bottom edge the reverse.
  assert.ok(pixel(map, 200, 0)[1] > 200);
  assert.ok(pixel(map, 200, 79)[1] < 56);
  // The shift fades across the bezel.
  assert.ok(pixel(map, 4, 40)[0] > pixel(map, 12, 40)[0]);
  assert.ok(Math.abs(pixel(map, 20, 40)[0] - 128) <= 1);
});

test("rounded corners bend along their diagonal", () => {
  const map = displacementMap(200, 80, { radius: 40, bezel: 18, shift: 12 });
  const [r, g] = pixel(map, 8, 8);
  assert.ok(r > 140 && g > 140, "top-left corner pulls towards the centre on both axes");
});

test("only Chromium browsers get the SVG backdrop filter", () => {
  assert.equal(supportsLiquidGlass({ userAgentData: { brands: [{ brand: "Chromium" }, { brand: "Microsoft Edge" }] } }), true);
  assert.equal(supportsLiquidGlass({ userAgent: "Mozilla/5.0 (Macintosh) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15" }), false);
  assert.equal(supportsLiquidGlass({ userAgent: "Mozilla/5.0 (iPhone) AppleWebKit/605.1.15 CriOS/140.0 Mobile Safari/604.1" }), false);
  assert.equal(supportsLiquidGlass({ userAgent: "Mozilla/5.0 (X11; Linux) Gecko/20100101 Firefox/140.0" }), false);
  assert.equal(supportsLiquidGlass({ userAgent: "Mozilla/5.0 (Linux; Android 16) AppleWebKit/537.36 Chrome/140.0 Mobile Safari/537.36" }), true);
});
