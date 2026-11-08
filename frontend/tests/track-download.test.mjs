import test from "node:test";
import assert from "node:assert/strict";
import { cacheKey } from "../src/services/localCache.js";
import { downloadFilename, findCachedDownload, resolveTrackDownload, sniffAudioExt } from "../src/services/track-download.js";

const bytes = (text, length = 32) => {
  const out = new Uint8Array(length);
  out.set([...text].map((c) => c.charCodeAt(0)));
  return out;
};
const m4a = (() => { const b = new Uint8Array(32); b.set([0, 0, 0, 0x20, 0x66, 0x74, 0x79, 0x70]); return b; })();
const native = { id: "applemusic--1--high", playback_id: "applemusic--1--high", platform: "applemusic", source_id: "1", selected_id: "1", quality: "high", title: "歌/名", artist: "歌手", status: "completed", streaming_mode: "" };
const alac = { ...native, id: "applemusic--1--lossless", playback_id: "applemusic--1--lossless", quality: "lossless", streaming_mode: "wasm_alac", streaming_url: "/api/v1/playbacks/applemusic--1--lossless/audio/stream" };
const record = (track, blob) => ({ key: cacheKey(track), task: { ...track }, track: { ...track }, blob });
const noNetwork = {
  api: { getPlayback: () => assert.fail("no status request"), createPlayback: () => assert.fail("no task request") },
  fetchImpl: () => assert.fail("no download request"),
};

test("container sniffing accepts playable files and rejects the ALAC packet stream", () => {
  assert.equal(sniffAudioExt(bytes("fLaC")), "flac");
  assert.equal(sniffAudioExt(m4a), "m4a");
  assert.equal(sniffAudioExt(bytes("ID3")), "mp3");
  assert.equal(sniffAudioExt(new Uint8Array([0xff, 0xfb, 0x90, 0])), "mp3");
  assert.equal(sniffAudioExt(bytes("OggS")), "ogg");
  assert.equal(sniffAudioExt(bytes("SPLALAC1")), null);
  assert.equal(sniffAudioExt(bytes("<html>")), null);
});

test("file names are safe and keep title and artist", () => {
  assert.equal(downloadFilename({ title: "歌/名: A?", artist: "歌手" }, "flac"), "歌 名 A - 歌手.flac");
  assert.equal(downloadFilename({ title: "" }, "m4a"), "未知歌曲.m4a");
  assert.ok(downloadFilename({ title: "长".repeat(300), artist: "x" }, "mp3").length <= 124);
});

test("a fully cached native track is saved from IndexedDB without any request", async () => {
  const stored = record(native, new Blob([m4a], { type: "" }));
  const result = await resolveTrackDownload(native, { lookup: async (key) => (key === stored.key ? stored : null), ...noNetwork });
  assert.equal(result.fromCache, true);
  assert.equal(result.filename, "歌 名 - 歌手.m4a");
  assert.equal(result.blob.type, "audio/mp4");
  assert.equal(result.blob.size, m4a.length);
});

test("an ALAC track reuses a cached native FLAC but never the packet stream", async () => {
  const packets = record(alac, new Blob([bytes("SPLALAC1")]));
  assert.equal(await findCachedDownload(alac, { lookup: async (key) => (key === packets.key ? packets : null) }), null);
  const flacTrack = { ...alac, streaming_mode: null, streaming_url: null };
  const flac = record(flacTrack, new Blob([bytes("fLaC")]));
  const found = await findCachedDownload(alac, { lookup: async (key) => ({ [packets.key]: packets, [flac.key]: flac })[key] || null });
  assert.equal(found.ext, "flac");
});

test("an uncached ALAC track waits for the completed /audio derivative and is not duplicated in the cache", async () => {
  const statuses = [{ ...alac, status: "streaming", audio_url: "/api/v1/playbacks/applemusic--1--lossless/audio" },
    { ...alac, status: "completed", audio_url: "/api/v1/playbacks/applemusic--1--lossless/audio" }];
  const requested = [];
  const stored = [];
  const result = await resolveTrackDownload(alac, {
    lookup: async () => null,
    api: { getPlayback: async () => statuses.shift(), createPlayback: () => assert.fail("task exists") },
    fetchImpl: async (url) => { requested.push(url); return new Response(bytes("fLaC", 64), { headers: { "content-type": "audio/flac" } }); },
    store: (...args) => stored.push(args),
  });
  assert.equal(requested.length, 1);
  assert.match(requested[0], /\/api\/v1\/playbacks\/applemusic--1--lossless\/audio\?/);
  assert.doesNotMatch(requested[0], /\/audio\/stream/);
  assert.equal(result.filename, "歌 名 - 歌手.flac");
  assert.equal(result.fromCache, false);
  assert.equal(stored.length, 0);
});

test("a downloaded native file fills the playback cache; an expired task is recreated", async () => {
  const stored = [];
  const created = [];
  await resolveTrackDownload(native, {
    lookup: async () => null,
    api: {
      getPlayback: async () => { throw new Error("404"); },
      createPlayback: async (body) => { created.push(body); return { playback: { ...native, audio_url: "/api/v1/playbacks/applemusic--1--high/audio" } }; },
    },
    fetchImpl: async () => new Response(m4a, { headers: { "content-type": "audio/mp4" } }),
    store: (...args) => stored.push(args),
  });
  assert.equal(created[0].streamingMode, null);
  assert.equal(created[0].quality, "high");
  assert.equal(stored.length, 1);
  assert.equal(stored[0][0], cacheKey(native));
  assert.ok(stored[0][4].blob instanceof Blob);
});

test("a non-audio response is rejected", async () => {
  await assert.rejects(resolveTrackDownload(native, {
    lookup: async () => null,
    api: { getPlayback: async () => ({ ...native, audio_url: "/api/v1/playbacks/x/audio" }), createPlayback: () => assert.fail() },
    fetchImpl: async () => new Response("<html>", { headers: { "content-type": "text/html" } }),
    store: () => assert.fail("must not cache"),
  }), /不是可播放的音频/);
});

test("a lossless Apple Music download saves the M4A Telegram keeps, not a FLAC transcode", async () => {
  const source = "/api/v1/playbacks/applemusic--1--lossless/audio/source?rev=s";
  const requested = [];
  const stored = [];
  const plain = { ...alac, streaming_mode: "" };
  const result = await resolveTrackDownload(plain, {
    lookup: async () => null,
    api: { getPlayback: async () => ({ ...plain, status: "completed", audio_url: "/api/v1/playbacks/applemusic--1--lossless/audio", alac_source_url: source }),
      createPlayback: () => assert.fail("task exists") },
    fetchImpl: async (url) => { requested.push(url); return new Response(m4a, { headers: { "content-type": "audio/mp4" } }); },
    store: (...args) => stored.push(args),
  });
  assert.equal(requested.length, 1);
  assert.match(requested[0], /\/audio\/source\?rev=s/);
  assert.equal(result.filename, "歌 名 - 歌手.m4a");
  assert.equal(stored.length, 0, "ALAC is not the native player's cache entry");
});
