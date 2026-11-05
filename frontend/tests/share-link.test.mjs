import test from "node:test";
import assert from "node:assert/strict";
import { buildShareUrl, canShareTrack, parseShareParams, shareSourceId, stripShareParams } from "../src/utils/share.js";

test("a share link round-trips the song identity and labels", () => {
  const track = { platform: "applemusic", source_id: "1440857781", title: "晴天 / Sunny", artist: "周杰伦", cover: "https://is1-ssl.mzstatic.com/a/600x600bb.jpg" };
  const url = buildShareUrl(track, "https://music.example.com/some/page?x=1");
  assert.ok(url.startsWith("https://music.example.com/?song=applemusic%3A1440857781"));
  assert.deepEqual(parseShareParams(new URL(url).search), {
    platform: "applemusic",
    id: "1440857781",
    title: "晴天 / Sunny",
    artist: "周杰伦",
    cover: "https://is1-ssl.mzstatic.com/a/600x600bb.jpg",
  });
});

test("the source id falls back to the playback id", () => {
  assert.equal(shareSourceId({ id: "tencent--002abc--lossless" }), "002abc");
  assert.equal(shareSourceId({ id: "netease--1--2--high" }), "1--2");
});

test("server-relative covers and bad input are left out", () => {
  const url = buildShareUrl({ platform: "netease", source_id: "1", cover: "/api/v1/playbacks/x/cover" }, "https://h.test/");
  assert.equal(new URL(url).searchParams.has("cover"), false);
  assert.equal(parseShareParams("?song=netease:1&cover=javascript:alert(1)").cover, "");
  assert.equal(parseShareParams("?song=bad platform:1"), null);
  assert.equal(parseShareParams("?song=netease:"), null);
  assert.equal(parseShareParams(""), null);
  assert.equal(canShareTrack({ platform: "netease" }), false);
});

test("stripping keeps unrelated parameters and the hash", () => {
  assert.equal(stripShareParams("https://h.test/?song=a:1&title=t&keep=1#x"), "/?keep=1#x");
  assert.equal(stripShareParams("https://h.test/?song=a:1"), "/");
});
