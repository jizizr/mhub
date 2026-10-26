import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import * as clock from '../src/services/playback.js';
import { createPlaybackLifecycle } from '../src/core/player/playback-lifecycle.js';

const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
let instance = 0;
const ranges = values => ({ length: values.length, start: i => values[i][0], end: i => values[i][1] });
class NativeAudio extends EventTarget {
  src = ''; paused = true; ended = false; error = null; readyState = 4; duration = 120; seeking = false; time = 0; assignments = []; plays = 0; emptyLoads = 0;
  buffered = ranges([[0, 120]]); seekable = ranges([[0, 120]]);
  get currentSrc() { return this.src; }
  get currentTime() { return this.time; }
  set currentTime(value) { this.time = value; this.assignments.push(value); this.seeking = true; this.emit('seeking'); }
  load() { if (!this.src) this.emptyLoads++; this.ended = false; this.error = null; }
  pause() { this.paused = true; this.emit('pause'); }
  async play() { this.plays++; this.paused = false; this.emit('play'); this.emit('playing'); }
  removeAttribute() { this.src = ''; }
  emit(name) { this.dispatchEvent(new Event(name)); }
  finish() { this.seeking = false; this.emit('seeked'); this.emit('timeupdate'); }
}
async function harness({ streaming = false, resolver, monitor, alac = false, lyricLoader, capability, lifecycle = false, nativeOutput = false, timers = null, options = {} } = {}) {
  clock.reset();
  const tracks = ['one', 'two'].map(id => ({ id, source_id: id, selected_id: id, title: id, platform: 'applemusic', quality: alac ? 'hires' : 'high', duration: 120000, status: streaming ? 'streaming' : 'completed', streaming_mode: alac ? 'wasm_alac' : null }));
  const queue = { queue: tracks, get queueLength() { return this.queue.length; }, getTrack(i) { return this.queue[i]; }, replaceTrack(i,t) { this.queue[i] = t; }, findTrackIndex(t) { return this.queue.findIndex(item => item.id === t.id); } };
  const status = { state: 'paused', position: 0, duration: 120000, quality: alac ? 'hires' : 'high', volume: .8, speed: 1, playIndex: 0, repeatMode: 'off', shuffleMode: 'off', currentSource: '', trackLoading: false, soundCheckEnabled: (lifecycle || nativeOutput) && !alac,
    get currentTrack() { return queue.getTrack(this.playIndex); }, get isPlaying() { return ['playing','buffering'].includes(this.state); }, persist() {}, setNotice(text) { this.notice = text; } };
  const media = { track: tracks[0], lyricLines: [], setTrack(t) { this.track = t; }, beginLyricLoad() { this.lyricLines = []; this.lyricLoading = true; }, setLyric(l) { this.lyricLines = l; this.lyricLoading = false; }, clearLyric() { this.lyricLines = []; this.lyricLoading = false; } };
  globalThis.localStorage = { removeItem() {} };
  globalThis.window = { location: { origin: 'http://localhost' }, setTimeout(fn) { timers?.push(fn); return 1; }, clearTimeout() {} };
  const session = Object.assign(new EventTarget(), { state: 'active' });
  const context = Object.assign(new EventTarget(), { state: 'running', resume: async () => {} });
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: lifecycle ? { audioSession: session } : {} });
  if (lifecycle) globalThis.document = new EventTarget();
  globalThis.__seekHarness = { status, media, queue, clock };
  let source = await readFile(new URL('../src/core/player/index.js', import.meta.url), 'utf8');
  source = source.replace('import { useMediaStore } from "../../stores/media";', 'const useMediaStore = () => globalThis.__seekHarness.media;')
    .replace('import { trackIdentity, useQueueStore } from "../../stores/queue";', 'const trackIdentity = track => track?.id; const useQueueStore = () => globalThis.__seekHarness.queue;')
    .replace('import { useStatusStore } from "../../stores/status";', 'const useStatusStore = () => globalThis.__seekHarness.status;')
    .replace('import * as playback from "../../services/playback";', 'const playback = globalThis.__seekHarness.clock;')
    .replace('import { trackFromSearch } from "../../utils/track";', 'const trackFromSearch = (track, quality) => ({ ...track, quality });');
  const player = await import(`data:text/javascript;base64,${Buffer.from(source + `\n// instance ${++instance}`).toString('base64')}`);
  const audio = new NativeAudio();
  const worklet = { context, canSeek: true, loaded: true, eof: false, seeks: [], playing: false, setVolume() {}, stop() {}, prepare() {}, async load() { return {}; },
    async seek(value) { this.seeks.push(value); this.onTime?.(value); this.onState?.('buffering'); if (this.delay) await this.delay.promise; },
    async play() { this.playing = true; }, pause() { this.playing = false; this.onState?.('paused'); } };
  player.initPlayer({ audio, resolveTrackSource: resolver || (async track => ({ source: `${lifecycle || nativeOutput ? 'blob' : 'test'}:${track.id}`, track })), waitForCompletedSource: monitor,
    loadLyricsForTrack: lyricLoader,
    ...(lifecycle ? { lifecycleFactory: createPlaybackLifecycle } : {}),
    ...(lifecycle || nativeOutput ? { outputFactory: { prepare() {}, createNative: () => ({ context, setVolume() {}, setLoudnessGain() {}, cancelFade() {} }) } } : {}),
    alacPlayerFactory: alac ? { canHandle: capability || (() => true), create: () => worklet } : null, ...options });
  await player.load(lifecycle || nativeOutput ? 'blob:one' : 'test:one', false, tracks[0]);
  if (streaming) { audio.buffered = ranges([[0,10]]); audio.seekable = ranges([[0,10]]); }
  return { player, status, media, queue, tracks, audio, worklet, session, context };
}

test('late ended and error events cannot skip the next track during a cold background handoff', async () => {
  const source = deferred(); let resolves = 0;
  const h = await harness({ resolver: () => { resolves++; return source.promise; } });
  h.queue.queue.push({ ...h.tracks[1], id: 'three', source_id: 'three', selected_id: 'three' });
  await h.player.play();
  h.audio.ended = true; h.audio.paused = true; h.audio.emit('ended');
  assert.equal(h.status.playIndex, 1);
  // The reused element has already reset its source, but old event tasks
  // can still arrive when a background page is scheduled again.
  h.audio.emit('ended'); h.audio.emit('error'); await tick();
  assert.equal(h.status.playIndex, 1);
  assert.equal(h.status.trackLoading, true);
  assert.equal(resolves, 1);
  source.resolve({ source: 'test:two', track: h.tracks[1] }); await tick(); await tick();
  h.audio.emit('ended'); h.audio.emit('error'); await tick();
  assert.equal(h.status.playIndex, 1);
  assert.equal(h.status.state, 'playing');
});

test('a cached next song starts inside the ended task without awaiting resolution', async () => {
  const timers = []; let resolves = 0; const cachedUrls = [];
  const h = await harness({ nativeOutput: true, timers, resolver: async track => { resolves++; return { source: `blob:${track.id}`, track }; }, options: {
    cacheActiveTrackSource: async () => true,
    prefetchTrackSource: async () => true,
    resolveCachedTrackSource: async track => { const source = `blob:cached-${track.id}-${cachedUrls.length}`; if (track.id === 'two') cachedUrls.push(source); return { source, track }; },
  } });
  await h.player.play();
  // Startup delay, then the cooperative yield before the next-song prefetch.
  for (let round = 0; round < 3; round++) { timers.splice(0).forEach(fn => fn()); await tick(); }
  assert.equal(cachedUrls.length, 1, 'the next song is armed once its local cache is ready');
  const before = h.audio.plays; resolves = 0;
  h.audio.ended = true; h.audio.paused = true; h.audio.emit('ended');
  // Microtasks only: a hidden phone may freeze the page at the next task.
  await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
  assert.equal(h.audio.src, cachedUrls[0]);
  assert.equal(h.audio.plays, before + 1);
  assert.equal(resolves, 0, 'no IndexedDB/network resolution between songs');
  assert.equal(h.status.playIndex, 1);
  await tick();
  assert.equal(h.status.state, 'playing');
  assert.equal(h.media.track.id, 'two');
});

async function prerollHarness(t) {
  const created = [];
  globalThis.document = { createElement: () => { const element = new NativeAudio(); element.paused = true; created.push(element); return element; }, body: { appendChild() {} } };
  t.after(() => { delete globalThis.document; });
  const timers = [];
  const h = await harness({ nativeOutput: true, timers, options: {
    cacheActiveTrackSource: async () => true, prefetchTrackSource: async () => true,
    resolveCachedTrackSource: async track => ({ source: `blob:cached-${track.id}`, track }),
  } });
  await h.player.play();
  for (let round = 0; round < 3; round++) { timers.splice(0).forEach(fn => fn()); await tick(); }
  return { ...h, created };
}

test('the next song preloads on a second player and starts muted before the current one ends', async (t) => {
  const h = await prerollHarness(t);
  const [next] = h.created;
  assert.equal(next.src, 'blob:cached-two');
  assert.equal(next.paused, true, 'preloading does not start a second player early');
  h.audio.time = 100; h.audio.emit('timeupdate');
  assert.equal(next.paused, true);
  h.audio.time = 119.2; h.audio.emit('timeupdate');
  assert.equal(next.muted, true);
  assert.equal(next.paused, false, 'a player stays in the media session through EOS');
  next.time = 0.8;
  // Chromium's final timeupdate at EOS already reports paused=true.
  h.audio.ended = true; h.audio.paused = true; h.audio.time = 120; h.audio.emit('timeupdate');
  assert.equal(next.paused, false, 'a natural EOS is not a pause');
  h.audio.emit('ended');
  await tick(); await tick();
  assert.equal(h.status.playIndex, 1);
  assert.equal(h.media.track.id, 'two');
  assert.deepEqual(next.assignments, [0], 'the muted preroll is rewound to the first frame');
  assert.equal(next.muted, true, 'stays silent until the rewind completes');
  next.finish();
  assert.equal(next.muted, false);
  assert.equal(h.audio.src, '', 'the ended player is released after the new one is active');
  assert.equal(next.paused, false);
  assert.equal(h.status.state, 'playing');
  next.time = 5; next.emit('timeupdate');
  assert.equal(h.status.position, 5000, 'the promoted element now drives playback');
});

test('pausing during the muted preroll stops the prepared player too', async (t) => {
  const h = await prerollHarness(t);
  const [next] = h.created;
  h.audio.time = 119.2; h.audio.emit('timeupdate');
  assert.equal(next.paused, false);
  h.player.pause();
  assert.equal(next.paused, true);
  assert.equal(next.src, 'blob:cached-two', 'the preloaded song stays ready');
});

test('an armed next song is discarded after a queue edit and the normal path is used', async () => {
  const timers = []; let resolves = 0;
  const h = await harness({ nativeOutput: true, timers, resolver: async track => { resolves++; return { source: `blob:${track.id}`, track }; }, options: {
    cacheActiveTrackSource: async () => true, prefetchTrackSource: async () => true,
    resolveCachedTrackSource: async track => ({ source: `blob:cached-${track.id}`, track }),
  } });
  await h.player.play();
  for (let round = 0; round < 3; round++) { timers.splice(0).forEach(fn => fn()); await tick(); }
  h.queue.queue.splice(1, 0, { ...h.tracks[1], id: 'inserted', source_id: 'inserted', selected_id: 'inserted' });
  resolves = 0;
  h.audio.ended = true; h.audio.paused = true; h.audio.emit('ended'); await tick(); await tick();
  assert.equal(resolves, 1);
  assert.equal(h.media.track.id, 'inserted');
  assert.equal(h.audio.src, 'blob:inserted');
});

test('a dropped stream reloads the same song once at its position before skipping', async () => {
  let resolves = 0;
  const h = await harness({ resolver: async track => { resolves++; return { source: `test:${track.id}`, track }; } });
  await h.player.play(); h.audio.time = 42;
  h.audio.error = { code: 2 }; h.audio.emit('error'); await tick(); await tick();
  assert.equal(h.status.playIndex, 0, 'a network drop is not a broken song');
  assert.equal(resolves, 1);
  assert.equal(h.audio.assignments.at(-1), 42);
  h.audio.finish(); await tick();
  assert.equal(h.status.state, 'playing');
  // After real playback a later drop gets its own reload.
  h.audio.error = { code: 2 }; h.audio.emit('error'); await tick(); await tick();
  assert.equal(resolves, 2);
  assert.equal(h.status.playIndex, 0);
  h.audio.finish(); await tick();
  assert.equal(h.status.state, 'playing');
  // A reload that drops again before producing audio is a real failure.
  h.audio.play = async () => { h.audio.plays++; h.audio.paused = false; };
  h.audio.error = { code: 2 }; h.audio.emit('error'); await tick(); await tick();
  h.audio.error = { code: 2 }; h.audio.emit('error');
  assert.equal(h.status.state, 'idle');
  assert.equal(resolves, 3);
});

test('native play starts before a suspended effects context finishes resuming', async () => {
  const h = await harness({ nativeOutput: true });
  h.status.soundCheckEnabled = true;
  // Attach the effects graph before simulating its OS suspension.
  await h.player.play(); h.player.pause();
  const resumed = deferred(); h.context.state = 'suspended'; h.context.resume = () => resumed.promise;
  const before = h.audio.plays;
  const pending = h.player.play(); await tick();
  assert.equal(h.audio.plays, before + 1, 'native media ownership must not wait for Web Audio');
  h.context.state = 'running'; resumed.resolve(); await pending;
  assert.equal(h.status.state, 'playing');
});

test('a delayed detach pause cannot cancel an automatic load, but explicit pause still wins', async () => {
  const resumed = deferred(); const h = await harness({ nativeOutput: true });
  h.status.soundCheckEnabled = true;
  await h.player.play(); h.player.pause();
  h.context.state = 'suspended'; h.context.resume = () => resumed.promise;
  const nativePlay = h.audio.play.bind(h.audio);
  const permission = deferred();
  h.audio.play = async () => { await permission.promise; return nativePlay(); };
  const loading = h.player.playAtIndex(1); await tick(); await tick();
  h.audio.emit('pause');
  assert.equal(h.status.state, 'buffering');
  h.player.pause();
  permission.resolve(); h.context.state = 'running'; resumed.resolve(); await loading;
  assert.equal(h.status.state, 'paused');
});

test('failed effects resume stops the native transport and keeps the song available for retry', async () => {
  const h = await harness({ nativeOutput: true });
  h.context.state = 'suspended';
  h.context.resume = async () => { throw new DOMException('Gesture required', 'NotAllowedError'); };
  assert.equal(await h.player.play(), false);
  assert.equal(h.audio.paused, true);
  assert.equal(h.status.state, 'paused');
  assert.equal(h.status.playIndex, 0);
  assert.equal(h.status.currentSource, 'blob:one');
});

test('a real current-source media error still enters the bounded failure path', async () => {
  const h = await harness(); await h.player.play();
  h.audio.error = { code: 3 }; h.audio.emit('error');
  assert.equal(h.status.state, 'idle');
  assert.equal(h.status.trackLoading, false);
});

test('ALAC EOF keeps system playback intent until its once-only queue handoff', async () => {
  const source = deferred(); const h = await harness({ alac: true, resolver: () => source.promise });
  await h.player.play(); h.worklet.onState('playing');
  const ended = h.worklet.onEnded;
  h.worklet.ended = true; h.worklet.onState('paused');
  assert.equal(h.status.state, 'playing', 'EOF is not a user pause');
  ended(); ended(); await tick();
  assert.equal(h.status.playIndex, 1);
  assert.equal(h.status.trackLoading, true);
  h.player.pause(); source.resolve({ source: 'test:two', track: h.tracks[1] }); await tick();
  assert.equal(h.status.state, 'paused');
});

test('automatic native handoff replaces the source without empty loads that release media ownership', async () => {
  const resolved = deferred(); const h = await harness({ resolver: () => resolved.promise });
  await h.player.play(); const before = h.audio.emptyLoads;
  h.audio.ended = true; h.audio.paused = true; h.audio.emit('ended'); await tick();
  assert.equal(h.audio.src, 'test:one', 'retain the stopped native player while the next source resolves');
  assert.equal(h.audio.emptyLoads, before);
  h.audio.emit('timeupdate');
  assert.equal(h.status.position, 0, 'retained source events do not change the next track');
  resolved.resolve({ source: 'test:two', track: h.tracks[1] }); await tick(); await tick();
  assert.equal(h.audio.emptyLoads, before, 'replace directly rather than clearing then recreating');
  assert.equal(h.audio.src, 'test:two'); assert.equal(h.status.state, 'playing');
});

test('failed source replacement cannot replay the retained previous song', async () => {
  let fail = true;
  const h = await harness({ resolver: async track => { if (fail) throw new Error('Offline'); return { source: 'test:two', track }; } });
  await h.player.play(); await h.player.playAtIndex(1);
  fail = false; await h.player.play();
  assert.equal(h.audio.src, 'test:two');
  assert.equal(h.status.playIndex, 1); assert.equal(h.status.state, 'playing');
});

test('retaining a native player does not retain a detached custom-renderer Blob', async (t) => {
  const h = await harness({ alac: true });
  const revoke = URL.revokeObjectURL, revoked = [];
  URL.revokeObjectURL = source => { revoked.push(source); };
  t.after(() => { URL.revokeObjectURL = revoke; });
  await h.player.load('blob:packet-one', false, h.tracks[0]);
  await h.player.load('blob:packet-two', false, h.tracks[1]);
  assert.deepEqual(revoked, ['blob:packet-one']);
});

test('native drag publishes target and keeps paused audio paused after seeked', async () => {
  const h = await harness(); h.player.seek(70000);
  assert.equal(h.status.position, 70000); assert.equal(clock.getCurrentTime(), 70000);
  h.audio.emit('timeupdate'); assert.equal(h.status.position, 70000);
  h.audio.finish(); await tick();
  assert.equal(h.audio.currentTime,70); assert.equal(h.audio.paused,true); assert.equal(h.status.state,'paused');
});

test('a native stream accepts a future target and resumes only when those bytes are seekable', async () => {
  const h = await harness({ streaming: true }); await h.player.play(); h.player.seek(80000);
  assert.equal(h.status.position,80000); assert.equal(h.status.state,'buffering'); assert.equal(h.audio.paused,true);
  assert.deepEqual(h.audio.assignments,[]); assert.equal(clock.isPlaying(),false);
  h.audio.time=2; h.audio.emit('timeupdate'); assert.equal(h.status.position,80000);
  h.audio.buffered=ranges([[0,90]]); h.audio.emit('progress');
  assert.deepEqual(h.audio.assignments,[80]); h.audio.finish(); await tick();
  assert.equal(h.status.state,'playing'); assert.equal(h.audio.paused,false); assert.equal(h.status.position,80000);
});

test('stale streaming metadata does not prohibit seeking within the actual seekable range', async () => {
  const h=await harness({streaming:true}); h.audio.seekable=ranges([[0,120]]); h.player.seek(100000);
  assert.deepEqual(h.audio.assignments,[100]); h.audio.finish(); assert.equal(h.status.position,100000);
});

test('repeated streaming seek uses only the latest target; pause wins when it becomes ready', async () => {
  const h=await harness({streaming:true}); await h.player.play(); h.player.seek(80000); h.player.seek(50000); h.player.pause();
  h.audio.buffered=ranges([[0,100]]); h.audio.emit('progress'); h.audio.finish(); await tick();
  assert.deepEqual(h.audio.assignments,[50]); assert.equal(h.status.position,50000); assert.equal(h.audio.paused,true); assert.equal(h.status.state,'paused');
});

test('completed native representation unblocks a future seek without starting another task', async () => {
  const completed=deferred(); const h=await harness({streaming:true,monitor:()=>completed.promise});
  await h.player.play(); h.player.seek(90000); completed.resolve('test:complete'); await tick();
  assert.equal(h.audio.src,'test:complete'); assert.deepEqual(h.audio.assignments,[90]);
  h.audio.finish(); await tick(); assert.equal(h.status.state,'playing');
});

test('switching songs aborts and ignores late completion from an old pending seek', async () => {
  const completed=deferred(); let signal;
  const h=await harness({streaming:true,monitor:(_track,options)=>{signal=options.signal;return completed.promise;}});
  h.player.seek(90000); await h.player.playAtIndex(1); completed.resolve('test:old-complete'); await tick();
  assert.equal(signal.aborted,true); assert.equal(h.audio.src,'test:two'); assert.equal(h.status.position,0);
});

test('seek during cold source resolution is retained and applied before playback', async () => {
  const source=deferred(); const h=await harness({resolver:()=>source.promise});
  const loading=h.player.playAtIndex(1); h.player.seek(45000);
  assert.equal(h.status.position,45000); source.resolve({source:'test:two',track:h.tracks[1]}); await loading;
  assert.deepEqual(h.audio.assignments,[45]); h.audio.finish(); await tick(); assert.equal(h.status.position,45000); assert.equal(h.status.state,'playing');
});

test('ALAC accepts forward seeks while its complete Blob is still unavailable', async () => {
  const h=await harness({alac:true,streaming:true}); await h.player.play(); await h.player.seek(95000);
  assert.deepEqual(h.worklet.seeks,[95000]); assert.equal(h.status.position,95000); assert.equal(h.status.state,'buffering'); assert.equal(clock.isPlaying(),false);
});

test('pause during an asynchronous ALAC seek never restarts playback', async () => {
  const h=await harness({alac:true}); await h.player.play(); h.worklet.delay=deferred();
  const seeking=h.player.seek(60000); h.player.pause(); h.worklet.delay.resolve(); await seeking;
  assert.equal(h.worklet.playing,false); assert.equal(h.status.state,'paused');
});

test('lyrics request starts before audio resolution and stays visible after resolution', async () => {
  const source=deferred(), lines=[{words:[{word:'synthetic'}]}], calls=[];
  const h=await harness({resolver:()=>source.promise,lyricLoader:async track=>{calls.push(track.id);return lines;}});
  const loading=h.player.playAtIndex(1); await tick();
  assert.deepEqual(calls,['two']); assert.equal(h.status.trackLoading,true); assert.deepEqual(h.media.lyricLines,lines);
  source.resolve({source:'test:two',track:h.tracks[1]}); await loading; await tick(); assert.deepEqual(h.media.lyricLines,lines);
});

test('failed cold resolution clears a queued seek so Play can retry the track', async () => {
  let release, attempts=0;
  const failed=new Promise((_resolve,reject)=>{release=reject;});
  const h=await harness({resolver:async track=>{attempts++; if(attempts===1)return failed; return {source:`test:${track.id}`,track};}});
  const loading=h.player.playAtIndex(1); h.player.seek(45000); release(new Error('temporary network failure')); await loading;
  assert.equal(h.status.trackLoading,false); assert.equal(h.status.currentSource,null);
  await h.player.play(); assert.equal(attempts,2); assert.equal(h.audio.src,'test:two'); assert.equal(h.status.state,'playing');
});


test('late capability checks cannot reattach an older native audio source', async () => {
  const delayed=deferred(); let calls=0;
  const h=await harness({alac:true,capability:()=>{calls++;return calls===2?delayed.promise:false;}});
  const oldLoad=h.player.load('test:cancelled',false,h.tracks[0]); await tick();
  await h.player.load('test:newest',false,h.tracks[0]); delayed.resolve(false); await oldLoad;
  assert.equal(h.audio.src,'test:newest'); assert.equal(h.status.currentSource,'test:newest');
});

test('ALAC queued time/playing messages cannot override an OS interruption with a running context', async () => {
  const h=await harness({alac:true,lifecycle:true}); await h.player.play();
  h.worklet.onState('playing'); h.worklet.onTime(10000);
  h.session.state='interrupted'; h.session.dispatchEvent(new Event('statechange'));
  assert.equal(h.context.state,'running'); assert.equal(h.status.state,'buffering');
  h.worklet.onTime(11000); h.worklet.onState('playing');
  assert.equal(h.status.position,10000); assert.equal(clock.getCurrentTime(),10000);
  assert.equal(h.status.state,'buffering'); assert.equal(clock.isPlaying(),false);
  h.session.state='active'; h.session.dispatchEvent(new Event('statechange'));
  assert.equal(h.status.state,'buffering', 'session release still waits for real transport advancement');
  h.worklet.onTime(11200);
  assert.equal(h.status.position,11200); assert.equal(h.status.state,'playing'); assert.equal(clock.isPlaying(),true);
  h.player.destroyPlayer();
});

test('native late playing/timeupdate and resolved play promise stay buffered until the audio session recovers', async () => {
  const h=await harness({lifecycle:true}); await h.player.play(); h.audio.time=10; h.audio.emit('timeupdate');
  h.session.state='interrupted'; h.session.dispatchEvent(new Event('statechange'));
  h.audio.time=11; h.audio.emit('playing'); h.audio.emit('timeupdate');
  await h.player.play();
  assert.equal(h.context.state,'running'); assert.equal(h.status.state,'buffering');
  assert.equal(h.status.position,10000); assert.equal(clock.getCurrentTime(),10000); assert.equal(clock.isPlaying(),false);
  h.session.state='active'; h.session.dispatchEvent(new Event('statechange'));
  assert.equal(h.status.state,'buffering');
  h.audio.time=11.2; h.audio.emit('timeupdate');
  assert.equal(h.status.state,'playing'); assert.equal(h.status.position,11200); assert.equal(clock.isPlaying(),true);
  h.player.destroyPlayer();
});

test('pausing during OS interruption prevents session recovery from restarting either transport', async () => {
  for(const alac of [false,true]) {
    const h=await harness({alac,lifecycle:true}); await h.player.play();
    if(alac)h.worklet.onState('playing');
    h.session.state='interrupted'; h.session.dispatchEvent(new Event('statechange'));
    h.player.pause(); h.session.state='active'; h.session.dispatchEvent(new Event('statechange')); await tick();
    assert.equal(h.status.state,'paused'); assert.equal(clock.isPlaying(),false);
    assert.equal(alac?h.worklet.playing:!h.audio.paused,false);
    h.player.destroyPlayer();
  }
});

test('a backwards seek during interruption uses its new position as the recovery baseline', async () => {
  const h=await harness({alac:true,lifecycle:true}); await h.player.play(); h.worklet.onState('playing'); h.worklet.onTime(30000);
  h.session.state='interrupted'; h.session.dispatchEvent(new Event('statechange'));
  await h.player.seek(5000);
  assert.equal(h.status.position,5000); assert.equal(h.status.state,'buffering');
  h.session.state='active'; h.session.dispatchEvent(new Event('statechange')); h.worklet.onTime(5200);
  assert.equal(h.status.state,'playing'); assert.equal(h.status.position,5200); assert.equal(clock.isPlaying(),true);
  h.player.destroyPlayer();
});
