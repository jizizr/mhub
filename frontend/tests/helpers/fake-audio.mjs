// A small Web Audio stand-in for scheduled buffer playback. Start frames use
// Chromium's rounding (TimeToSampleFrame with 1/1024-frame oversampling), so
// tests catch gaps or overlaps that real playback would produce.

export function chromiumFrame(time, sampleRate) {
  return Math.ceil(Math.round(time * sampleRate * 1024) / 1024);
}

export class FakeParam {
  // `value` mirrors the latest requested value (what UI code reads back);
  // valueAt() evaluates the automation timeline from the initial value.
  constructor(value = 1) { this.initial = this.value = value; this.events = []; }
  setValueAtTime(value, time) { this.value = value; this.events.push({ type: "set", value, time }); return this; }
  linearRampToValueAtTime(value, time) { this.value = value; this.events.push({ type: "ramp", value, time }); return this; }
  setTargetAtTime(value, time) { this.value = value; this.events.push({ type: "target", value, time }); return this; }
  cancelScheduledValues(time) { this.events = this.events.filter((event) => event.time < time); return this; }
  cancelAndHoldAtTime(time) {
    const value = this.valueAt(time);
    this.events = this.events.filter((event) => event.time < time);
    this.events.push({ type: "set", value, time });
    return this;
  }
  /** Automation value at `time` (set and linear ramp events only). */
  valueAt(time) {
    let value = this.initial;
    let previous = { time: 0, value };
    for (const event of [...this.events].sort((a, b) => a.time - b.time)) {
      if (event.time > time) {
        if (event.type === "ramp") return previous.value + (event.value - previous.value) * ((time - previous.time) / (event.time - previous.time));
        break;
      }
      value = event.value;
      previous = event;
    }
    return value;
  }
}

class FakeGain {
  constructor() { this.gain = new FakeParam(1); this.outputs = new Set(); }
  connect(node) { this.outputs.add(node); return node; }
  disconnect() { this.outputs.clear(); }
}

class FakeBuffer {
  constructor(channels, length, sampleRate) {
    this.numberOfChannels = channels;
    this.length = length;
    this.sampleRate = sampleRate;
    this.duration = length / sampleRate;
    this.channels = Array.from({ length: channels }, () => new Float32Array(length));
  }
  getChannelData(channel) { return this.channels[channel]; }
}

class FakeSource {
  constructor(context) { this.context = context; this.buffer = null; this.onended = null; this.stopTime = Infinity; this.ended = false; }
  connect(node) { this.output = node; return node; }
  disconnect() { this.output = null; }
  start(when = 0, offset = 0) {
    if (this.started) throw new Error("start() called twice");
    this.started = true;
    this.when = Math.max(when, this.context.currentTime);
    this.startFrame = chromiumFrame(this.when, this.context.sampleRate);
    this.offsetFrames = Math.round(offset * this.buffer.sampleRate);
    this.context.sources.push(this);
  }
  stop(when = this.context.currentTime) { this.stopTime = Math.max(when, this.context.currentTime); }
  get endFrame() {
    const natural = this.startFrame + this.buffer.length - this.offsetFrames;
    return Math.min(natural, chromiumFrame(this.stopTime === Infinity ? Infinity : this.stopTime, this.context.sampleRate));
  }
}

export class FakeContext {
  constructor({ sampleRate = 48000, baseLatency = 0.01, latencyHint } = {}) {
    this.sampleRate = sampleRate;
    this.baseLatency = baseLatency;
    this.latencyHint = latencyHint;
    this.currentTime = 0;
    this.state = "running";
    this.destination = new FakeGain();
    this.sources = [];
    FakeContext.instances.push(this);
  }
  createGain() { return new FakeGain(); }
  createBuffer(channels, length, sampleRate) { return new FakeBuffer(channels, length, sampleRate); }
  createBufferSource() { return new FakeSource(this); }
  async resume() { this.state = "running"; }
  async suspend() { this.state = "suspended"; }
  async close() { this.closed = true; this.state = "closed"; }

  /** Advance the render clock and fire `ended` for sources that finished, in order. */
  advance(seconds) {
    this.currentTime += seconds;
    const now = chromiumFrame(this.currentTime, this.sampleRate);
    for (;;) {
      const due = this.sources.filter((source) => !source.ended && source.endFrame <= now)
        .sort((a, b) => a.endFrame - b.endFrame)[0];
      if (!due) break;
      due.ended = true;
      due.onended?.();
    }
  }

  /** Live (not stopped before starting) sources overlapping a context frame. */
  sourcesAt(frame) {
    return this.sources.filter((source) => source.startFrame <= frame && frame < source.endFrame);
  }

  /** Channel-0 sample a listener hears at a context frame (gain ignored). */
  sampleAt(frame, channel = 0) {
    let sum = 0;
    for (const source of this.sourcesAt(frame)) {
      sum += source.buffer.getChannelData(channel)[source.offsetFrames + frame - source.startFrame];
    }
    return sum;
  }
}
FakeContext.instances = [];
