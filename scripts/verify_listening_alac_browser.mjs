/**
 * Real stereo 96 kHz / 24-bit ALAC crossfade regression. Uses the production
 * player, WASM decoder, packet parser and AudioWorklet; only media/API/cache
 * inputs are synthetic. No account, credentials or production requests.
 *
 * Requires ffmpeg, cargo, Playwright. BROWSER=webkit|chromium|both (default both).
 * Run audio browser regressions sequentially, without another audio harness.
 * MUSIC_LISTENING_CASES=pause,switch,finish selects cases (default all three).
 * PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs node scripts/verify_listening_alac_browser.mjs
 */
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs/promises';
import http from 'node:http';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const frontend = path.join(root, 'frontend');
const require = createRequire(path.join(frontend, 'package.json'));
const { build } = require('esbuild');
const pw = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : 'playwright');
const engines = process.env.BROWSER && process.env.BROWSER !== 'both' ? [process.env.BROWSER] : ['chromium', 'webkit'];
const directory = await fs.mkdtemp(path.join(tmpdir(), 'listening-alac-'));
const streams = {}, gates = new Map();
const reports = [];
let server;
const command = (binary, args) => execFileSync(binary, args, { cwd: root, stdio: 'pipe' });

try {
  command('cargo', ['build', '--quiet', '--example', 'alac_packet_stream']);
  for (const [name, seconds, hz] of [['alpha', 10, 440], ['beta', 13, 660], ['gamma', 11, 880]]) {
    const prefix = path.join(directory, name);
    command('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', `aevalsrc=0.2*sin(2*PI*${hz}*t)|0.15*sin(2*PI*${hz * 2}*t):s=96000:d=${seconds}`, '-sample_fmt', 's32p', '-c:a', 'alac', `${prefix}.m4a`, '-y']);
    command(path.join(root, 'target/debug/examples/alac_packet_stream'), [`${prefix}.m4a`, `${prefix}.alacstream`]);
    streams[name] = await fs.readFile(`${prefix}.alacstream`);
    assert.equal(streams[name].subarray(0, 8).toString(), 'SPLALAC1');
  }
  const tracks = Object.entries(streams).map(([id, bytes]) => ({
    id: `applemusic--${id}--hires`, playback_id: `applemusic--${id}--hires`, source_id: id, selected_id: id,
    platform: 'applemusic', quality: 'hires', title: `Synthetic ${id}`, artist: 'Test signal',
    duration: { alpha: 10000, beta: 13000, gamma: 11000 }[id], format: 'm4a', byte_size: bytes.length, status: 'completed',
    streaming_mode: 'wasm_alac', streaming_codec: 'alac', streaming_protocol: 'splayer-alac-v2',
  }));
  const entry = `
import { createPinia, setActivePinia } from 'pinia';
import * as player from './src/core/player/index.js';
import { prepareOutput, createNativeOutput } from './src/core/player/audio-output.js';
import { AlacWorkletPlayer } from './src/core/player/alac-worklet.js';
import { getCachedLoudness, warmLoudness } from './src/services/loudness.js';
import { useStatusStore } from './src/stores/status.js';
import { useMediaStore } from './src/stores/media.js';
setActivePinia(createPinia());
const status = useStatusStore(), media = useMediaStore();
const tracks = ${JSON.stringify(tracks)};
const runId = Math.random().toString(36).slice(2);
status.quality = 'hires'; status.volume = .4; status.repeatMode = 'off';
const events = [], records = [], cache = new Map(), sourceNames = new Map(), contexts = [];
let holdFirst = true;
const audio = document.createElement('audio'); audio.id = 'native-unused'; document.body.append(audio);
function source(track) {
  const url = '/api/v1/playbacks/' + encodeURIComponent(track.playback_id || track.id) + '/audio/stream?run=' + runId + (track.source_id === 'alpha' && holdFirst ? '&hold=1' : '');
  sourceNames.set(url, track.source_id); return url;
}
async function cacheTrack(track) {
  if (!cache.has(track.id)) { const response = await fetch(source(track)); cache.set(track.id, await response.blob()); }
  return true;
}
function cachedSource(track) {
  const blob = cache.get(track.id); if (!blob) return null;
  const source = URL.createObjectURL(blob); sourceNames.set(source, track.source_id);
  return { source, track: { ...track, byte_size: blob.size, status: 'completed' } };
}
function createAlac() {
  const worklet = new AlacWorkletPlayer();
  const load = worklet.load.bind(worklet), fade = worklet.fadeTo.bind(worklet);
  worklet.fadeTo = (target, duration, options) => {
    events.push({ kind: 'ramp', deck: records.indexOf(worklet.testRecord), target, duration, at: worklet.context?.currentTime });
    return fade(target, duration, options);
  };
  worklet.load = async (url, options) => {
    const record = { worklet, id: sourceNames.get(url) || 'unknown', source: url, output: null, node: null, analyser: null, completed: false, inputRate: null, bits: null };
    records.push(record); worklet.testRecord = record;
    events.push({ kind: 'load-start', id: record.id, source: String(url).startsWith('blob:') ? 'blob' : 'http', deck: records.length - 1 });
    const completed = worklet.onStreamComplete;
    worklet.onStreamComplete = details => { record.completed = true; events.push({ kind:'stream-eof', id:record.id, bytes:details?.blob?.size }); completed(details); };
    const result = await load(url, options);
    record.output = worklet.output; record.node = worklet.node; record.inputRate = worklet.track?.sampleRate;
    record.bits = worklet.decoder?.bitDepth;
    if (!contexts.includes(worklet.context)) contexts.push(worklet.context);
    const analyser = worklet.context.createAnalyser(); analyser.fftSize = 4096;
    const muted = worklet.context.createGain(); muted.gain.value = 0;
    worklet.output.volumeGain.connect(analyser); analyser.connect(muted); muted.connect(worklet.context.destination);
    record.analyser = analyser;
    events.push({ kind: 'load-ready', id: record.id, deck: records.length - 1, rate: worklet.context.sampleRate });
    return result;
  };
  return worklet;
}
player.initPlayer({ audio, outputFactory: { prepare: prepareOutput, createNative: createNativeOutput },
  alacPlayerFactory: { create: createAlac, canRequest: track => AlacWorkletPlayer.canRequest(track), canHandle: track => AlacWorkletPlayer.canHandle(track) },
  lookupLoudness: getCachedLoudness, warmLoudness,
  resolveTrackSource: async track => cachedSource(track) || { source: source(track), track },
  resolveCachedTrackSource: async track => cachedSource(track),
  cacheActiveTrackSource: async (track, { blob } = {}) => {
    const owned = records.findLast(r => r.worklet.output === r.output && r.output && r.id === track.source_id);
    events.push({kind:'cache-start',id:track.source_id,eof:Boolean(owned?.worklet.eof),fromCompletedBlob:blob instanceof Blob});
    if (!(blob instanceof Blob) || !owned?.worklet.eof) throw new Error('Cache attempted before real ALAC EOF');
    cache.set(track.id, blob); events.push({kind:'cache-complete',id:track.source_id,bytes:blob.size}); return true;
  },
  prefetchTrackSource: async track => { events.push({kind:'prefetch-start',id:track.source_id}); await cacheTrack(track); events.push({kind:'prefetch-complete',id:track.source_id}); return true; },
});
function rms(analyser) { if (!analyser) return 0; const values = new Float32Array(analyser.fftSize); analyser.getFloatTimeDomainData(values); return Math.sqrt(values.reduce((sum, value) => sum + value * value, 0) / values.length); }
function snapshot() {
 return { state:status.state, notice:status.notice, loading:status.trackLoading, current:media.track?.source_id, transition:status.transitionState, position:status.position, crossfade:status.crossfadeEnabled, soundCheck:status.soundCheckEnabled, soundCheckState:status.soundCheckState,
  records: records.map(record => { const {worklet,output,analyser,node} = record; const active = Boolean(node && worklet.node === node && worklet.output === output);
    return {id:record.id,active,playing:active&&worklet.playing,stopped:!active||Boolean(output?.disposed),completed:record.completed,readBytes:active?worklet.readBytes:0,eof:active&&worklet.eof,source:record.source.startsWith('blob:')?'blob':'http',inputRate:record.inputRate,bits:record.bits,
      ...(!active&&worklet.testRecord===record?{loadingDetail:{bytes:worklet.readBytes,packets:worklet.packets.length,reader:Boolean(worklet.reader),context:worklet.context?.state,prepared:worklet.preparedContext?.state,decoder:Boolean(worklet.decoder),output:Boolean(worklet.output),aborted:worklet.abortController?.signal.aborted}}:{}),
      fade:output?.fadeGain.gain.value,normalization:output?.normalizationGain.gain.value,volume:output?.volumeGain.gain.value,context:contexts.indexOf(output?.context),contextTime:output?.context.currentTime,contextState:output?.context.state,outputRate:output?.context.sampleRate,rms:rms(analyser)};
  }), events:[...events] };
}
window.fixture = { player, status, tracks, snapshot };
document.querySelector('#play').onclick = () => { void player.playFrom(tracks); };
document.querySelector('#crossfade').onclick = () => { player.setCrossfadeSeconds(3); player.setCrossfadeEnabled(true); };
document.querySelector('#balance').onclick = () => { player.setSoundCheckEnabled(true); };
document.querySelector('#pause').onclick = () => { player.pause(); };
document.querySelector('#release').onclick = () => { holdFirst = false; void fetch('/__release?run=' + runId); };
document.querySelector('#switch').onclick = () => { void player.playFrom([tracks[0]]); };
document.querySelector('#replay').onclick = () => { void player.playFrom(tracks); };
window.fixtureReady = true;
`;
  const compiled = await build({
    stdin: { contents: entry, resolveDir: frontend, sourcefile: 'listening-alac-fixture.js' },
    bundle: true, write: false, format: 'esm',
    plugins: [{ name: 'fixture-url', setup(build) {
      build.onResolve({ filter: /alac-processor\.js\?url$/ }, args => ({ path: args.path, namespace: 'fixture-url' }));
      build.onLoad({ filter: /.*/, namespace: 'fixture-url' }, () => ({ contents: 'export default "/alac-processor.js"', loader: 'js' }));
    } }],
  });
  const assets = new Map([
    ['/alac-processor.js', { type: 'text/javascript', bytes: await fs.readFile(path.join(frontend, 'src/core/player/alac-processor.js')) }],
    ['/alac_decoder_bg.wasm', { type: 'application/wasm', bytes: await fs.readFile(path.join(frontend, 'src/wasm/alac-decoder/alac_decoder_bg.wasm')) }],
  ]);
  const html = `<!doctype html><html><head><meta charset="utf-8"><title>Synthetic ALAC listening fixture</title></head><body><h1>Synthetic ALAC listening fixture</h1>${['play','crossfade','balance','pause','release','switch','replay'].map(id=>`<button id="${id}">${id}</button>`).join(' ')}<script type="module" src="/fixture.js"></script></body></html>`;
  server = http.createServer(async (request, response) => {
    const url = new URL(request.url, 'http://localhost');
    if (url.pathname === '/') return response.writeHead(200, {
      'content-type':'text/html',
      // Keep media on the browser's ordinary HTTP path while the browser
      // itself blocks outside requests. unsafe-eval is only for this local
      // synthetic WASM fixture, not a suggested production CSP.
      'content-security-policy':"default-src 'self' blob: data:; script-src 'self' 'unsafe-eval'; connect-src 'self' blob:; worker-src 'self' blob:; media-src 'self' blob:",
    }).end(html);
    if (url.pathname === '/fixture.js') return response.writeHead(200, {'content-type':'text/javascript'}).end(compiled.outputFiles[0].text);
    if (url.pathname === '/__release') { gates.get(url.searchParams.get('run'))?.(); return response.end('ok'); }
    if (assets.has(url.pathname)) { const asset = assets.get(url.pathname); return response.writeHead(200, {'content-type':asset.type}).end(asset.bytes); }
    const match = /^\/api\/v1\/playbacks\/applemusic--(alpha|beta|gamma)--hires\/(audio\/stream|loudness)$/.exec(url.pathname);
    if (!match) return response.writeHead(404).end();
    if (match[2] === 'loudness') {
      await new Promise(resolve=>setTimeout(resolve, 800));
      return response.writeHead(200, {'content-type':'application/json','cache-control':'no-store'}).end(JSON.stringify({state:'ready',version:1,integrated_lufs:-8,true_peak_dbfs:-1,gain_db:-8}));
    }
    const bytes = streams[match[1]];
    response.writeHead(200, {'content-type':'application/x-splayer-alac','cache-control':'no-store','content-length':String(bytes.length)});
    if (match[1] === 'alpha' && url.searchParams.has('hold')) {
      // Send ~five seconds as complete packets, then deliberately keep EOF
      // pending. The browser can already play but may not prefetch the next.
      let end = 24 + bytes.readUInt16BE(18), frames = 0;
      while (frames < 96000 * 5 && end < bytes.length) { frames += bytes.readUInt32BE(end + 4); end += 8 + bytes.readUInt32BE(end); }
      const key = url.searchParams.get('run');
      response.write(bytes.subarray(0, end));
      const release = () => { if (!response.destroyed) response.end(bytes.subarray(end)); gates.delete(key); };
      gates.set(key, release);
      response.on('close', () => { if (gates.get(key) === release) gates.delete(key); });
      return;
    }
    response.end(bytes);
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;

  async function runEngine(engine) {
    const results = [], failures = [], evidence = [];
    const check = (name, condition, details) => { results.push({name,passed:Boolean(condition),...(!condition?{details}: {})}); assert.ok(condition,name); };
    let executablePath = process.env.BROWSER_EXECUTABLE;
    if (!executablePath && engine === 'chromium' && process.platform === 'darwin') {
      const candidate = '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge';
      try { await fs.access(candidate); executablePath = candidate; } catch {}
    }
    const browser = await pw[engine].launch({headless:true,...(executablePath?{executablePath}:{})});
    const browserVersion = browser.version();
    const snapshot = page=>page.evaluate(()=>fixture.snapshot());
    const until = async (page, predicate, timeout=18000) => {
      const deadline=Date.now()+timeout;
      while(Date.now()<deadline){const value=await snapshot(page);if(predicate(value))return value;await page.waitForTimeout(60);}
      throw new Error('State timeout: '+JSON.stringify(await snapshot(page)));
    };
    async function fixture() {
      const context=await browser.newContext({serviceWorkers:'block'}),page=await context.newPage(),errors=[],external=[],consoleErrors=[];
      page.on('pageerror',error=>errors.push(error.message));
      page.on('console',message=>{if(['warning','error'].includes(message.type()))consoleErrors.push(message.text());});
      // Do not enable Playwright network interception for held-open media.
      // CSP above blocks external destinations without proxying local streams;
      // request events and policy violations independently record attempts.
      page.on('request',request=>{const url=new URL(request.url());if(url.origin!==origin&&!['blob:','data:'].includes(url.protocol))external.push(url.origin);});
      await page.addInitScript(()=>{window.blockedFixtureRequests=[];document.addEventListener('securitypolicyviolation',event=>window.blockedFixtureRequests.push(event.blockedURI));});
      await page.goto(origin);await page.waitForFunction(()=>window.fixtureReady);
      return {context,page,errors,external,consoleErrors};
    }
    try {
      for (const cancellation of (process.env.MUSIC_LISTENING_CASES?.split(',') || ['pause','switch','finish'])) {
        const {context,page,errors,external,consoleErrors}=await fixture();
        try {
          await page.locator('#crossfade').click();await page.locator('#play').click();
          const started=await until(page,v=>v.state==='playing'&&v.position>250&&v.records[0]?.rms>.001);
          check(`${cancellation}: real 96kHz 24-bit ALAC starts before EOF`,started.records[0].inputRate===96000&&started.records[0].bits===24&&!started.records[0].eof,started);
          await page.waitForTimeout(500);const held=await snapshot(page);
          check(`${cancellation}: no next fetch or cache before current EOF`,!held.events.some(e=>e.kind==='prefetch-start'||e.kind==='cache-start'),held.events);
          await page.locator('#release').click();
          const mixing=await until(page,v=>v.transition==='mixing'&&v.current==='beta'&&v.records.filter(r=>r.playing).length===2);
          const live=mixing.records.filter(r=>r.playing);
          check(`${cancellation}: two real WASM/AudioWorklet decks overlap`,live.length===2&&live.every(r=>r.inputRate===96000&&r.bits===24),mixing);
          check(`${cancellation}: both decks share the 192kHz render clock`,live.every(r=>r.outputRate===192000)&&live[0].context===live[1].context&&live[0].context>=0,live);
          const eof=mixing.events.findIndex(e=>e.kind==='stream-eof'&&e.id==='alpha'),cached=mixing.events.findIndex(e=>e.kind==='cache-complete'&&e.id==='alpha'),prefetch=mixing.events.findIndex(e=>e.kind==='prefetch-start'&&e.id==='beta');
          check(`${cancellation}: complete packet cache gates the next request`,eof>=0&&cached>eof&&prefetch>cached&&mixing.events.some(e=>e.kind==='cache-start'&&e.id==='alpha'&&e.eof&&e.fromCompletedBlob),mixing.events);
          check(`${cancellation}: candidate uses complete local Blob`,live.find(r=>r.id==='beta')?.source==='blob'&&live.find(r=>r.id==='beta')?.completed,live);
          const later=await until(page,v=>v.transition==='mixing'&&v.records.filter(r=>r.playing).length===2&&v.records[1].fade>.2&&v.records[1].fade<.8);
          check(`${cancellation}: actual AudioParam values interpolate`,later.records[0].fade>.1&&later.records[0].fade<.85&&later.records[1].fade>.15&&later.records[1].fade<.9,later.records);
          check(`${cancellation}: both rendered PCM levels are nonzero`,later.records.filter(r=>r.playing).every(r=>r.rms>.001),later.records);
          evidence.push({case:cancellation,phase:'alpha-to-beta',decks:later.records.filter(r=>r.playing)});
          check(`${cancellation}: fade operations schedule both directions`,later.events.some(e=>e.kind==='ramp'&&e.target===0&&e.duration>0)&&later.events.some(e=>e.kind==='ramp'&&e.target===1&&e.duration>0),later.events);
          if(cancellation==='finish') {
            const finished=await until(page,v=>v.transition==='idle'&&v.current==='beta'&&v.records[0].stopped&&v.records[1].playing);
            check('natural ALAC transition disposes old renderer',finished.records[0].stopped&&Math.abs(finished.records[1].fade-1)<.001,finished);
            const secondMix=await until(page,v=>v.transition==='mixing'&&v.current==='gamma'&&v.records[2]?.fade>.2&&v.records[2]?.fade<.8);
            const secondLive=secondMix.records.filter(r=>r.playing);
            check('third cached ALAC track also crossfades with two audible renderers',secondLive.length===2&&secondLive.every(r=>r.rms>.001&&r.outputRate===192000)&&secondLive[0].context===secondLive[1].context,secondMix);
            evidence.push({case:cancellation,phase:'beta-to-gamma',decks:secondLive});
            const third=await until(page,v=>v.transition==='idle'&&v.current==='gamma'&&v.records[1].stopped&&v.records[2].playing);
            check('successive natural transitions leave only the last renderer active',third.records.filter(r=>r.playing).length===1&&third.records[0].stopped&&Math.abs(third.records[2].fade-1)<.001,third);
          } else {
            await page.locator('#'+cancellation).click();
            const cancelled=await until(page,v=>v.transition==='idle'&&(cancellation==='pause'?v.state==='paused':v.state==='playing'&&v.current==='alpha'&&v.records.length>=3));
            check(`${cancellation}: retiring ALAC renderer is stopped`,cancelled.records[0].stopped,cancelled);
            check(`${cancellation}: only requested transport survives`,cancellation==='pause'?cancelled.records.every(r=>!r.playing):cancelled.records[1].stopped&&cancelled.records.filter(r=>r.playing).length===1,cancelled);
            await page.waitForTimeout(500);const settled=await snapshot(page);
            check(`${cancellation}: old PCM cannot resume after delayed callbacks`,settled.transition==='idle'&&settled.records[0].stopped&&(cancellation==='pause'?settled.records.every(r=>!r.playing):settled.records[1].stopped&&settled.records.filter(r=>r.playing).length===1),settled);
          }
          const blocked=await page.evaluate(()=>window.blockedFixtureRequests);
          check(`${cancellation}: no browser errors or external requests`,!errors.length&&!external.length&&!blocked.length,{errors,external,blocked});
        } catch(error){failures.push({case:cancellation,error:error.message,errors,external,consoleErrors});} finally {await context.close();}
      }
    } finally {await browser.close();}
    const report={engine,browserVersion,passed:!failures.length&&results.every(r=>r.passed),checks:results.length,results,evidence,failures};
    reports.push(report);
  }
  for(const engine of engines) await runEngine(engine);
} finally {
  for (const release of gates.values()) release();
  if (server) await new Promise(resolve=>server.close(resolve));
  await fs.rm(directory,{recursive:true,force:true});
}
console.log(JSON.stringify({passed:reports.every(report=>report.passed),reports},null,2));
assert.ok(reports.every(report=>report.passed),'ALAC listening browser regression failed');
