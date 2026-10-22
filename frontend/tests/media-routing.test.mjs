import test from "node:test";
import assert from "node:assert/strict";
import { playbackAudioUrl, mediaOrigin, withTaskVersion, trustedMediaOrigins } from "../src/apis/media-routing.js";
import { cacheKey } from "../src/services/localCache.js";

const task = {
  id: "applemusic--123--hires", platform: "applemusic", track_id: "123", quality: "hires",
  status: "completed", streaming_mode: "wasm_alac",
  audio_url: "/api/v1/playbacks/applemusic--123--hires/audio?rev=native-a&download=1",
  streaming_url: "/api/v1/playbacks/applemusic--123--hires/audio/stream?rev=packet-a",
};
const legacyConfig = { legacyMediaBaseUrl: "https://media.example.test" };
const config = { mediaBaseUrl: "https://media.example.test/" };

test("completed packet/native audio use configured media origin and preserve revision/query", () => {
  assert.equal(playbackAudioUrl(task, config), "https://media.example.test/api/v1/playbacks/applemusic--123--hires/audio/stream?rev=packet-a&v=applemusic--123--hires");
  assert.equal(playbackAudioUrl({ ...task, streaming_mode: null }, config), "https://media.example.test/api/v1/playbacks/applemusic--123--hires/audio?rev=native-a&download=1&v=applemusic--123--hires");
});

test("noncompleted requests stay on API origin even with a previous absolute media URL", () => {
  for (const status of ["queued", "processing", "streaming", "failed"]) {
    const url = playbackAudioUrl({ ...task, status, streaming_url: `https://media.example.test${task.streaming_url}` }, config);
    assert.equal(url, "/api/v1/playbacks/applemusic--123--hires/audio/stream?rev=packet-a&v=applemusic--123--hires");
  }
});

test("unconfigured production/development stays same-origin; custom server absolute URLs remain supported", () => {
  assert.deepEqual(trustedMediaOrigins(), []);
  assert.ok(playbackAudioUrl(task).startsWith("/api/"));
  assert.ok(playbackAudioUrl({ ...task, streaming_url: `https://media.example${task.streaming_url}` }).startsWith("https://media.example/"));
});

test("persisted production packet and native media URLs return to the API origin", () => {
  for (const streaming_mode of [null, "wasm_alac"]) {
    const old = { ...task, streaming_mode,
      audio_url: `https://media.example.test${task.audio_url}`,
      streaming_url: `https://media.example.test${task.streaming_url}` };
    assert.equal(playbackAudioUrl(old, legacyConfig), playbackAudioUrl({ ...task, streaming_mode }));
    assert.equal(cacheKey(old), cacheKey({ ...task, streaming_mode }));
  }
});

test("legacy media normalization preserves the revision and replaces only the task v parameter", () => {
  const url = playbackAudioUrl({ ...task,
    streaming_url: `https://media.example.test${task.streaming_url}&v=old&extra=a%2Bb&extra=c%26d` }, legacyConfig);
  assert.equal(url, `${task.streaming_url}&v=${task.id}&extra=a%2Bb&extra=c%26d`);
});

test("legacy origin normalization never rewrites another task, representation path or lookalike origin", () => {
  const endpoints = [
    `https://media.example.test/api/v1/playbacks/other/audio/stream?rev=other`,
    `https://media.example.test/api/v1/playbacks/${task.id}/lyrics`,
    `https://media.example.test/api/v1/playbacks/${task.id}/audio/stream/extra`,
    `https://media.example.test.example.test${task.streaming_url}`,
    `https://media.example.test:8443${task.streaming_url}`,
  ];
  for (const endpoint of endpoints) {
    assert.equal(playbackAudioUrl({ ...task, streaming_url: endpoint }, legacyConfig), withTaskVersion(endpoint, task.id));
  }
  assert.equal(playbackAudioUrl({ ...task, streaming_url: `https://name:secret@media.example.test${task.streaming_url}` }), "");
});

test("only exact task audio endpoints can be rerouted; version query is not duplicated", () => {
  for (const path of ["cover", "lyrics", "loudness", "status"]) {
    const endpoint = `/api/v1/playbacks/${task.id}/${path}?q=a%2Bb`;
    assert.ok(playbackAudioUrl({ ...task, streaming_mode: null, audio_url: endpoint }, config).startsWith(endpoint));
  }
  const other = "/api/v1/playbacks/other/audio?rev=other";
  assert.equal(playbackAudioUrl({ ...task, streaming_url: other }, config), `${other}&v=${task.id}`);
  assert.equal(withTaskVersion("/cover?rev=a&v=old", task.id), `/cover?rev=a&v=${task.id}`);
});

test("invalid or credentialed media origins are never trusted", () => {
  for (const value of ["//media.example", "https://name:secret@media.example", "https://media.example/path", "https://media.example?token=x", "javascript:alert(1)"]) assert.equal(mediaOrigin(value), "");
  assert.equal(withTaskVersion("https://name:secret@media.example/audio", task.id), "");
});

test("IndexedDB keeps song/quality/representation identity independent of CDN or revision", () => {
  assert.equal(cacheKey(task), cacheKey({ ...task, streaming_url: `https://media.example.test${task.streaming_url}`, media_revision: "next" }));
  assert.notEqual(cacheKey(task), cacheKey({ ...task, streaming_mode: null }));
  assert.notEqual(cacheKey(task), cacheKey({ ...task, quality: "lossless" }));
});
