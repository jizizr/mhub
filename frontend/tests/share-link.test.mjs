import test from "node:test";
import assert from "node:assert/strict";
import { buildShareUrl, canShareTrack, lyricLineText, parseShareParams, shareSourceId, stripShareParams } from "../src/utils/share.js";

test("a share link carries only the platform and song id", () => {
  const track = { platform: "applemusic", source_id: "1535791948", title: "交换余生", artist: "林俊杰", cover: "https://is1-ssl.mzstatic.com/a.jpg" };
  const url = buildShareUrl(track, "https://music.example.com/some/page?x=1#y");
  assert.equal(url, "https://music.example.com/?song=applemusic%3A1535791948");
  assert.deepEqual(parseShareParams(new URL(url).search), { platform: "applemusic", id: "1535791948" });
});

test("labels in a link are ignored; only the id identifies the song", () => {
  assert.deepEqual(
    parseShareParams("?song=applemusic%3A1535791948&title=%E5%81%87&artist=x&cover=https%3A%2F%2Fevil.test%2Fa.jpg"),
    { platform: "applemusic", id: "1535791948" },
  );
});

test("the source id falls back to the playback id", () => {
  assert.equal(shareSourceId({ id: "tencent--002abc--lossless" }), "002abc");
  assert.equal(shareSourceId({ id: "netease--1--2--high" }), "1--2");
});

test("unknown platforms and malformed ids are rejected", () => {
  assert.equal(parseShareParams("?song=bilibili:1"), null);
  assert.equal(parseShareParams("?song=netease:1/2"), null);
  assert.equal(parseShareParams("?song=netease:"), null);
  assert.equal(parseShareParams(`?song=netease:${"9".repeat(65)}`), null);
  assert.equal(parseShareParams(""), null);
  assert.equal(canShareTrack({ platform: "netease" }), false);
  assert.equal(canShareTrack({ platform: "bilibili", source_id: "1" }), false);
  assert.equal(canShareTrack({ platform: "tencent", source_id: "0039MnYb0qxYhV" }), true);
});

test("stripping removes current and legacy share parameters only", () => {
  assert.equal(stripShareParams("https://h.test/?song=a:1&title=t&cover=c&keep=1#x"), "/?keep=1#x");
  assert.equal(stripShareParams("https://h.test/?song=a:1"), "/");
});

test("lyric lines become plain text", () => {
  assert.equal(lyricLineText({ words: [{ word: "Hello " }, { word: " world" }] }), "Hello world");
  assert.equal(lyricLineText({ words: [{ word: "交换" }, { word: "余生" }] }), "交换余生");
  assert.equal(lyricLineText({ words: [] }), "");
});
