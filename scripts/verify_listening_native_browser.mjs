/**
 * Native listening effects regression using the actual player/audio modules.
 * All media and API responses are synthetic and served by this local fixture.
 * No production service, account, or personal music is accessed.
 *
 * PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs BROWSER=webkit node scripts/verify_listening_native_browser.mjs
 * BROWSER=chromium BROWSER_EXECUTABLE='/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge' ...
 * BROWSER defaults to both, with Edge on macOS when installed.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const frontend = path.join(root, 'frontend');
const require = createRequire(path.join(frontend, 'package.json'));
const { build } = require('esbuild');
const pw = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : 'playwright');
const onlyCases = process.env.MUSIC_LISTENING_CASES?.split(',');
const engines = process.env.BROWSER && process.env.BROWSER !== 'both' ? [process.env.BROWSER] : ['chromium', 'webkit'];

function wav(seconds, frequency) {
  const rate = 16000, frames = rate * seconds, bytes = Buffer.alloc(44 + frames * 2);
  bytes.write('RIFF'); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8);
  bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22);
  bytes.writeUInt32LE(rate, 24); bytes.writeUInt32LE(rate * 2, 28); bytes.writeUInt16LE(2, 32); bytes.writeUInt16LE(16, 34);
  bytes.write('data', 36); bytes.writeUInt32LE(frames * 2, 40);
  for (let i = 0; i < frames; i++) bytes.writeInt16LE(Math.round(Math.sin(i / rate * Math.PI * 2 * frequency) * 1800), 44 + i * 2);
  return bytes;
}
const mediaBytes = { alpha: wav(9, 440), beta: wav(12, 660) };
const tracks = Object.entries(mediaBytes).map(([id, bytes]) => ({ id: `applemusic--${id}--high`, playback_id: `applemusic--${id}--high`, source_id: id, selected_id: id, platform: 'applemusic', quality: 'high', title: `Synthetic ${id}`, artist: 'Test signal', duration: id === 'alpha' ? 9000 : 12000, format: 'wav', byte_size: bytes.length, status: 'completed' }));
const entry = `
import { createPinia, setActivePinia } from 'pinia';
import * as player from './src/core/player/index.js';
import { prepareOutput, createNativeOutput } from './src/core/player/audio-output.js';
import { getCachedLoudness, warmLoudness } from './src/services/loudness.js';
import { useStatusStore } from './src/stores/status.js';
import { useQueueStore } from './src/stores/queue.js';
import { useMediaStore } from './src/stores/media.js';
setActivePinia(createPinia());
const status = useStatusStore(), queue = useQueueStore(), media = useMediaStore();
const tracks = ${JSON.stringify(tracks)};
status.quality = 'high'; status.volume = .4; status.repeatMode = 'off';
const events = [], outputs = [], cache = new Map();
const audio = document.createElement('audio'); audio.crossOrigin = 'anonymous'; audio.id = 'initial-audio'; audio.preload = 'auto'; document.body.append(audio);
const source = track => '/api/v1/playbacks/' + encodeURIComponent(track.playback_id || track.id) + '/audio';
async function cacheTrack(track) {
  if (!cache.has(track.id)) { const response = await fetch(source(track)); cache.set(track.id, await response.blob()); }
  return true;
}
function cachedSource(track) {
  const blob = cache.get(track.id); return blob ? { source: URL.createObjectURL(blob), track } : null;
}
const outputFactory = {
  prepare: prepareOutput,
  createNative(element) {
    const output = createNativeOutput(element);
    const analyser = output.context.createAnalyser(); analyser.fftSize = 2048;
    const muted = output.context.createGain(); muted.gain.value = 0;
    output.volumeGain.connect(analyser); analyser.connect(muted); muted.connect(output.context.destination);
    const record = { output, element, analyser }; outputs.push(record);
    const normalize = output.setLoudnessGain.bind(output); output.setLoudnessGain = value => { events.push({kind:'normalize',value,now:output.context.currentTime}); return normalize(value); };
    const fade = output.fadeTo.bind(output);
    output.fadeTo = (value, seconds, options) => { events.push({ kind: 'ramp', deck: outputs.indexOf(record), value, at: output.context.currentTime + seconds, now: output.context.currentTime }); return fade(value, seconds, options); };
    return output;
  },
};
player.initPlayer({ audio, outputFactory, lookupLoudness: getCachedLoudness, warmLoudness,
  resolveTrackSource: async track => cachedSource(track) || { source: source(track), track },
  resolveCachedTrackSource: async track => cachedSource(track),
  cacheActiveTrackSource: async track => { events.push({kind:'cache-start', id:track.source_id}); await cacheTrack(track); events.push({kind:'cache-complete',id:track.source_id}); return true; },
  prefetchTrackSource: async track => { events.push({kind:'prefetch-start',id:track.source_id}); await cacheTrack(track); events.push({kind:'prefetch-complete',id:track.source_id}); return true; },
});
function rms(analyser) { const values = new Float32Array(analyser.fftSize); analyser.getFloatTimeDomainData(values); return Math.sqrt(values.reduce((sum, value) => sum + value * value, 0) / values.length); }
function snapshot() {
 return { state:status.state, current:media.track?.source_id, transition:status.transitionState, position:status.position, crossfade:status.crossfadeEnabled, soundCheck:status.soundCheckEnabled, soundCheckState:status.soundCheckState,
  audio:[...document.querySelectorAll('audio')].map(element=>({time:element.currentTime,paused:element.paused,src:element.currentSrc||element.src,attached:element.hasAttribute('src'),duration:Number.isFinite(element.duration)?element.duration:null})),
  outputs:outputs.map(({output,element,analyser})=>({time:element.currentTime,paused:element.paused,src:element.currentSrc||element.src,attached:element.hasAttribute('src'),fade:output.fadeGain.gain.value,normalization:output.normalizationGain.gain.value,volume:output.volumeGain.gain.value,contextTime:output.context.currentTime,contextState:output.context.state,rms:rms(analyser)})), events:[...events] };
}
window.fixture = { player, status, tracks, snapshot };
document.querySelector('#play').onclick = () => { void player.playFrom(tracks); };
document.querySelector('#crossfade').onclick = () => { player.setCrossfadeSeconds(3); player.setCrossfadeEnabled(true); };
document.querySelector('#balance').onclick = () => { player.setSoundCheckEnabled(true); };
document.querySelector('#pause').onclick = () => { player.pause(); };
document.querySelector('#seek').onclick = () => { player.seek(1000); };
document.querySelector('#replay').onclick = () => { void player.playAtIndex(1).then(() => player.playAtIndex(0)); };
window.fixtureReady = true;
`;
const compiled = await build({ stdin: { contents: entry, resolveDir: frontend, sourcefile: 'listening-native-fixture.js' }, bundle: true, write: false, format: 'iife' });
const html = `<!doctype html><html><head><meta charset="utf-8"><title>Synthetic listening fixture</title></head><body><h1>Synthetic listening fixture</h1>${['play','crossfade','balance','pause','seek','replay'].map(id=>`<button id="${id}">${id}</button>`).join(' ')}<script src="/fixture.js"></script></body></html>`;
const server = http.createServer(async (request, response) => {
  const url = new URL(request.url, 'http://localhost');
  if (url.pathname === '/') return response.writeHead(200, {'content-type':'text/html'}).end(html);
  if (url.pathname === '/fixture.js') return response.writeHead(200, {'content-type':'text/javascript'}).end(compiled.outputFiles[0].text);
  const match = /^\/api\/v1\/playbacks\/applemusic--(alpha|beta)--high\/(audio|loudness)$/.exec(url.pathname);
  if (!match) return response.writeHead(404).end();
  if (match[2] === 'loudness') {
    // Analysis arrives after playback starts, proving there is no gain jump.
    await new Promise(resolve=>setTimeout(resolve, 800));
    return response.writeHead(200, {'content-type':'application/json','cache-control':'no-store'}).end(JSON.stringify({ state:'ready',version:1,integrated_lufs:-8,true_peak_dbfs:-1,gain_db:-8 }));
  }
  const bytes = mediaBytes[match[1]];
  const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range || '');
  const headers = {'content-type':'audio/wav','accept-ranges':'bytes','cache-control':'no-store','access-control-allow-origin':'*'};
  if (range) {
    const start=Number(range[1]),end=Math.min(Number(range[2]||bytes.length-1),bytes.length-1);
    return response.writeHead(206,{...headers,'content-range':`bytes ${start}-${end}/${bytes.length}`,'content-length':String(end-start+1)}).end(bytes.subarray(start,end+1));
  }
  return response.writeHead(200,{...headers,'content-length':String(bytes.length)}).end(bytes);
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
const reports = [];
async function runEngine(engine) {
  const results = [], failures = [];
  const check = (name, condition, details) => { results.push({name,passed:Boolean(condition),...(!condition?{details}: {})}); assert.ok(condition,name); };
  let executablePath = process.env.BROWSER_EXECUTABLE;
  if (!executablePath && engine === 'chromium' && process.platform === 'darwin') {
    const candidate = '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge';
    try { await fs.access(candidate); executablePath = candidate; } catch {}
  }
  const browser = await pw[engine].launch({headless:true,...(executablePath?{executablePath}:{})});
  const snapshot = page=>page.evaluate(()=>fixture.snapshot());
  const until = async (page, predicate, timeout=14000) => {
    const deadline=Date.now()+timeout;
    while(Date.now()<deadline){const value=await snapshot(page);if(predicate(value))return value;await page.waitForTimeout(60);}
    throw new Error('State timeout: '+JSON.stringify(await snapshot(page)));
  };
  async function fixture() {
    const context=await browser.newContext({serviceWorkers:'block'}),page=await context.newPage(),errors=[],external=[];
    page.on('pageerror',error=>errors.push(error.message));
    await page.route('**/*',route=>{const url=new URL(route.request().url());if(url.origin===origin||['blob:','data:'].includes(url.protocol))return route.continue();external.push(url.origin);return route.abort();});
    await page.goto(origin);await page.waitForFunction(()=>window.fixtureReady);
    return {context,page,errors,external};
  }
  try {
    if (!onlyCases || onlyCases.includes('loudness')) {
      const {context,page,errors,external}=await fixture();
      try {
        const initial=await snapshot(page);check('effects default off',!initial.crossfade&&!initial.soundCheck);
        await page.locator('#play').click();
        const direct=await until(page,v=>v.audio[0]?.time>.15&&!v.audio[0].paused);
        check('default native playback has no processing graph',direct.outputs.length===0);
        await page.locator('#balance').click();
        const loading=await until(page,v=>v.outputs.length===1&&v.soundCheckState==='pending');
        check('unknown loudness starts at unity',Math.abs(loading.outputs[0].normalization-1)<.001);
        const warmed=await until(page,v=>v.soundCheckState==='ready');
        check('analysis completion never changes current gain',Math.abs(warmed.outputs[0].normalization-1)<.001&&warmed.audio[0].time>loading.audio[0].time);
        await page.locator('#replay').click();
        const replay=await until(page,v=>v.audio[0].time>.15&&!v.audio[0].paused&&Math.abs(v.outputs[0].normalization-Math.pow(10,-8/20))<.0001);
        check('cached loudness applies on subsequent play',Math.abs(replay.outputs[0].normalization-Math.pow(10,-8/20))<.002);
        await page.waitForTimeout(500);const stable=await snapshot(page);
        check('subsequent play keeps a fixed gain',Math.abs(stable.outputs[0].normalization-replay.outputs[0].normalization)<.001);
        const audible = await until(page,v=>v.current==='alpha'&&v.outputs[0].rms>.001,5000);
        check('normalization uses rendered PCM',audible.outputs[0].rms>0.001&&audible.outputs[0].contextState==='running',audible.outputs[0]);
        check('normalization fixture has no errors or external requests',!errors.length&&!external.length,{errors,external});
      } catch(error){failures.push({case:'loudness',error:error.message});} finally {await context.close();}
    }
    for (const cancellation of ['pause','seek','finish'].filter(value=>!onlyCases||onlyCases.includes(value))) {
      const {context,page,errors,external}=await fixture();
      try {
        await page.locator('#crossfade').click();await page.locator('#play').click();
        const mixing=await until(page,v=>v.transition==='mixing'&&v.current==='beta'&&v.outputs.length===2&&v.outputs.every(o=>o.time>.05&&!o.paused));
        check(`${cancellation}: two real audio elements overlap`,mixing.outputs.every(o=>!o.paused&&o.time>0),mixing);
        const ramps=mixing.events.filter(event=>event.kind==='ramp');
        check(`${cancellation}: scheduled AudioParam ramps fade both decks`,ramps.some(r=>r.value===0&&r.at>r.now)&&ramps.some(r=>r.value===1&&r.at>r.now),ramps);
        check(`${cancellation}: current cache completes before next fetch`,mixing.events.findIndex(e=>e.kind==='cache-complete'&&e.id==='alpha')<mixing.events.findIndex(e=>e.kind==='prefetch-start'&&e.id==='beta'),mixing.events);
        const later=await until(page,v=>v.transition==='mixing'&&v.outputs[1].fade>.2&&v.outputs[1].fade<.8);
        check(`${cancellation}: real gain values interpolate`,later.outputs[0].fade<.85&&later.outputs[0].fade>.1&&later.outputs[1].fade>.15&&later.outputs[1].fade<.9,later.outputs);
        const audible = await until(page,v=>v.transition==='mixing'&&v.outputs.every(o=>o.rms>.001),3500);
        check(`${cancellation}: both decks render PCM during overlap`,audible.outputs.every(o=>o.rms>.001),audible.outputs);
        if(cancellation==='finish'){
          const finished=await until(page,v=>v.transition==='idle'&&v.current==='beta'&&v.outputs[0].paused&&!v.outputs[1].paused);
          check('natural transition releases retiring source',!finished.outputs[0].attached&&finished.outputs[1].time>audible.outputs[1].time&&Math.abs(finished.outputs[1].fade-1)<.001,finished);
        } else {
          await page.locator('#'+cancellation).click();await page.waitForTimeout(300);const cancelled=await snapshot(page);
          check(`${cancellation}: overlap cancelled and retired deck stopped`,cancelled.transition==='idle'&&cancelled.outputs[0].paused&&!cancelled.outputs[0].attached,cancelled);
          check(`${cancellation}: surviving deck returns to full fade gain`,Math.abs(cancelled.outputs[1].fade-1)<.001,cancelled.outputs);
          check(`${cancellation}: transport follows user action`,cancellation==='pause'?cancelled.state==='paused'&&cancelled.audio.every(a=>a.paused):cancelled.state==='playing'&&!cancelled.outputs[1].paused&&cancelled.outputs[1].time>=1&&cancelled.outputs[1].time<2,cancelled);
          await page.waitForTimeout(500);const settled=await snapshot(page);
          check(`${cancellation}: no stale deck resumes`,settled.transition==='idle'&&settled.outputs[0].paused&&(cancellation!=='pause'||settled.audio.every(a=>a.paused)),settled);
        }
        check(`${cancellation}: no browser errors or external requests`,!errors.length&&!external.length,{errors,external});
      } catch(error){failures.push({case:cancellation,error:error.message});} finally {await context.close();}
    }
  } finally {await browser.close();}
  const report={engine,passed:failures.length===0&&results.every(r=>r.passed),checks:results.length,results,failures};reports.push(report);console.error(`${engine}: ${results.filter(r=>r.passed).length}/${results.length} passed`);
}
try {for(const engine of engines)await runEngine(engine);} finally {await new Promise(resolve=>server.close(resolve));}
console.log(JSON.stringify({passed:reports.every(report=>report.passed),reports},null,2));
assert.ok(reports.every(report=>report.passed),'Native listening browser regression failed');
