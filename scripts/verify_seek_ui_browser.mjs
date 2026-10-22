/** Actual PlayerBar/FullPlayer pointer and keyboard seeking, using synthetic
 * native WAV and real WASM ALAC. No production account or media is accessed.
 * PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs BROWSER=both node scripts/verify_seek_ui_browser.mjs
 * Run serially with other audio browser checks.
 */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const frontend = path.join(root, "frontend");
const { createServer } = await import(pathToFileURL(path.join(frontend, "node_modules/vite/dist/node/index.js")));
const playwright = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const engines = process.env.BROWSER && process.env.BROWSER !== "both" ? [process.env.BROWSER] : ["chromium", "webkit"];
const directory = await fs.mkdtemp(path.join(frontend, ".seek-ui-fixture-"));
const command = (binary, args) => execFileSync(binary, args, { cwd: root, stdio: "pipe" });
const reports = [];
let server;
let browser;
try {
  command("cargo", ["build", "--quiet", "--example", "alac_packet_stream"]);
  const source = path.join(directory, "signal");
  command("ffmpeg", ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "aevalsrc=0.15*sin(2*PI*440*t)|0.12*sin(2*PI*660*t):s=96000:d=40", "-c:a", "alac", `${source}.m4a`, "-y"]);
  command(path.join(root, "target/debug/examples/alac_packet_stream"), [`${source}.m4a`, `${source}.alacstream`]);
  command("ffmpeg", ["-hide_banner", "-loglevel", "error", "-i", `${source}.m4a`, "-ar", "16000", "-c:a", "pcm_s16le", `${source}.wav`, "-y"]);
  const fixture = `
import { createApp, h } from 'vue';
import { createPinia, setActivePinia } from 'pinia';
import PlayerBar from '../src/components/player/PlayerBar.vue';
import FullPlayer from '../src/components/player/FullPlayer.vue';
import * as player from '../src/core/player/index.js';
import { AlacWorkletPlayer } from '../src/core/player/alac-worklet.js';
import { useMediaStore } from '../src/stores/media.js';
import { useStatusStore } from '../src/stores/status.js';
import '../src/styles.css';
const pinia = createPinia(); setActivePinia(pinia);
const status = useStatusStore(), media = useMediaStore();
const alac = new URLSearchParams(location.search).get('transport') === 'alac';
const track = { id: alac ? 'test--seek--hires' : 'test--seek--high', source_id:'seek', platform:'test', quality:alac?'hires':'high', duration:40000, title:'Synthetic seek test', artist:'Test signal', ...(alac ? {streaming_mode:'wasm_alac',streaming_codec:'alac',streaming_protocol:'splayer-alac-v2'} : {}) };
status.quality = track.quality; status.repeatMode = 'off';
const calls = [];
const audio = document.createElement('audio'); audio.preload='auto'; document.body.append(audio);
const nativeTime = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'currentTime');
Object.defineProperty(audio, 'currentTime', {get(){return nativeTime.get.call(this)},set(value){calls.push(value*1000);nativeTime.set.call(this,value)}});
let worklet, acceptedSeek = 0;
player.initPlayer({ audio,
  resolveTrackSource: async () => ({source:'./signal.' + (alac?'alacstream':'wav'),track}),
  loadLyrics:async()=>[],
  alacPlayerFactory: { canRequest:()=>alac, canHandle:()=>alac, create(){
    worklet=new AlacWorkletPlayer(); const seek=worklet.seek.bind(worklet);
    worklet.seek=async value=>{calls.push(value);const accepted=await seek(value);acceptedSeek=accepted;return accepted}; return worklet;
  }},
});
createApp({render:()=>[h(PlayerBar),h(FullPlayer)]}).use(pinia).mount('#app');
document.querySelector('#start').onclick=()=>player.playFrom([track]);
window.fixture={player,status,media,track,calls,snapshot:()=>({state:status.state,position:status.position,loading:status.trackLoading,calls:[...calls],eof:worklet?.eof,actual:alac?acceptedSeek:audio.currentTime*1000})};
window.fixtureReady=true;
`;
  await fs.writeFile(path.join(directory, "fixture.js"), fixture);
  await fs.writeFile(path.join(directory, "index.html"), '<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1"><title>Synthetic seek UI</title></head><body><button id="start">Start synthetic playback</button><div id="app"></div><script type="module" src="./fixture.js"></script></body></html>');
  server = await createServer({ root: frontend, logLevel: "warn", server: { host: "127.0.0.1", port: 0, watch: null } });
  await server.listen();
  const base = `http://127.0.0.1:${server.httpServer.address().port}/${path.basename(directory)}/index.html`;
  for (const engine of engines) {
    const executablePath = process.env.BROWSER_EXECUTABLE || (engine === "chromium" && process.platform === "darwin" ? "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge" : undefined);
    browser = await playwright[engine].launch({ headless: true, ...(executablePath ? { executablePath } : {}) });
    for (const transport of ["native", "alac"]) {
      const context = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true });
      const page = await context.newPage();
      const checks = [], errors = [];
      const check = (name, ok, details) => checks.push({name, passed:Boolean(ok), ...(!ok?{details}: {})});
      page.on("pageerror", error => errors.push(error.message));
      try {
        await page.goto(`${base}?transport=${transport}`);
        await page.waitForFunction(() => window.fixtureReady);
        await page.locator("#start").click();
        await page.waitForFunction(() => window.fixture.status.state === "playing", null, { timeout: 15000 });
        if (transport === "alac") await page.waitForFunction(() => window.fixture.snapshot().eof === true);
        await page.evaluate(() => { window.fixture.player.pause(); window.fixture.calls.length=0; });
        for (const layout of ["compact", "full"]) {
          if (layout === "full") {
            await page.locator(".player-bar .track-info").click();
            await page.locator(".full-player").waitFor();
            await page.waitForTimeout(400);
          }
          const input = page.locator(layout === "compact" ? ".player-bar .seek-slider" : ".full-player .seek-slider");
          const box = await input.boundingBox();
          const at = fraction => ({x:box.x+box.width*fraction,y:box.y+box.height/2});
          const before = await page.evaluate(() => window.fixture.calls.length);
          await page.mouse.move(at(.2).x,at(.2).y); await page.mouse.down();
          await page.mouse.move(at(.7).x,at(.7).y,{steps:8});
          await page.evaluate(() => {window.fixture.status.position=1000});
          await page.waitForTimeout(30);
          check(`${layout}: live clock cannot overwrite drag preview`, Math.abs(Number(await input.inputValue())-28000)<100, await input.inputValue());
          check(`${layout}: dragging does not repeatedly seek`, await page.evaluate(() => window.fixture.calls.length)===before);
          await page.mouse.up();
          await page.waitForFunction(() => Math.abs(window.fixture.status.position-28000)<150);
          await page.waitForFunction(() => Math.abs(window.fixture.snapshot().actual-28000)<200);
          check(`${layout}: release commits exactly once`, await page.evaluate(() => window.fixture.calls.length)===before+1, await page.evaluate(()=>window.fixture.snapshot()));
          check(`${layout}: real transport reaches target while paused`, Math.abs((await page.evaluate(()=>window.fixture.snapshot())).actual-28000)<200, await page.evaluate(()=>window.fixture.snapshot()));
          await input.focus();
          await page.keyboard.down("ArrowLeft"); await page.keyboard.down("ArrowLeft");
          check(`${layout}: held keyboard input previews without seeking`, await page.evaluate(() => window.fixture.calls.length)===before+1);
          await page.keyboard.up("ArrowLeft");
          await page.waitForFunction(() => Math.abs(window.fixture.status.position-18000)<150);
          check(`${layout}: keyboard releases one seek`, await page.evaluate(() => window.fixture.calls.length)===before+2);
          await page.keyboard.press("Home");
          await page.waitForFunction(()=>window.fixture.status.position<100);
          await page.waitForFunction(()=>window.fixture.snapshot().actual<100);
          check(`${layout}: keyboard can return to start`, await page.evaluate(()=>window.fixture.snapshot().actual)<200);

          // A gesture cancelled by the browser must never become a seek on the
          // following pointerup; track replacement similarly invalidates it.
          const cancelBefore = await page.evaluate(()=>window.fixture.calls.length);
          await input.evaluate(element=>element.addEventListener('pointerdown',event=>{element.dataset.testPointer=event.pointerId},{once:true}));
          await page.mouse.move(at(.3).x,at(.3).y); await page.mouse.down();
          await input.evaluate(element=>element.dispatchEvent(new PointerEvent('pointercancel',{pointerId:Number(element.dataset.testPointer),bubbles:true})));
          await page.mouse.up();
          check(`${layout}: cancelled gesture does not seek`, await page.evaluate(()=>window.fixture.calls.length)===cancelBefore);
          await page.mouse.move(at(.3).x,at(.3).y); await page.mouse.down();
          await page.evaluate(()=>{window.fixture.media.track={...window.fixture.track,id:'test--changed--high'}});
          await page.mouse.move(at(.8).x,at(.8).y); await page.mouse.up();
          check(`${layout}: track switch invalidates held gesture`, await page.evaluate(()=>window.fixture.calls.length)===cancelBefore);
          await page.evaluate(()=>{window.fixture.media.track=window.fixture.track;window.fixture.status.trackLoading=true});
          check(`${layout}: loading with known duration remains seekable`, await input.isEnabled());
          await page.evaluate(()=>{window.fixture.status.trackLoading=false});

          // Release outside the input must still commit the clamped endpoint.
          await page.mouse.move(at(.4).x,at(.4).y); await page.mouse.down();
          await page.mouse.move(Math.max(1,box.x-40),box.y-70); await page.mouse.up();
          check(`${layout}: pointer capture commits outside release once`, await page.evaluate(()=>window.fixture.calls.length)===cancelBefore+1);
          await page.waitForTimeout(60);
          if (engine === "chromium") {
            const cdp = await context.newCDPSession(page);
            const touchBefore = await page.evaluate(()=>window.fixture.calls.length);
            await cdp.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{x:at(.2).x,y:at(.2).y}]});
            await cdp.send('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:at(.6).x,y:at(.6).y}]});
            await cdp.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});
            await page.waitForFunction(()=>Math.abs(window.fixture.status.position-24000)<150);
            check(`${layout}: real touch drag commits one seek`,await page.evaluate(()=>window.fixture.calls.length)===touchBefore+1);
            await cdp.detach();
          } else {
            await page.touchscreen.tap(at(.6).x,at(.6).y);
            await page.waitForFunction(()=>Math.abs(window.fixture.status.position-24000)<150);
            check(`${layout}: touch tap reaches desired position`, true);
          }
        }
        check("no uncaught browser errors",errors.length===0,errors);
      } catch (error) { check("scenario completes",false,{message:error.message,snapshot:await page.evaluate(()=>window.fixture?.snapshot()).catch(()=>null),errors}); }
      finally { await context.close(); }
      reports.push({engine,transport,passed:checks.filter(c=>c.passed).length,failed:checks.filter(c=>!c.passed).length,checks});
    }
    await browser.close(); browser=null;
  }
} finally { await browser?.close(); await server?.close(); await fs.rm(directory,{recursive:true,force:true}); }
console.log(JSON.stringify(reports,null,2));
assert.ok(reports.every(report=>report.failed===0),"seek UI browser regression failed");
