import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { audioUrl } from "../src/apis/music.js";
import { cacheKey, cacheEntryMatches, hasCachedAudio } from "../src/services/localCache.js";
import { trackFromPlayback } from "../src/utils/track.js";

const app = await readFile(new URL("../src/App.vue", import.meta.url), "utf8");
const source = app.slice(app.indexOf("async function cacheActiveTrackSource("), app.indexOf("\n/**\n * Prepare the next queue item"));
const task = { id: "applemusic--123--high", platform: "applemusic", track_id: "123", quality: "high", status: "streaming", audio_url: "/api/v1/playbacks/applemusic--123--high/audio?rev=old" };
function harness({ initial = task, latest = { ...task, status: "completed", audio_url: "/api/v1/playbacks/applemusic--123--high/audio?rev=new" }, stored = null, reject = false } = {}) {
  let reads = 0; const writes = [];
  const cached = stored || { task: initial, track: initial };
  const deps = {
    cacheKey, cacheEntryMatches, hasCachedAudio, audioUrl, trackFromPlayback,
    getCachedTrack: async () => cached,
    getPlayback: async () => { reads++; if (reject) throw new Error("offline"); return latest; },
    cacheAudioBlob: async (key, url, task, track, options) => {
      writes.push({ key, url, task, track, options });
      return { key, task, track, blob: new Blob(["complete"]) };
    }, refreshLocalCache() {},
  };
  return { run: new Function(...Object.keys(deps), `return (${source});`)(...Object.values(deps)), writes, reads: () => reads };
}

test("native cache refreshes stale metadata and preserves the latest same-origin media URL", async () => {
  const h = harness();
  assert.equal(await h.run(task), true);
  assert.equal(h.reads(), 1); assert.equal(h.writes.length, 1);
  assert.equal(h.writes[0].url, "/api/v1/playbacks/applemusic--123--high/audio?rev=new&v=applemusic--123--high");
  assert.equal(h.writes[0].task.status, "completed");
  assert.equal(h.writes[0].track.status, "completed");
});

test("validated ALAC EOF blob is persisted without status or second media request", async () => {
  const packet = { ...task, streaming_mode: "wasm_alac", streaming_url: `${task.audio_url.split('?')[0]}/stream?rev=packet` };
  const h = harness({ initial: packet }); const blob = new Blob(["complete packet"]);
  assert.equal(await h.run(packet, { blob }), true);
  assert.equal(h.reads(), 0); assert.equal(h.writes[0].options.blob, blob);
});

test("native cache preserves its representation when latest task also offers WASM", async () => {
  const h = harness({ latest: { ...task, status: "completed", streaming_mode: "wasm_alac", streaming_url: "/packet" } });
  assert.equal(await h.run(task), true);
  assert.equal(h.writes[0].task.streaming_mode, null);
  assert.equal(h.writes[0].key, cacheKey(task));
});

test("failed or changed quality tasks cannot fill the current cache", async () => {
  for (const latest of [{ ...task, status: "failed" }, { ...task, quality: "hires" }]) {
    const h = harness({ latest }); assert.equal(await h.run(task), false); assert.equal(h.writes.length, 0);
  }
});

test("status failure is retryable and cannot fetch from stale metadata", async () => {
  const h = harness({ reject: true }); await assert.rejects(h.run(task), /offline/); assert.equal(h.writes.length, 0);
});
