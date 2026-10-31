import nodeTest from "node:test";
import assert from "node:assert/strict";
import { FakeContext, chromiumFrame } from "./helpers/fake-audio.mjs";
import { PcmScheduler } from "../src/core/player/pcm-scheduler.js";

// PCM whose left sample is its own frame number (exact in Float32 below 2^24)
// and right sample its negation, so every rendered frame proves which source
// frame it came from.
function pcm(start, frames, channels = 2) {
  const data = new Float32Array(frames * channels);
  for (let frame = 0; frame < frames; frame += 1) {
    data[frame * channels] = start + frame;
    if (channels === 2) data[frame * channels + 1] = -(start + frame);
  }
  return data;
}

// Every scheduler made by a test is disposed afterwards so its pump timer
// cannot keep the process alive.
const live = new Set();
function test(name, body) {
  nodeTest(name, async (t) => {
    try { await body(t); } finally { for (const scheduler of live) scheduler.dispose(); live.clear(); }
  });
}

function setup({ sampleRate = 48000, startupFrames = 4800, rebufferFrames = 9600, channels = 2 } = {}) {
  const context = new FakeContext({ sampleRate, baseLatency: 0.01 });
  const events = { states: [], times: [], ended: 0, need: 0 };
  const scheduler = new PcmScheduler(context, context.destination, {
    channels, startupFrames, rebufferFrames,
    onState: (state) => events.states.push(state),
    onTime: (frame) => events.times.push(frame),
    onEnded: () => { events.ended += 1; },
    onNeedData: () => { events.need += 1; },
  });
  live.add(scheduler);
  let next = 0;
  const feed = (frames) => { scheduler.push(pcm(next, frames, channels)); next += frames; scheduler.commit(); };
  return { context, scheduler, events, feed, fed: () => next };
}

// Every context frame from the anchor until `frames` later must carry the
// next source frame exactly once: no gap, overlap, repeat or interpolation.
function assertContinuous(context, fromContextFrame, firstSourceFrame, frames) {
  for (let offset = 0; offset < frames; offset += 1) {
    const live = context.sourcesAt(fromContextFrame + offset);
    assert.equal(live.length, 1, `exactly one source at +${offset}`);
    assert.equal(context.sampleAt(fromContextFrame + offset), firstSourceFrame + offset, `frame at +${offset}`);
  }
}

test("blocks join on exact consecutive frames with no interpolation", () => {
  const { context, scheduler, feed } = setup();
  // Odd sizes make every boundary a non-round time.
  for (const frames of [4801, 997, 1499, 7]) feed(frames);
  context.currentTime = 12.3456789;
  scheduler.play();
  const anchor = context.sources[0].startFrame;
  assert.equal(context.sources.length, 4);
  for (let index = 1; index < context.sources.length; index += 1) {
    const previous = context.sources[index - 1];
    assert.equal(context.sources[index].startFrame, previous.startFrame + previous.buffer.length - previous.offsetFrames);
    // Start times are integer frames divided once by the rate.
    assert.ok(Math.abs(context.sources[index].when * 48000 - context.sources[index].startFrame) < 1e-6);
  }
  assertContinuous(context, anchor, 0, 4801 + 997 + 1499 + 7);
});

test("playback waits for the startup cushion, then reports playing only once audio renders", () => {
  const { context, scheduler, events, feed } = setup();
  feed(1000);
  scheduler.play();
  assert.equal(context.sources.length, 0);
  assert.equal(scheduler.state, "buffering");
  feed(4000);
  assert.equal(context.sources.length, 2);
  assert.notEqual(events.states.at(-1), "playing", "scheduled is not yet audible");
  context.advance(0.05); scheduler.check();
  assert.equal(events.states.at(-1), "playing");
  assert.ok(scheduler.position > 0);
});

test("pause freezes the exact frame, ramps out and resumes from it with a fade-in", () => {
  const { context, scheduler, feed } = setup();
  feed(48000);
  scheduler.play();
  const anchor = context.sources[0].startFrame;
  context.advance(0.5);
  const expected = chromiumFrame(context.currentTime, 48000) - anchor;
  scheduler.pause();
  assert.equal(scheduler.position, expected);
  const stopped = context.sources[0];
  assert.ok(stopped.stopTime > context.currentTime && stopped.stopTime <= context.currentTime + 0.004, "a 3 ms ramp, then stop");
  assert.equal(scheduler.gate.gain.valueAt(stopped.stopTime), 0);
  context.advance(2);
  assert.equal(scheduler.position, expected, "paused clock does not move");
  scheduler.play();
  const resumed = context.sources.at(-1);
  assert.equal(resumed.offsetFrames, expected, "resumes from the paused frame without re-decoding");
  assert.equal(scheduler.gate.gain.valueAt(resumed.when), 0);
  assert.equal(scheduler.gate.gain.valueAt(resumed.when + 0.003), 1);
  assertContinuous(context, resumed.startFrame, expected, 48000 - expected);
});

test("an underrun freezes the clock, waits for a cushion and continues without losing frames", () => {
  const { context, scheduler, events, feed } = setup();
  feed(4800);
  scheduler.play();
  context.advance(0.03 + 0.1); // anchor lead plus the whole block
  assert.equal(scheduler.state, "buffering");
  assert.equal(scheduler.position, 4800);
  assert.ok(events.need > 0);
  feed(4800);
  assert.equal(scheduler.running, false, "one block is below the two-block rebuffer cushion");
  context.advance(1);
  assert.equal(scheduler.position, 4800, "no fake progress while waiting");
  feed(4800);
  assert.equal(scheduler.running, true);
  const restart = context.sources.at(-2);
  assert.equal(restart.offsetFrames, 0);
  assertContinuous(context, restart.startFrame, 4800, 9600);
  assert.equal(scheduler.gate.gain.valueAt(restart.when), 0, "recovery fades in");
});

test("a block committed after its slot restarts behind a cushion instead of overlapping", () => {
  const { context, scheduler, feed } = setup({ rebufferFrames: 4800 });
  feed(4800);
  scheduler.play();
  const first = context.sources[0];
  // The next block arrives 1 ms before the first one ends: too late to join.
  context.currentTime = first.when + 0.099;
  feed(4800);
  assert.ok(first.stopTime < Infinity, "the old tail is ramped out and stopped");
  // Like a pause, recovery continues from the playhead; the 3 ms ramp-out
  // region is heard again at full level rather than skipped.
  const played = chromiumFrame(context.currentTime, 48000) - first.startFrame;
  assert.equal(scheduler.frame, played);
  const restarted = context.sources.slice(1);
  assert.ok(restarted.length > 0);
  assert.ok(restarted[0].startFrame >= first.endFrame, "never overlaps the stopped tail");
  assertContinuous(context, restarted[0].startFrame, played, 9600 - played);
});

test("a short EOF tail plays below the cushion and ends exactly once", () => {
  const { context, scheduler, events, feed } = setup();
  feed(4800);
  scheduler.play();
  context.advance(0.2);
  assert.equal(scheduler.state, "buffering");
  feed(100);
  scheduler.finish();
  assert.equal(scheduler.running, true, "EOF bypasses the rebuffer cushion");
  context.advance(0.2);
  assert.equal(events.ended, 1);
  assert.equal(scheduler.position, 4900);
  assert.equal(scheduler.ended, true);
  context.advance(1); scheduler.check();
  assert.equal(events.ended, 1);
});

test("a looming underrun ramps out at the scheduled end; arriving PCM cancels the ramp", () => {
  const { context, scheduler, feed } = setup();
  feed(9600);
  scheduler.play();
  const first = context.sources[0];
  context.advance(0.03 + 0.1);
  scheduler.check();
  const end = first.when + 0.2;
  assert.equal(scheduler.fadingOut, true);
  assert.equal(scheduler.gate.gain.valueAt(end), 0);
  assert.equal(scheduler.gate.gain.valueAt(end - 0.01), 1);
  feed(9600);
  assert.equal(scheduler.fadingOut, false);
  assert.equal(scheduler.gate.gain.valueAt(end), 1, "the queued ramp is cancelled");
  assertContinuous(context, first.startFrame + 4800, 4800, 14400);
});

test("reset and dispose stop every source and release PCM", () => {
  const { context, scheduler, feed } = setup();
  feed(48000); feed(48000);
  scheduler.play();
  scheduler.reset(96000);
  assert.ok(context.sources.every((source) => source.stopTime <= context.currentTime));
  assert.equal(scheduler.blocks.length, 0);
  assert.equal(scheduler.queuedFrames, 0);
  assert.equal(scheduler.position, 96000);
  scheduler.dispose();
  assert.equal(scheduler.gate.outputs.size, 0);
});

test("mono PCM fills one channel and fully played blocks are released", () => {
  const { context, scheduler, feed } = setup({ channels: 1 });
  feed(4800); feed(4800);
  scheduler.play();
  assert.equal(context.sources[0].buffer.numberOfChannels, 1);
  assertContinuous(context, context.sources[0].startFrame, 0, 9600);
  context.advance(0.03 + 0.11);
  assert.equal(scheduler.blocks.length, 1, "the first block's PCM is released after it ends");
});

test("sustained half-rate delivery cycles through cushioned recoveries without fake progress", () => {
  const sampleRate = 192000;
  const { context, scheduler, feed } = setup({ sampleRate, startupFrames: sampleRate / 2, rebufferFrames: sampleRate * 2 });
  feed(sampleRate / 2);
  scheduler.play();
  let recoveries = 0;
  let wasRunning = scheduler.running;
  let frozen = 0;
  for (let interval = 0; interval < 120; interval += 1) {
    const before = scheduler.position;
    const waiting = !scheduler.running;
    context.advance(0.25); scheduler.check();
    if (waiting && !scheduler.running) { assert.equal(scheduler.position, before); frozen += 1; }
    feed(sampleRate / 8);
    if (!wasRunning && scheduler.running) recoveries += 1;
    wasRunning = scheduler.running;
  }
  assert.ok(recoveries >= 3);
  assert.ok(frozen >= 30);
});
