/*
 * Plays decoded PCM as consecutive AudioBufferSourceNodes on one exact frame
 * timeline.
 *
 * An AudioWorklet makes Chromium render on a second thread: with the default
 * "bypass output buffering" (Edge/Chrome 156 on macOS) the device callback
 * posts each render to the worklet thread and waits for it. After some
 * suspend/resume cycles that wait returns early, so every other device buffer
 * is silent and music plays at half speed, an octave low, until the page
 * creates a new AudioContext. Buffer sources render on the device thread
 * itself, which has no such handshake, and cost no JavaScript per quantum.
 *
 * Start times are integer context frames divided once by the sample rate, so
 * Chromium maps every block back to the exact frame after its predecessor:
 * blocks join without gaps, overlaps or sub-sample interpolation.
 */

const DECLICK_SECONDS = 0.003;
const PUMP_MS = 100;
// When the next block is not there this close to the scheduled end, ramp out
// so a network stall ends in silence instead of a cut-off waveform.
const LOW_WATER_SECONDS = 0.25;

export class PcmScheduler {
  constructor(context, destination, { channels = 2, startupFrames = 0, rebufferFrames = 0,
    onState = () => {}, onTime = () => {}, onEnded = () => {}, onNeedData = () => {} } = {}) {
    this.context = context;
    this.sampleRate = context.sampleRate;
    this.channels = channels;
    this.startupFrames = startupFrames;
    this.rebufferFrames = Math.max(startupFrames, rebufferFrames);
    Object.assign(this, { onState, onTime, onEnded, onNeedData });
    // One gain shared by every block, only for the short pause/underrun ramps.
    this.gate = context.createGain();
    this.gate.connect(destination);
    // The render clock advances one device callback at a time. A new anchor
    // must lie beyond the callback being rendered or its first frames would
    // start late and overlap the next block.
    const callback = Number(context.baseLatency) > 0 ? Number(context.baseLatency) : 0.0107;
    this.startLead = Math.max(0.03, callback * 2);
    this.safeLead = Math.max(0.015, callback * 1.25);
    this.blocks = [];
    this.sources = [];
    this.pending = [];
    this.pendingFrames = 0;
    this.pumpTimer = 0;
    this.playingTimer = 0;
    this.disposed = false;
    this.reset(0);
  }

  get running() { return this.anchorTime !== null; }

  /** The output frame being rendered now (frozen while paused or starved). */
  get position() {
    if (!this.running) return this.frame;
    const elapsed = Math.floor((this.context.currentTime - this.anchorTime) * this.sampleRate + 1e-6);
    return Math.min(this.scheduledEnd, this.anchorFrame + Math.max(0, elapsed));
  }

  get queuedFrames() { return Math.max(0, this.endFrame + this.pendingFrames - this.position); }

  /** Drop all audio and continue from `frame` (a seek or a new track). */
  reset(frame) {
    this.silenceSources(false);
    this.blocks = [];
    this.pending = [];
    this.pendingFrames = 0;
    this.frame = this.endFrame = this.scheduledEnd = this.reportedFrame = Math.max(0, Math.round(frame) || 0);
    this.anchorTime = this.anchorFrame = null;
    this.playing = this.finished = this.ended = this.fadingOut = false;
    this.waitFrames = this.startupFrames;
    // A track's first frame stays exact; any other start begins mid-waveform.
    this.fadeIn = this.frame > 0;
    this.state = "paused";
    this.stopPump();
  }

  /** Queue interleaved PCM at the context rate; commit() makes it playable. */
  push(pcm) {
    const frames = Math.floor(pcm.length / this.channels);
    if (!frames || this.finished) return;
    this.pending.push(frames * this.channels === pcm.length ? pcm : pcm.subarray(0, frames * this.channels));
    this.pendingFrames += frames;
  }

  /** Move queued PCM into one AudioBuffer and schedule it when playing. */
  commit() {
    if (!this.pendingFrames || this.disposed) return;
    const frames = this.pendingFrames;
    const buffer = this.context.createBuffer(this.channels, frames, this.sampleRate);
    if (this.channels === 1) {
      let offset = 0;
      const target = buffer.getChannelData(0);
      for (const chunk of this.pending) { target.set(chunk, offset); offset += chunk.length; }
    } else {
      const left = buffer.getChannelData(0);
      const right = buffer.getChannelData(1);
      let frame = 0;
      for (const chunk of this.pending) {
        for (let index = 0; index < chunk.length; index += 2, frame += 1) {
          left[frame] = chunk[index];
          right[frame] = chunk[index + 1];
        }
      }
    }
    this.pending = [];
    this.pendingFrames = 0;
    const block = { buffer, frame: this.endFrame, frames };
    this.blocks.push(block);
    this.endFrame += frames;
    if (!this.running) { this.maybeStart(); return; }
    const now = this.context.currentTime;
    if (this.timeOf(block.frame) < now + this.safeLead) {
      // The block arrived after its slot: restart from it after a cushion.
      this.starve();
      return;
    }
    if (this.fadingOut) {
      // The low-water ramp is still in the future; keep the music going.
      const from = Math.max(now, this.anchorTime + DECLICK_SECONDS);
      this.gate.gain.cancelScheduledValues(from);
      this.gate.gain.setValueAtTime(1, from);
      this.fadingOut = false;
    }
    this.schedule(block);
  }

  /** No more PCM follows: the remaining audio may play below the cushion. */
  finish() {
    this.commit();
    this.finished = true;
    if (this.running) this.check();
    else this.maybeStart();
  }

  play() {
    if (this.playing || this.disposed) return;
    this.playing = true;
    this.ended = false;
    this.state = "buffering";
    this.maybeStart();
    this.startPump();
  }

  pause() {
    if (!this.playing) return;
    this.frame = this.position;
    this.reportedFrame = this.frame;
    this.playing = false;
    if (this.running) {
      this.silenceSources(true);
      this.anchorTime = this.anchorFrame = null;
      this.fadeIn = true;
    }
    this.releasePlayed(this.frame);
    this.state = "paused";
    this.stopPump();
  }

  dispose() {
    if (this.disposed) return;
    this.reset(0);
    this.disposed = true;
    this.gate.disconnect();
  }

  timeOf(frame) { return (this.anchorContextFrame + frame - this.anchorFrame) / this.sampleRate; }

  maybeStart() {
    if (!this.playing || this.running || this.disposed) return;
    const available = this.endFrame - this.frame;
    if (available <= 0) {
      if (this.finished) this.end();
      else this.setState("buffering");
      return;
    }
    if (available < this.waitFrames && !this.finished) {
      this.setState("buffering");
      return;
    }
    // Anchor on an integer context frame; all later start times derive from
    // it with one division each.
    const contextFrame = Math.ceil((this.context.currentTime + this.startLead) * this.sampleRate);
    this.anchorContextFrame = contextFrame;
    this.anchorFrame = this.scheduledEnd = this.frame;
    this.anchorTime = contextFrame / this.sampleRate;
    const gain = this.gate.gain;
    gain.cancelScheduledValues(this.context.currentTime);
    if (this.fadeIn) {
      gain.setValueAtTime(0, this.anchorTime);
      gain.linearRampToValueAtTime(1, this.anchorTime + DECLICK_SECONDS);
    } else gain.setValueAtTime(1, this.anchorTime);
    this.fadeIn = false;
    this.fadingOut = false;
    for (const block of this.blocks) this.schedule(block);
    clearTimeout(this.playingTimer);
    const delay = Math.max(0, (this.anchorTime - this.context.currentTime) * 1000);
    this.playingTimer = setTimeout(() => this.check(), delay + 1);
  }

  schedule(block) {
    const skip = Math.max(0, this.anchorFrame - block.frame);
    if (skip >= block.frames) return;
    const source = this.context.createBufferSource();
    source.buffer = block.buffer;
    source.connect(this.gate);
    const entry = { source, block, stopped: false };
    source.onended = () => this.sourceEnded(entry);
    source.start(this.timeOf(block.frame + skip), skip / this.sampleRate);
    this.sources.push(entry);
    this.scheduledEnd = block.frame + block.frames;
  }

  sourceEnded(entry) {
    if (entry.stopped) return;
    entry.source.onended = null;
    this.sources = this.sources.filter((item) => item !== entry);
    if (entry.block === this.blocks[0]) this.blocks.shift();
    this.check();
  }

  /** Called by the pump and by every finished block (events are not throttled). */
  check() {
    if (!this.running || this.disposed) return;
    const now = this.context.currentTime;
    const endTime = this.timeOf(this.scheduledEnd);
    if (now >= this.anchorTime && now < endTime) this.setState("playing");
    if (now >= endTime - 1e-6 || (!this.sources.length && now >= this.anchorTime)) {
      if (this.finished && this.scheduledEnd === this.endFrame) this.end();
      else this.starve();
      return;
    }
    this.report();
    if (!this.finished && endTime - now < LOW_WATER_SECONDS) {
      if (!this.pendingFrames && !this.fadingOut && endTime - DECLICK_SECONDS > now) {
        const gain = this.gate.gain;
        gain.setValueAtTime(1, endTime - DECLICK_SECONDS);
        gain.linearRampToValueAtTime(0, endTime);
        this.fadingOut = true;
      }
    }
    this.onNeedData();
  }

  /** Ran out of PCM before the end: hold the clock and wait for a cushion. */
  starve() {
    this.frame = this.position;
    this.silenceSources(true);
    this.anchorTime = this.anchorFrame = null;
    this.releasePlayed(this.frame);
    this.fadeIn = true;
    this.waitFrames = this.rebufferFrames;
    this.report();
    this.setState("buffering");
    this.onNeedData();
    this.maybeStart();
  }

  end() {
    this.frame = this.reportedFrame = this.endFrame;
    this.silenceSources(false);
    this.anchorTime = this.anchorFrame = null;
    this.blocks = [];
    this.playing = false;
    this.ended = true;
    this.state = "paused";
    this.stopPump();
    this.onEnded();
  }

  silenceSources(ramp) {
    clearTimeout(this.playingTimer);
    const sources = this.sources;
    this.sources = [];
    if (!sources.length) return;
    const now = this.context.currentTime;
    let stopAt = now;
    if (ramp) {
      // Ramp from wherever the gain is (it may still be fading in).
      const gain = this.gate.gain;
      if (gain.cancelAndHoldAtTime) gain.cancelAndHoldAtTime(now);
      else { gain.cancelScheduledValues(now); gain.setValueAtTime(gain.value, now); }
      gain.linearRampToValueAtTime(0, now + DECLICK_SECONDS);
      stopAt = now + DECLICK_SECONDS;
    }
    for (const entry of sources) {
      entry.stopped = true;
      entry.source.onended = null;
      try { entry.source.stop(stopAt); } catch { /* already stopped */ }
    }
    this.fadingOut = false;
  }

  releasePlayed(frame) {
    while (this.blocks.length && this.blocks[0].frame + this.blocks[0].frames <= frame) this.blocks.shift();
  }

  report() {
    const frame = this.position;
    if (frame === this.reportedFrame) return;
    this.reportedFrame = frame;
    this.onTime(frame);
  }

  setState(state) {
    if (state === this.state) return;
    this.state = state;
    this.onState(state);
  }

  startPump() {
    if (this.pumpTimer) return;
    const tick = () => {
      this.pumpTimer = 0;
      if (!this.playing || this.disposed) return;
      if (this.running) this.check();
      else this.onNeedData();
      this.pumpTimer = setTimeout(tick, PUMP_MS);
    };
    this.pumpTimer = setTimeout(tick, PUMP_MS);
  }

  stopPump() {
    clearTimeout(this.pumpTimer);
    this.pumpTimer = 0;
  }
}
