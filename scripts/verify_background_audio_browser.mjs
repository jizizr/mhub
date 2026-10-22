/**
 * Real native + WASM/AudioWorklet background lifecycle/Media Session checks.
 * Uses synthetic local audio and actual production modules. Context suspension
 * and page lifecycle events are deterministic simulations, NOT an assertion
 * that a desktop browser emulates a locked physical iOS/Android device.
 *
 * PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs BROWSER=both node scripts/verify_background_audio_browser.mjs
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
const modes = process.env.MUSIC_BACKGROUND_MODES?.split(',') || ['native', 'alac'];
const directory = await fs.mkdtemp(path.join(tmpdir(), 'background-audio-'));
const reports = [];
let server;

try {
  const command = (binary, args) => execFileSync(binary, args, { cwd: root, stdio: 'pipe' });
  command('cargo', ['build', '--quiet', '--example', 'alac_packet_stream']);
  command('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'aevalsrc=0.08*sin(2*PI*440*t)|0.06*sin(2*PI*660*t):s=96000:d=60', '-sample_fmt', 's32p', '-c:a', 'alac', path.join(directory, 'test.m4a'), '-y']);
  command(path.join(root, 'target/debug/examples/alac_packet_stream'), [path.join(directory, 'test.m4a'), path.join(directory, 'test.alacstream')]);
  command('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000:duration=60', '-c:a', 'pcm_s16le', path.join(directory, 'test.wav'), '-y']);
  const alac = await fs.readFile(path.join(directory, 'test.alacstream'));
  const wav = await fs.readFile(path.join(directory, 'test.wav'));
  const entry = `
import { createPinia, setActivePinia } from 'pinia';
import * as player from './src/core/player/index.js';
import { prepareOutput, preparePlaybackSession, createNativeOutput } from './src/core/player/audio-output.js';
import { createPlaybackLifecycle } from './src/core/player/playback-lifecycle.js';
import { createMediaSessionController } from './src/core/player/media-session.js';
import { AlacWorkletPlayer } from './src/core/player/alac-worklet.js';
import { useStatusStore } from './src/stores/status.js';
import { useMediaStore } from './src/stores/media.js';
setActivePinia(createPinia());
const mode = new URL(location.href).searchParams.get('mode');
const status = useStatusStore(), media = useMediaStore();
status.quality = mode === 'alac' ? 'hires' : 'high'; status.volume = .3; status.repeatMode = 'off';
const tracks = ['alpha', 'beta'].map(id => ({ id:'applemusic--'+id+'--'+status.quality, playback_id:'applemusic--'+id+'--'+status.quality, source_id:id, selected_id:id, platform:'applemusic', title:'Synthetic '+id, artist:'Test signal', album:'Public fixture', quality:status.quality, duration:60000, status:'completed', format:mode==='alac'?'m4a':'wav', ...(mode==='alac'?{streaming_mode:'wasm_alac',streaming_codec:'alac',streaming_protocol:'splayer-alac-v2'}:{}) }));
const audio = document.createElement('audio'); audio.id = 'native-audio'; audio.preload = 'auto'; audio.crossOrigin = 'anonymous'; audio.playsInline = true; document.body.append(audio);
const outputs = [], worklets = [], contexts = [];
function observeOutput(output) {
  if (!output || outputs.some(record=>record.output===output)) return;
  const analyser=output.context.createAnalyser(); analyser.fftSize=4096;
  const silent=output.context.createGain(); silent.gain.value=0;
  output.volumeGain.connect(analyser); analyser.connect(silent); silent.connect(output.context.destination);
  outputs.push({output,analyser}); if(!contexts.includes(output.context))contexts.push(output.context);
}
function createAlac() {
  const instance=new AlacWorkletPlayer(); worklets.push(instance);
  const load=instance.load.bind(instance);
  instance.load=async (...args)=>{const result=await load(...args);observeOutput(instance.output);return result;};
  return instance;
}
const source = '/media.'+(mode==='alac'?'alacstream':'wav');
player.initPlayer({audio,
  mediaSessionFactory:createMediaSessionController,lifecycleFactory:createPlaybackLifecycle,
  outputFactory:{prepare:prepareOutput,prepareSession:preparePlaybackSession,createNative(element){const output=createNativeOutput(element);observeOutput(output);return output;}},
  alacPlayerFactory:{create:createAlac,canRequest:track=>AlacWorkletPlayer.canRequest(track),canHandle:track=>AlacWorkletPlayer.canHandle(track)},
  resolveTrackSource:async track=>({source,track}),
});
function rms(analyser) {const values=new Float32Array(analyser.fftSize);analyser.getFloatTimeDomainData(values);return Math.sqrt(values.reduce((sum,v)=>sum+v*v,0)/values.length);}
function snapshot(){const session=navigator.mediaSession;return {mode,state:status.state,position:status.position,current:media.track?.source_id,notice:status.notice,metadata:session?.metadata?{title:session.metadata.title,artist:session.metadata.artist,album:session.metadata.album}:null,mediaState:session?.playbackState,actions:Object.keys(window.fixtureActions||{}),audioSession:navigator.audioSession?.type,contexts:contexts.map(c=>({state:c.state,time:c.currentTime,rate:c.sampleRate})),outputs:outputs.map(({output,analyser})=>({disposed:output.disposed,rms:rms(analyser)})),elements:[...document.querySelectorAll('audio')].map(el=>({id:el.id,paused:el.paused,src:el.getAttribute('src'),stream:el.srcObject instanceof MediaStream,tracks:el.srcObject?.getAudioTracks?.().map(t=>({state:t.readyState,enabled:t.enabled}))||[],time:el.currentTime})),worklets:worklets.map(w=>({playing:w.playing,loaded:w.loaded,eof:w.eof,queue:w.queuedFrames,rate:w.outputSampleRate,position:w.consumedFrames}))};}
window.fixture={player,status,tracks,audio,contexts,worklets,snapshot,
  async interrupt(){await Promise.all(contexts.filter(c=>c.state==='running').map(c=>c.suspend()));},
  lifecycle(){window.dispatchEvent(new PageTransitionEvent('pageshow',{persisted:true}));document.dispatchEvent(new Event('visibilitychange'));},
  async action(name,details={}){const handler=window.fixtureActions?.[name];if(!handler)throw new Error('No '+name+' handler');await handler(details);}
};
document.querySelector('#play').onclick=()=>{if(mode==='native')player.setSoundCheckEnabled(true);void player.playFrom(tracks);};
document.querySelector('#pause').onclick=()=>player.pause();
document.querySelector('#resume').onclick=()=>void player.play();
window.fixtureReady=true;
`;
  const compiled = await build({ stdin: { contents: entry, resolveDir: frontend, sourcefile: 'background-audio-fixture.js' }, bundle: true, write: false, format: 'esm', plugins: [{ name: 'fixture-url', setup(b) {
    b.onResolve({ filter: /alac-processor\.js\?url$/ }, args => ({ path: args.path, namespace: 'fixture-url' }));
    b.onLoad({ filter: /.*/, namespace: 'fixture-url' }, () => ({ contents: 'export default "/alac-processor.js"', loader: 'js' }));
  } }] });
  const assets = new Map([
    ['/fixture.js', { type: 'text/javascript', bytes: compiled.outputFiles[0].text }],
    ['/alac-processor.js', { type: 'text/javascript', bytes: await fs.readFile(path.join(frontend, 'src/core/player/alac-processor.js')) }],
    ['/alac_decoder_bg.wasm', { type: 'application/wasm', bytes: await fs.readFile(path.join(frontend, 'src/wasm/alac-decoder/alac_decoder_bg.wasm')) }],
    ['/media.alacstream', { type: 'application/x-splayer-alac', bytes: alac }],
    ['/media.wav', { type: 'audio/wav', bytes: wav }],
  ]);
  const html = '<!doctype html><html><head><meta charset="utf-8"><title>Synthetic background audio fixture</title></head><body><h1>Synthetic background audio fixture</h1><button id="play">Play</button><button id="pause">Pause</button><button id="resume">Resume</button><script type="module" src="/fixture.js"></script></body></html>';
  server = http.createServer((request,response)=>{
    const url=new URL(request.url,'http://localhost');
    if(url.pathname==='/')return response.writeHead(200,{'content-type':'text/html','content-security-policy':"default-src 'self' blob: data:; script-src 'self' 'unsafe-eval'; connect-src 'self' blob:; worker-src 'self' blob:; media-src 'self' blob:"}).end(html);
    const asset=assets.get(url.pathname);if(!asset)return response.writeHead(404).end();
    const bytes=asset.bytes,headers={'content-type':asset.type,'cache-control':'no-store','accept-ranges':'bytes'};
    const range=/^bytes=(\d+)-(\d*)$/.exec(request.headers.range||'');
    if(range&&Buffer.isBuffer(bytes)){const start=Number(range[1]),end=Math.min(Number(range[2]||bytes.length-1),bytes.length-1);return response.writeHead(206,{...headers,'content-range':`bytes ${start}-${end}/${bytes.length}`,'content-length':String(end-start+1)}).end(bytes.subarray(start,end+1));}
    response.writeHead(200,{...headers,'content-length':String(Buffer.byteLength(bytes))}).end(bytes);
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const origin=`http://127.0.0.1:${server.address().port}`;
  for(const engine of engines){
    let executablePath=process.env.BROWSER_EXECUTABLE;
    if(!executablePath&&engine==='chromium'&&process.platform==='darwin'){const candidate='/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge';try{await fs.access(candidate);executablePath=candidate;}catch{}}
    const browser=await pw[engine].launch({headless:true,...(executablePath?{executablePath}:{})});
    const results=[],failures=[],evidence=[];
    const check=(name,condition,details)=>{results.push({name,passed:Boolean(condition),...(!condition?{details}:{})});assert.ok(condition,name);};
    try{
      for(const mode of modes){
        const context=await browser.newContext({serviceWorkers:'block'}),page=await context.newPage(),errors=[],external=[];
        page.on('pageerror',error=>errors.push(error.message));
        page.on('request',request=>{const url=new URL(request.url());if(url.origin!==origin&&!['blob:','data:'].includes(url.protocol))external.push(url.origin);});
        await page.addInitScript(()=>{window.fixtureActions={};const session=navigator.mediaSession;if(session){const original=session.setActionHandler.bind(session);session.setActionHandler=(name,handler)=>{const result=original(name,handler);if(handler)window.fixtureActions[name]=handler;else delete window.fixtureActions[name];return result;};}});
        const snapshot=()=>page.evaluate(()=>fixture.snapshot());
        const until=async(predicate,timeout=12000)=>{const deadline=Date.now()+timeout;while(Date.now()<deadline){const value=await snapshot();if(predicate(value))return value;await page.waitForTimeout(70);}throw new Error('State timeout: '+JSON.stringify(await snapshot()));};
        try{
          await page.goto(origin+'/?mode='+mode);await page.waitForFunction(()=>window.fixtureReady);
          await page.locator('#play').click();
          const started=await until(v=>v.state==='playing'&&v.position>300&&v.outputs.some(o=>o.rms>.001));
          check(mode+': actual audio renders',started.contexts.every(c=>c.state==='running')&&started.outputs.some(o=>o.rms>.001),started);
          check(mode+': current public metadata registered',started.metadata?.title==='Synthetic alpha'&&started.metadata?.artist==='Test signal',started);
          check(mode+': playing state registered',started.mediaState==='playing',started);
          check(mode+': lock-screen controls registered',['play','pause','previoustrack','nexttrack','seekto','seekbackward','seekforward'].every(action=>started.actions.includes(action)),started.actions);
          if(started.audioSession!==undefined)check(mode+': audio session uses playback category',started.audioSession==='playback',started.audioSession);
          if(mode==='alac')check('alac: actual WASM renderer runs without a silent media keepalive',started.worklets.some(w=>w.playing&&w.loaded)&&started.elements.every(el=>el.paused),started);
          evidence.push({mode,phase:'playing',snapshot:started});
          const before=started.position;
          await page.evaluate(async()=>{await fixture.interrupt();fixture.lifecycle();});
          const recovered=await until(v=>v.state==='playing'&&v.position>before+300&&v.contexts.every(c=>c.state==='running')&&v.outputs.some(o=>o.rms>.001));
          check(mode+': playing context recovers after lifecycle interruption',recovered.current==='alpha'&&recovered.mediaState==='playing',recovered);
          await page.evaluate(()=>fixture.action('seekto',{seekTime:12}));
          const sought=await until(v=>v.position>=12000&&v.position<14500&&v.state==='playing');
          check(mode+': system seek preserves current track',sought.current==='alpha',sought);
          await page.evaluate(()=>fixture.action('pause'));
          const paused=await until(v=>v.state==='paused'&&v.mediaState==='paused');
          await page.evaluate(async()=>{await fixture.interrupt();fixture.lifecycle();});
          await page.waitForTimeout(500);
          const keptPaused=await snapshot();
          check(mode+': lifecycle does not override explicit system pause',keptPaused.state==='paused'&&keptPaused.mediaState==='paused'&&Math.abs(keptPaused.position-paused.position)<180,keptPaused);
          await page.locator('#resume').click();
          const resumed=await until(v=>v.state==='playing'&&v.position>keptPaused.position+250&&v.mediaState==='playing');
          check(mode+': user resume restores rendered audio',resumed.outputs.some(o=>o.rms>.001),resumed);
          await page.evaluate(()=>fixture.action('nexttrack'));
          const next=await until(v=>v.current==='beta'&&v.state==='playing'&&v.position>250&&v.metadata?.title==='Synthetic beta');
          check(mode+': next action updates native metadata',next.mediaState==='playing',next);
          await page.evaluate(()=>fixture.action('pause'));
          await until(v=>v.state==='paused'&&v.mediaState==='paused');
          if(next.actions.includes('stop')){
            await page.evaluate(()=>fixture.action('stop'));
            const stopped=await until(v=>v.state==='stopped'&&v.mediaState==='none');
            check(mode+': stop clears media ownership and metadata',stopped.metadata===null&&!stopped.worklets.some(w=>w.playing),stopped);
          }
          check(mode+': isolated fixture has no page errors or outside requests',!errors.length&&!external.length,{errors,external});
        }catch(error){failures.push({mode,error:error.stack||error.message});}finally{await context.close();}
      }
    }finally{await browser.close();}
    reports.push({engine,version:browser.version(),results,failures,evidence});
  }
  process.stdout.write(JSON.stringify({scope:'Desktop browser deterministic lifecycle tests; physical mobile lock-screen verification remains device-specific.',reports},null,2)+'\n');
  if(reports.some(report=>report.failures.length))process.exitCode=1;
}finally{
  if(server){server.closeAllConnections();await new Promise(resolve=>server.close(resolve));}
  await fs.rm(directory,{recursive:true,force:true});
}
