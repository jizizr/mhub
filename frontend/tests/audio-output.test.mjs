import test from "node:test";
import assert from "node:assert/strict";

let instance = 0;
class Parameter {
  value = 1;
  events = [];
  cancelScheduledValues(at) { this.events.push(["cancel", at]); }
  setValueAtTime(value, at) { this.value = value; this.events.push(["set", value, at]); }
  setTargetAtTime(value, at, timeConstant) { this.value = value; this.events.push(["target", value, at, timeConstant]); }
  linearRampToValueAtTime(value, at) { this.events.push(["linear", value, at]); }
}
class Node {
  gain = new Parameter();
  connections = new Set();
  connect(node) { this.connections.add(node); return node; }
  disconnect(node) { if (node) this.connections.delete(node); else this.connections.clear(); }
}

async function harness({ rejectRate = false, resumeFails = () => false } = {}) {
  const created = [];
  class Context {
    constructor(options = {}) {
      if (rejectRate && options.sampleRate) throw new Error("Unsupported sample rate");
      this.sampleRate = options.sampleRate || 44100;
      this.currentTime = 0;
      this.state = "suspended";
      this.destination = {};
      this.sources = [];
      this.resumes = 0;
      this.closes = 0;
      created.push(this);
    }
    resume() { this.resumes += 1; if (!resumeFails(this)) this.state = "running"; return Promise.resolve(); }
    suspend() { this.suspends = (this.suspends || 0) + 1; this.state = "suspended"; return Promise.resolve(); }
    close() { this.closes += 1; this.state = "closed"; return Promise.resolve(); }
    createGain() { return new Node(); }
    createMediaElementSource(element) { const source = new Node(); this.sources.push({ element, source }); return source; }
  }
  globalThis.window = { AudioContext: Context };
  const api = await import(`../src/core/player/audio-output.js?case=${++instance}`);
  return { ...api, created };
}

test("48k and 192k shared contexts preserve both active decks and retain gesture authorization", async () => {
  const h = await harness();
  const low = h.prepareOutput("high");
  const first = h.createAlacOutput();
  const second = h.createAlacOutput();
  assert.equal(first.context, low);
  assert.equal(second.context, low);
  const high = h.prepareOutput("hires");
  assert.notEqual(high, low);
  assert.equal(low.sampleRate, 48000);
  assert.equal(high.sampleRate, 192000);
  assert.equal(h.prepareOutput(192000), high);
  assert.equal(h.prepareOutput("lossless"), low);
  first.dispose(); second.dispose();
  assert.equal(h.created.length, 2);
  assert.equal(low.closes + high.closes, 0);
  assert.equal(low.state, "running");
});

test("unsupported requested sample rate uses the browser's context fallback", async () => {
  const h = await harness({ rejectRate: true });
  const context = h.prepareOutput("hires");
  assert.equal(context.sampleRate, 44100);
  assert.equal(h.prepareOutput("hires"), context);
  assert.equal(h.created.length, 1);
});

test("each deck routes normalization, transition and volume in order without affecting other gains", async () => {
  const h = await harness();
  const context = h.prepareOutput("lossless");
  const output = h.createAlacOutput();
  assert.deepEqual([...output.input.connections], [output.fadeGain]);
  assert.deepEqual([...output.fadeGain.connections], [output.volumeGain]);
  assert.deepEqual([...output.volumeGain.connections], [context.destination]);
  output.setVolume(0.3);
  output.setLoudnessGain(0.5);
  output.cancelFade(0);
  output.fadeTo(1, 4);
  context.currentTime = 2;
  output.setVolume(0.8);
  assert.equal(output.getFadeLevel(), 0.5);
  assert.equal(output.normalizationGain.gain.value, 0.5);
  assert.equal(output.volumeGain.gain.value, 0.8);
  output.setLoudnessGain(1);
  output.cancelFade(1);
  assert.equal(output.volumeGain.gain.value, 0.8, "disabling effects preserves the user volume");
});

test("complementary linear fades sum to unity across the entire scheduled transition", async () => {
  const h = await harness();
  h.prepareOutput();
  const outgoing = h.createAlacOutput();
  const incoming = h.createAlacOutput();
  incoming.cancelFade(0);
  outgoing.fadeTo(0, 4, { at: 10 });
  incoming.fadeTo(1, 4, { at: 10 });
  for (let step = 0; step <= 600; step += 1) {
    const at = 9 + step / 100;
    assert.ok(Math.abs(outgoing.getFadeLevel(at) + incoming.getFadeLevel(at) - 1) < 1e-12);
  }
  assert.deepEqual(outgoing.fadeGain.gain.events.at(-1), ["linear", 0, 14]);
  assert.deepEqual(incoming.fadeGain.gain.events.at(-1), ["linear", 1, 14]);
});

test("cancelling or replacing a fade holds its interpolated value and never jumps to the old target", async () => {
  const h = await harness();
  const context = h.prepareOutput();
  const output = h.createAlacOutput();
  output.fadeTo(0, 8);
  context.currentTime = 2;
  assert.equal(output.cancelFade(), 0.75);
  assert.equal(output.getFadeLevel(20), 0.75);
  assert.deepEqual(output.fadeGain.gain.events.slice(-2), [["cancel", 2], ["set", 0.75, 2]]);
  output.fadeTo(1, 2);
  context.currentTime = 3;
  assert.equal(output.getFadeLevel(), 0.875);
  output.fadeTo(0, 2);
  assert.equal(output.getFadeLevel(), 0.875);
  assert.equal(output.getFadeLevel(4), 0.4375);
  output.cancelFade(1);
  assert.equal(output.getFadeLevel(20), 1);
});

test("native element reuses its one MediaElementSource across output recreation and rate changes", async () => {
  const h = await harness();
  const originalContext = h.prepareOutput("high");
  const audio = { volume: 0.2 };
  const first = h.createNativeOutput(audio);
  assert.equal(audio.volume, 1);
  assert.equal(first.volumeGain.gain.value, 0.2, "connecting an active native element preserves its existing level");
  assert.equal(h.createNativeOutput(audio), first);
  first.setVolume(0.4);
  assert.equal(first.volumeGain.gain.value, 0.4);
  assert.equal(originalContext.sources.length, 1);
  const source = originalContext.sources[0].source;
  assert.deepEqual([...source.connections], [first.input]);
  first.dispose(); first.dispose();
  assert.equal(source.connections.size, 0);
  h.prepareOutput("hires");
  const replacement = h.createNativeOutput(audio);
  assert.equal(replacement.context, originalContext);
  assert.equal(originalContext.sources.length, 1);
  assert.deepEqual([...source.connections], [replacement.input]);
  assert.equal(h.created[1].sources.length, 0);
});

test("disposing a deck removes all graph edges and makes pending controls inert without closing the context", async () => {
  const h = await harness();
  const context = h.prepareOutput();
  const output = h.createAlacOutput();
  output.fadeTo(0, 5);
  output.dispose();
  const events = output.fadeGain.gain.events.length;
  output.fadeTo(1, 4); output.setVolume(1); output.setLoudnessGain(1); output.dispose();
  assert.equal(output.fadeGain.gain.events.length, events);
  assert.equal(output.normalizationGain.connections.size + output.fadeGain.connections.size + output.volumeGain.connections.size, 0);
  assert.equal(context.closes, 0);
});

test("playback Audio Session is selected before resuming music and not rewritten repeatedly", async () => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
  let category = 'auto', writes = 0;
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { audioSession: {
    get type() { return category; }, set type(value) { category = value; writes++; }
  } } });
  try {
    const h = await harness();
    h.preparePlaybackSession();
    assert.equal(category, 'playback'); assert.equal(h.created.length, 0, 'native playback registration needs no extra decoder/context');
    const context = h.prepareOutput('hires'); h.prepareOutput('hires');
    assert.equal(writes, 1); assert.equal(context.resumes, 2); assert.equal(context.sampleRate, 192000);
  } finally {
    if (descriptor) Object.defineProperty(globalThis, 'navigator', descriptor); else delete globalThis.navigator;
  }
});

test("restricted Audio Session access cannot prevent the existing audio graph from playing", async () => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { get audioSession() { throw new Error('Unavailable in embed'); } } });
  try { const h = await harness(); assert.doesNotThrow(() => h.preparePlaybackSession()); assert.equal(h.prepareOutput().state, 'running'); }
  finally { if (descriptor) Object.defineProperty(globalThis, 'navigator', descriptor); else delete globalThis.navigator; }
});

test("Hi-Res renders 44.1/48 kHz songs at 48 kHz and real Hi-Res at 192 kHz", async () => {
  const h = await harness();
  assert.deepEqual([44100, 48000, 88200, 96000, 192000].map(h.outputRateFor), [48000, 48000, 192000, 192000, 192000]);
  const high = h.prepareOutput("hires");
  const low = h.created.find((context) => context.sampleRate === 48000);
  assert.equal(high.sampleRate, 192000);
  assert.equal(low?.state, "running", "the play gesture unlocks both rates");
  assert.equal(await h.acquireOutputContext(44100, [high]), low);
  assert.equal(await h.acquireOutputContext(96000, [high]), high);
});

test("the idle rate is suspended, but never one a deck or native element still uses", async () => {
  const h = await harness();
  const high = h.prepareOutput("hires");
  const low = h.created.find((context) => context.sampleRate === 48000);
  const deck = h.createAlacOutput(high);
  h.suspendIdleOutputs(low);
  assert.equal(high.state, "running", "a live deck keeps its context");
  deck.dispose();
  h.suspendIdleOutputs(low);
  assert.equal(high.state, "suspended", "an idle 192 kHz clock is stopped");
  assert.equal(low.state, "running");
  // A suspended rate is resumed when a song needs it again.
  assert.equal(await h.acquireOutputContext(96000, [low]), high);
  assert.equal(high.state, "running");
  h.createNativeOutput({ id: "audio" });
  h.suspendIdleOutputs(low);
  assert.equal(high.state, "running", "native <audio> graphs are never suspended");
});

test("a context that cannot resume outside a gesture falls back to a running one", async () => {
  const h = await harness({ resumeFails: (context) => context.sampleRate === 48000 && context.resumes > 1 });
  const high = h.prepareOutput("hires");
  const low = h.created.find((context) => context.sampleRate === 48000);
  h.suspendIdleOutputs(high);
  assert.equal(low.state, "suspended");
  assert.equal(await h.acquireOutputContext(44100, [high]), high, "playback continues at 192 kHz instead of stalling");
});
