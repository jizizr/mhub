import test from "node:test";
import assert from "node:assert/strict";
import { needsMediaCompanion } from "../src/core/player/playback-platform.js";

test("Android browsers get the silent media companion, by UA or Client Hints", () => {
  assert.equal(needsMediaCompanion({ navigatorObject: {
    userAgent: "Mozilla/5.0 (Linux; Android 15; Test Phone) AppleWebKit/537.36 Chrome/130.0.0.0 Mobile Safari/537.36",
  } }), true);
  assert.equal(needsMediaCompanion({ navigatorObject: { userAgentData: { platform: "Android" }, userAgent: "Mozilla/5.0" } }), true);
  assert.equal(needsMediaCompanion({ navigatorObject: {
    onLine: false, userAgent: "Mozilla/5.0 (Linux; Android 15) Chrome/130.0 Mobile Safari/537.36",
  } }), true, "offline playback needs the system controls too");
});

test("other browsers and restricted navigators get no companion", () => {
  for (const navigatorObject of [
    null,
    {},
    { userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)" },
    { userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" },
    { userAgentData: { platform: "Windows" }, userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" },
    { get userAgent() { throw new Error("restricted"); } },
  ]) {
    assert.equal(needsMediaCompanion({ navigatorObject }), false);
  }
});
