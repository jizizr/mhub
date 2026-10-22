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
  src = ''; paused = true; readyState = 4; duration = 120; seeking = false; time = 0; assignments = []; plays = 0;
  buffered = ranges([[0, 120]]); seekable = ranges([[0, 120]]);
  get currentSrc() { return this.src; }
  get currentTime() { return this.time; }
  set currentTime(value) { this.time = value; this.assignments.push(value); this.seeking = true; this.emit('seeking'); }
  load() {}
  pause() { this.paused = true; this.emit('pause'); }
  async play() { this.plays++; this.paused = false; this.emit('play'); this.emit('playing'); }
  removeAttribute() { this.src = ''; }
  emit(name) { this.dispatchEvent(new Event(name)); }
  finish() { this.seeking = false; this.emit('seeked'); this.emit('timeupdate'); }
}
async function harness({ streaming = false, resolver, monitor, alac = false, lyricLoader, capability, lifecycle = false } = {}) {
  clock.reset();
  const tracks = ['one', 'two'].map(id => ({ id, source_id: id, selected_id: id, title: id, platform: 'applemusic', quality: alac ? 'hires' : 'high', duration: 120000, status: streaming ? 'streaming' : 'completed', streaming_mode: alac ? 'wasm_alac' : null }));
  const queue = { queue: tracks, get queueLength() { return this.queue.length; }, getTrack(i) { return this.queue[i]; }, replaceTrack(i,t) { this.queue[i] = t; }, findTrackIndex(t) { return this.queue.findIndex(item => item.id === t.id); } };
  const status = { state: 'paused', position: 0, duration: 120000, quality: alac ? 'hires' : 'high', volume: .8, speed: 1, playIndex: 0, repeatMode: 'off', shuffleMode: 'off', currentSource: '', trackLoading: false, soundCheckEnabled: lifecycle && !alac,
    get currentTrack() { return queue.getTrack(this.playIndex); }, get isPlaying() { return ['playing','buffering'].includes(this.state); }, persist() {}, setNotice(text) { this.notice = text; } };
  const media = { track: tracks[0], lyricLines: [], setTrack(t) { this.track = t; }, beginLyricLoad() { this.lyricLines = []; this.lyricLoading = true; }, setLyric(l) { this.lyricLines = l; this.lyricLoading = false; }, clearLyric() { this.lyricLines = []; this.lyricLoading = false; } };
  globalThis.localStorage = { removeItem() {} };
  globalThis.window = { location: { origin: 'http://localhost' }, setTimeout() { return 1; }, clearTimeout() {} };
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
  player.initPlayer({ audio, resolveTrackSource: resolver || (async track => ({ source: `${lifecycle ? 'blob' : 'test'}:${track.id}`, track })), waitForCompletedSource: monitor,
    loadLyricsForTrack: lyricLoader,
    ...(lifecycle ? { lifecycleFactory: createPlaybackLifecycle, outputFactory: { prepare() {}, createNative: () => ({ context, setVolume() {}, setLoudnessGain() {}, cancelFade() {} }) } } : {}),
    alacPlayerFactory: alac ? { canHandle: capability || (() => true), create: () => worklet } : null });
  await player.load(lifecycle ? 'blob:one' : 'test:one', false, tracks[0]);
  if (streaming) { audio.buffered = ranges([[0,10]]); audio.seekable = ranges([[0,10]]); }
  return { player, status, media, queue, tracks, audio, worklet, session, context };
}

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
