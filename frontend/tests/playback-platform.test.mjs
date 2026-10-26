import test from "node:test";
import assert from "node:assert/strict";
import { shouldPreferNativeBackgroundPlayback } from "../src/core/player/playback-platform.js";

const android = {
  onLine: true,
  userAgent: "Mozilla/5.0 (Linux; Android 15; Test Phone) AppleWebKit/537.36 Chrome/130.0.0.0 Mobile Safari/537.36",
};

test("online Android with native FLAC prefers the native transport without requiring UA Client Hints", () => {
  assert.equal(shouldPreferNativeBackgroundPlayback({ navigatorObject: android, canPlayNative: () => true }), true);
  assert.equal(shouldPreferNativeBackgroundPlayback({ navigatorObject: android, canPlayNative: () => "probably" }), true);
  assert.equal(shouldPreferNativeBackgroundPlayback({ navigatorObject: android, canPlayNative: () => "maybe" }), true);
});

test("Android Client Hints are recognized even with a desktop or reduced UA", () => {
  assert.equal(shouldPreferNativeBackgroundPlayback({
    navigatorObject: { onLine: true, userAgentData: { platform: "Android" }, userAgent: "Mozilla/5.0" },
    canPlayNative: () => true,
  }), true);
});

test("offline Android retains the existing packet cache path without probing native playback", () => {
  let probes = 0;
  assert.equal(shouldPreferNativeBackgroundPlayback({
    navigatorObject: { ...android, onLine: false },
    canPlayNative: () => { probes += 1; return true; },
  }), false);
  assert.equal(probes, 0);
});

test("Audio Session API presence alone does not prove Android background media ownership", () => {
  let probes = 0;
  assert.equal(shouldPreferNativeBackgroundPlayback({
    navigatorObject: { ...android, audioSession: { type: "auto" } },
    canPlayNative: () => { probes += 1; return true; },
  }), true);
  assert.equal(probes, 1);
});

test("non-Android browsers are untouched and never perform the FLAC probe", () => {
  let probes = 0;
  for (const navigatorObject of [
    {},
    { userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)" },
    { userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" },
    { userAgentData: { platform: "Windows" }, userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" },
    { userAgentData: { platform: "Linux" }, userAgent: "Mozilla/5.0 (X11; Linux x86_64)" },
  ]) {
    assert.equal(shouldPreferNativeBackgroundPlayback({ navigatorObject, canPlayNative: () => { probes += 1; return true; } }), false);
  }
  assert.equal(probes, 0);
});

test("unsupported native FLAC or restricted browser capabilities safely keep the current transport", () => {
  for (const support of [false, "", undefined, null, "no"]) {
    assert.equal(shouldPreferNativeBackgroundPlayback({ navigatorObject: android, canPlayNative: () => support }), false);
  }
  assert.equal(shouldPreferNativeBackgroundPlayback({ navigatorObject: android, canPlayNative: () => { throw new Error("restricted"); } }), false);
  assert.equal(shouldPreferNativeBackgroundPlayback({ navigatorObject: { get onLine() { throw new Error("restricted"); } }, canPlayNative: () => true }), false);
  assert.equal(shouldPreferNativeBackgroundPlayback({ navigatorObject: null, canPlayNative: () => true }), false);
});

test("the default codec probe checks actual browser support for FLAC", (t) => {
  const previous = globalThis.document;
  const requestedTypes = [];
  const requestedTags = [];
  let support = "probably";
  globalThis.document = { createElement(tag) {
    requestedTags.push(tag);
    return { canPlayType(type) { requestedTypes.push(type); return support; } };
  } };
  t.after(() => { if (previous === undefined) delete globalThis.document; else globalThis.document = previous; });
  assert.equal(shouldPreferNativeBackgroundPlayback({ navigatorObject: android }), true);
  support = "";
  assert.equal(shouldPreferNativeBackgroundPlayback({ navigatorObject: android }), false);
  assert.deepEqual(requestedTypes, ["audio/flac", "audio/flac"]);
  assert.deepEqual(requestedTags, ["audio", "audio"]);
});
