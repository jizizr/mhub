import test from "node:test";
import assert from "node:assert/strict";
import { createMediaSessionController, systemArtwork } from "../src/core/player/media-session.js";

function fixture(options = {}) {
  const values = { metadata: [], playbackState: [], positions: [], actions: [] };
  const handlers = new Map();
  const current = {
    track: { id: "test-song", title: "Synthetic song", artist: "Test artist", album: "Test album", cover: "/art/test.webp" },
    state: "playing", position: 5000, duration: 120000, speed: 1, wantsPlay: true,
  };
  let timestamp = 0;
  let actualState = "none";
  class Metadata { constructor(value) { Object.assign(this, value); } }
  const session = {
    set metadata(value) { values.metadata.push(value); },
    get playbackState() { return actualState; },
    set playbackState(value) { actualState = value; values.playbackState.push(value); },
    setPositionState(value) { values.positions.push(value); },
    setActionHandler(action, handler) { values.actions.push([action, handler]); handlers.set(action, handler); },
  };
  const calls = [];
  const actions = {
    play: () => calls.push(["play"]),
    pause: () => calls.push(["pause"]),
    previous: () => calls.push(["previous"]),
    next: () => calls.push(["next"]),
    seek: (position) => { calls.push(["seek", position]); current.position = position; },
    stop: () => calls.push(["stop"]),
  };
  const controller = createMediaSessionController({
    readState: () => current, actions, navigatorObject: { mediaSession: session }, Metadata,
    baseUrl: "https://example.test/app/", now: () => timestamp, ...options,
  });
  return { controller, values, handlers, current, calls, session, advance: (ms) => { timestamp += ms; } };
}

test("system metadata uses actual artwork without invented image type or dimensions and dedupes time updates", () => {
  const h = fixture();
  h.controller.update();
  assert.deepEqual(h.values.metadata.at(-1).artwork, [{ src: "https://example.test/art/test.webp" }]);
  assert.equal(h.values.metadata.at(-1).title, "Synthetic song");
  assert.equal(h.values.playbackState.at(-1), "playing");
  for (let count = 0; count < 50; count += 1) {
    h.advance(100); h.current.position += 100; h.controller.update();
  }
  assert.equal(h.values.metadata.length, 1);
  assert.equal(h.values.actions.length, 8);
  assert.equal(h.values.positions.filter(Boolean).length, 6);
  h.current.track.cover = "";
  h.current.track.cover_url = "https://example.test/cover.jpg";
  h.controller.update();
  assert.deepEqual(h.values.metadata.at(-1).artwork, [{ src: "https://example.test/cover.jpg" }]);
});

test("buffering retains playing controls until the user pauses; state changes immediately refresh progress", () => {
  const h = fixture();
  h.controller.update();
  h.current.state = "buffering"; h.controller.update();
  assert.equal(h.values.playbackState.at(-1), "playing");
  h.current.wantsPlay = false; h.controller.update();
  assert.equal(h.values.playbackState.at(-1), "paused");
  assert.equal(h.values.positions.at(-1).position, 5);
  h.current.state = "idle"; h.current.wantsPlay = true; h.controller.update();
  assert.equal(h.values.playbackState.at(-1), "playing", "a cold source can still be paused from the OS");
});

test("a new native transport republishes controls and metadata even when DOM session state still says playing", () => {
  const h = fixture(); h.controller.update();
  const previous = { metadata: h.values.metadata.length, actions: h.values.actions.length, state: h.values.playbackState.length };
  // Android may release the old native player without changing the JS
  // MediaSession getters. The real playing event must republish its session.
  h.controller.activate();
  assert.equal(h.values.metadata.length, previous.metadata + 1);
  assert.equal(h.values.actions.length, previous.actions + 8);
  assert.equal(h.values.playbackState.length, previous.state + 1);
  assert.equal(h.values.playbackState.at(-1), 'playing');
  assert.ok(h.values.actions.slice(previous.actions).every(([, handler]) => typeof handler === 'function'));
  h.current.state = 'paused'; h.current.wantsPlay = false;
  h.controller.activate();
  assert.equal(h.values.playbackState.at(-1), 'paused', 'activation never overrides user intent');
});

test("position clamps seeks, validates rates and duration, and clears stale progress for unknown duration", () => {
  const h = fixture();
  h.controller.update();
  h.current.position = 180000; h.current.speed = 0; h.controller.syncPosition();
  assert.deepEqual(h.values.positions.at(-1), { duration: 120, position: 120, playbackRate: 1 });
  h.current.position = -3000; h.current.speed = NaN; h.controller.syncPosition();
  assert.equal(h.values.positions.at(-1).position, 0);
  h.current.speed = 1.5; h.controller.syncPosition();
  assert.equal(h.values.positions.at(-1).playbackRate, 1.5);
  h.current.duration = Infinity; h.controller.syncPosition();
  assert.equal(h.values.positions.at(-1), undefined);
  const count = h.values.positions.length;
  h.current.duration = 0; h.controller.syncPosition();
  assert.equal(h.values.positions.length, count, "invalid duration clears once instead of spamming the platform");
  h.current.duration = 10000; h.controller.syncPosition();
  assert.equal(h.values.positions.at(-1).duration, 10);
});

test("normal progress is throttled but seeks, speed, duration and forced updates are immediate", () => {
  const h = fixture();
  h.controller.update();
  const count = h.values.positions.length;
  h.advance(100); h.current.position += 100; h.controller.syncPosition();
  assert.equal(h.values.positions.length, count);
  h.current.position = 70000; h.controller.syncPosition();
  assert.equal(h.values.positions.at(-1).position, 70);
  h.current.speed = 2; h.controller.syncPosition();
  assert.equal(h.values.positions.at(-1).playbackRate, 2);
  h.current.duration = 125000; h.controller.syncPosition();
  assert.equal(h.values.positions.at(-1).duration, 125);
  const beforeForced = h.values.positions.length;
  h.controller.syncPosition({ force: true });
  assert.equal(h.values.positions.length, beforeForced + 1);
});

test("media actions use milliseconds, valid offsets, bounds and complete transport controls", () => {
  const h = fixture(); h.controller.update();
  for (const name of ["play", "pause", "previoustrack", "nexttrack"]) h.handlers.get(name)();
  assert.deepEqual(h.calls, [["play"], ["pause"], ["previous"], ["next"]]);
  h.handlers.get("seekbackward")({ seekOffset: 10 });
  assert.deepEqual(h.calls.at(-1), ["seek", 0]);
  h.handlers.get("seekforward")({ seekOffset: NaN });
  assert.deepEqual(h.calls.at(-1), ["seek", 10000]);
  h.handlers.get("seekto")({ seekTime: 500, fastSeek: true });
  assert.deepEqual(h.calls.at(-1), ["seek", 120000]);
  const count = h.calls.length;
  h.handlers.get("seekto")({}); h.handlers.get("seekto")({ seekTime: Infinity });
  assert.equal(h.calls.length, count, "invalid seek events do not restart or break the song");
  h.handlers.get("seekto")({ seekTime: 0 });
  assert.deepEqual(h.calls.at(-1), ["seek", 0]);
  h.handlers.get("stop")();
  assert.deepEqual(h.calls.at(-1), ["stop"]);
  assert.equal(h.values.metadata.at(-1), null);
  assert.equal(h.values.playbackState.at(-1), "none");
  assert.ok([...h.handlers.values()].every((handler) => handler === null));
});

test("clear drops metadata/actions/position and the next song restores controls; disposal ignores stale handlers", () => {
  const h = fixture(); h.controller.update();
  const oldPlay = h.handlers.get("play");
  h.controller.update(null);
  assert.equal(h.values.metadata.at(-1), null);
  assert.equal(h.values.positions.at(-1), undefined);
  oldPlay(); assert.equal(h.calls.length, 0);
  h.current.track = { id: "second", title: "Second song", artist: "Test artist" };
  h.current.position = 0; h.controller.update();
  assert.equal(h.values.metadata.at(-1).title, "Second song");
  assert.equal(h.values.positions.at(-1).position, 0);
  h.handlers.get("play")(); assert.equal(h.calls.length, 1);
  const latestPlay = h.handlers.get("play");
  h.controller.dispose(); latestPlay(); h.controller.update(); h.controller.register();
  assert.equal(h.calls.length, 1);
  assert.ok([...h.handlers.values()].every((handler) => handler === null));
});

test("missing MediaMetadata does not disable system playback controls or state", () => {
  const h = fixture({ Metadata: null }); h.controller.update();
  assert.equal(h.values.metadata.length, 0);
  assert.equal(h.values.playbackState.at(-1), "playing");
  h.handlers.get("pause")(); assert.deepEqual(h.calls.at(-1), ["pause"]);
});

test("a browser rejecting one action or position update does not break remaining controls", () => {
  const h = fixture();
  const original = h.session.setActionHandler;
  h.session.setActionHandler = function (action, handler) {
    if (action === "seekto") throw new TypeError("Unsupported action");
    original.call(this, action, handler);
  };
  h.session.setPositionState = () => { throw new TypeError("Unsupported position state"); };
  assert.doesNotThrow(() => h.controller.update());
  assert.equal(h.values.playbackState.at(-1), "playing");
  h.handlers.get("nexttrack")(); assert.deepEqual(h.calls.at(-1), ["next"]);
  assert.doesNotThrow(() => h.controller.clear());
});

test("artwork fallback and throwing metadata/state setters remain isolated from playback", () => {
  class RejectArtwork {
    constructor(value) {
      if (value.artwork.length) throw new Error("Artwork format unavailable");
      Object.assign(this, value);
    }
  }
  const h = fixture({ Metadata: RejectArtwork }); h.controller.update();
  assert.equal(h.values.metadata.at(-1).title, "Synthetic song");
  assert.deepEqual(h.values.metadata.at(-1).artwork, []);
  Object.defineProperty(h.session, "metadata", { set() { throw new Error("restricted"); } });
  Object.defineProperty(h.session, "playbackState", { set() { throw new Error("restricted"); } });
  h.current.track.title = "New title"; h.current.state = "paused"; h.current.wantsPlay = false;
  assert.doesNotThrow(() => h.controller.update());
  h.handlers.get("play")(); assert.deepEqual(h.calls.at(-1), ["play"]);
  assert.doesNotThrow(() => h.controller.dispose());
});

test("unsupported/restricted Media Session and rejected action promises are harmless", async () => {
  for (const navigatorObject of [undefined, {}, { get mediaSession() { throw new Error("restricted"); } }]) {
    const controller = createMediaSessionController({ navigatorObject });
    assert.doesNotThrow(() => { controller.register(); controller.update(); controller.syncPosition(); controller.clear(); controller.dispose(); });
  }
  const h = fixture({ actions: { play: () => Promise.reject(new Error("not allowed")), pause: () => { throw new Error("interrupted"); } } });
  h.controller.update();
  assert.doesNotThrow(() => { h.handlers.get("play")(); h.handlers.get("pause")(); });
  await new Promise((resolve) => setImmediate(resolve));
});

test("invalid artwork URL schemes are omitted and valid backend cover is used", () => {
  const h = fixture();
  h.current.track.cover = "javascript:alert(1)";
  h.current.track.cover_url = "/cover/second.jpeg";
  h.controller.update();
  assert.deepEqual(h.values.metadata.at(-1).artwork, [{ src: "https://example.test/cover/second.jpeg" }]);
});

test("real progress reconciles a browser state reset while WASM audio remains playing", () => {
  const h = fixture(); h.controller.update();
  h.session.playbackState = "paused";
  h.advance(100); h.current.position += 100; h.controller.syncPosition();
  assert.equal(h.session.playbackState, "playing");
  assert.equal(h.values.positions.at(-1).position, 5.1, "the correction also reanchors system progress");
  const count = h.values.playbackState.length;
  for (let i = 0; i < 30; i++) { h.advance(100); h.current.position += 100; h.controller.syncPosition(); }
  assert.equal(h.values.playbackState.length, count, "stable declared state causes no extra writes");
  h.current.state = "paused"; h.current.wantsPlay = false; h.controller.update();
  h.session.playbackState = "playing"; h.controller.syncPosition();
  assert.equal(h.session.playbackState, "paused", "explicit pause remains authoritative");
});

test("state reconciliation does not depend on position-state support", () => {
  const h = fixture(); delete h.session.setPositionState;
  h.controller.update(); h.session.playbackState = "none"; h.controller.syncPosition();
  assert.equal(h.session.playbackState, "playing");
});

test("queued progress/pause after Stop cannot resurrect cleared media ownership", () => {
  const h = fixture(); h.controller.update(); h.current.state = "stopped"; h.current.wantsPlay = false; h.controller.clear();
  const count = h.values.positions.length;
  h.controller.syncPosition(); h.controller.update();
  assert.equal(h.session.playbackState, "none");
  assert.equal(h.values.metadata.at(-1), null);
  assert.equal(h.values.positions.length, count);
  assert.ok([...h.handlers.values()].every(handler => handler === null));
});

test("platform covers offer the system a real 512px rendition before the original", () => {
  const h = fixture();
  const original = "https://is1-ssl.mzstatic.com/image/thumb/Music221/v4/a0/dd/fd/a0/26UM1IM21436.rgb.jpg/1200x1200bb.jpg";
  h.current.track.cover = original;
  h.controller.update();
  assert.deepEqual(h.values.metadata.at(-1).artwork, [
    { src: "https://is1-ssl.mzstatic.com/image/thumb/Music221/v4/a0/dd/fd/a0/26UM1IM21436.rgb.jpg/512x512bb.jpg", sizes: "512x512" },
    { src: original },
  ]);
});

test("each platform CDN is asked for a rendition it serves; unknown hosts are left alone", () => {
  assert.deepEqual(systemArtwork("https://y.gtimg.cn/music/photo_new/T002M000003y8dsH2wBHlo.jpg"),
    { src: "https://y.gtimg.cn/music/photo_new/T002R500x500M000003y8dsH2wBHlo.jpg", sizes: "500x500" });
  assert.deepEqual(systemArtwork("https://y.gtimg.cn/music/photo_new/T002R300x300M000003y8dsH2wBHlo.jpg"),
    { src: "https://y.gtimg.cn/music/photo_new/T002R500x500M000003y8dsH2wBHlo.jpg", sizes: "500x500" });
  assert.deepEqual(systemArtwork("http://p1.music.126.net/ZGffiDQZrGj5s_hnR1CNbg==/109951165566379710.jpg?param=1080y1080"),
    { src: "https://p1.music.126.net/ZGffiDQZrGj5s_hnR1CNbg==/109951165566379710.jpg?param=512y512", sizes: "512x512" });
  assert.deepEqual(systemArtwork("https://p3-luna.douyinpic.com/img/tos-cn-v-2774c002/abc~c5_375x375-resize:960:960.png"),
    { src: "https://p3-luna.douyinpic.com/img/tos-cn-v-2774c002/abc~c5_375x375-resize:512:512.png", sizes: "512x512" });
  assert.equal(systemArtwork("https://is1-ssl.mzstatic.com/image/thumb/Music/source/cover.jpg"), null);
  assert.equal(systemArtwork("https://imge.kugou.com/stdmusic/400/20200909/cover.jpg"), null);
  assert.equal(systemArtwork("https://example.test/art/test.webp"), null);
  assert.equal(systemArtwork("blob:https://example.test/1234"), null);
});

test("buffering holds the system progress at the target while keeping the playing controls", () => {
  const h = fixture();
  h.controller.update();
  assert.equal(h.values.positions.at(-1).playbackRate, 1);
  // A system seek lands beyond the download: no sound until the data arrives.
  h.current.state = "buffering";
  h.current.position = 90000;
  h.controller.update();
  const held = h.values.positions.at(-1);
  assert.equal(held.position, 90);
  assert.ok(held.playbackRate > 0 && held.playbackRate < 1e-3, "near-zero rate freezes OS extrapolation");
  assert.equal(h.values.playbackState.at(-1), "playing", "the pause control and background eligibility stay");
  h.advance(5000);
  h.controller.syncPosition();
  assert.deepEqual(h.values.positions.at(-1), held, "later updates keep holding the target");
  // Audio renders from the target: the real rate is published at once.
  h.current.state = "playing";
  h.controller.update();
  assert.equal(h.values.positions.at(-1).playbackRate, 1);
  assert.equal(h.values.positions.at(-1).position, 90);
});

test("a browser rejecting the near-zero rate still receives the held position", () => {
  const h = fixture();
  const accepted = h.session.setPositionState;
  h.session.setPositionState = (value) => {
    if (value.playbackRate < 1e-3) throw new TypeError("rate");
    accepted(value);
  };
  h.current.state = "buffering";
  h.current.position = 30000;
  h.controller.update();
  assert.deepEqual(h.values.positions.at(-1), { duration: 120, playbackRate: 1, position: 30 });
});
