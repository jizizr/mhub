/*
 * AudioWorklet renderer for PCM produced by the WASM ALAC decoder.
 *
 * The decoder deliberately stays out of the worklet. The main thread (or a
 * future decoder worker) decodes small chunks and transfers either interleaved
 * Float32 PCM or little-endian sample bytes here. Audio rendering then remains
 * independent of network/WASM scheduling and does not stall the UI thread.
 */
class AlacPcmProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.channels = 2;
    this.bits = 16;
    this.bytesPerChannel = 2;
    this.queue = [];
    this.pendingFrames = 0;
    this.playing = false;
    this.eof = false;
    this.position = 0;
    this.lastReport = 0;
    this.ended = false;
    this.epoch = 0;
    this.consumedFrames = 0;
    this.renderState = "paused";
    this.needDataSent = false;
    this.needsBuffer = true;
    this.resumeFrames = 24000;
    this.rebufferFrames = 96000;
    this.bufferTargetFrames = this.resumeFrames;
    this.lowWaterFrames = 24000;
    this.reportFrames = 4800;
    this.disposed = false;
    this.port.onmessage = (event) => this.handleMessage(event.data || {});
  }

  handleMessage(message) {
    if (this.disposed) return;
    if (message.type !== "configure" && message.epoch !== this.epoch) return;
    switch (message.type) {
      case "dispose":
        this.disposed = true;
        this.playing = false;
        this.queue = [];
        this.pendingFrames = 0;
        this.port.onmessage = null;
        this.port.close();
        break;
      case "configure":
        this.epoch = message.epoch;
        this.channels = Math.max(1, Math.min(2, Number(message.channels) || 2));
        this.bits = Number(message.bits) === 24 ? 24 : 16;
        this.bytesPerChannel = this.bits === 24 ? 3 : 2;
        this.queue = [];
        this.pendingFrames = 0;
        this.position = Number(message.position) || 0;
        this.lastReport = this.position;
        this.consumedFrames = 0;
        this.playing = false;
        this.renderState = "paused";
        this.needDataSent = false;
        this.needsBuffer = true;
        this.resumeFrames = Math.max(0, Number(message.resumeFrames) || 0);
        this.rebufferFrames = Math.max(this.resumeFrames, Number(message.rebufferFrames) || this.resumeFrames);
        this.bufferTargetFrames = this.resumeFrames;
        this.lowWaterFrames = Math.max(1, Number(message.lowWaterFrames) || 8192);
        this.reportFrames = Math.max(1, Number(message.reportFrames) || 2048);
        this.eof = false;
        this.ended = false;
        break;
      case "data": {
        if (!(message.data instanceof ArrayBuffer) || !message.frames) break;
        const floats = message.format === "f32" ? new Float32Array(message.data) : null;
        const bytes = floats ? null : new Uint8Array(message.data);
        const frames = Math.min(
          Number(message.frames) || 0,
          floats
            ? Math.floor(floats.length / this.channels)
            : Math.floor(bytes.byteLength / (this.channels * this.bytesPerChannel)),
        );
        if (frames <= 0) break;
        this.queue.push({ bytes, floats, frames, offset: 0 });
        this.pendingFrames += frames;
        this.needDataSent = false;
        this.report("queued");
        break;
      }
      case "eof":
        this.eof = true;
        break;
      case "play":
        this.playing = true;
        this.ended = false;
        break;
      case "pause":
        this.playing = false;
        this.renderState = "paused";
        this.report("position");
        break;
      case "flush":
        this.queue = [];
        this.pendingFrames = 0;
        this.eof = false;
        this.ended = false;
        this.position = Number(message.position) || 0;
        this.lastReport = this.position;
        this.consumedFrames = 0;
        this.needsBuffer = true;
        this.bufferTargetFrames = this.resumeFrames;
        break;
      case "volume":
        // Gain is handled by the main-thread GainNode. Keep this message for
        // forwards compatibility with callers that send volume to the node.
        break;
      default:
        break;
    }
  }

  report(type, extra = {}) {
    this.port.postMessage({ type, epoch: this.epoch, position: this.position, consumedFrames: this.consumedFrames, pendingFrames: this.pendingFrames, ...extra });
  }

  reportState(state) {
    if (state === this.renderState) return;
    this.renderState = state;
    this.report("position");
    this.report("state", { state });
  }

  readSample(frame, channel) {
    const item = this.queue[0];
    if (!item || item.offset >= item.frames) return 0;
    if (item.floats) return item.floats[item.offset * this.channels + channel] || 0;
    const byteOffset = (item.offset * this.channels + channel) * this.bytesPerChannel;
    const b = item.bytes;
    if (this.bits === 24) {
      let value = b[byteOffset] | (b[byteOffset + 1] << 8) | (b[byteOffset + 2] << 16);
      if (value & 0x800000) value |= 0xff000000;
      return value / 8388608;
    }
    const value = (b[byteOffset] | (b[byteOffset + 1] << 8));
    return (value & 0x8000 ? value - 0x10000 : value) / 32768;
  }

  consumeFrame() {
    const item = this.queue[0];
    if (!item) return;
    item.offset += 1;
    this.pendingFrames = Math.max(0, this.pendingFrames - 1);
    if (item.offset >= item.frames) this.queue.shift();
    this.position += 1;
    this.consumedFrames += 1;
  }

  process(_inputs, outputs) {
    if (this.disposed) return false;
    const output = outputs[0];
    if (!output || output.length === 0) return true;
    const frameCount = output[0].length;
    for (const channel of output) channel.fill(0);

    if (this.playing) {
      const previousPosition = this.position;
      // After an underrun, accumulate a useful cushion before resuming. One
      // packet at a time otherwise alternates tiny sound bursts and silence.
      if (this.needsBuffer && (this.pendingFrames >= this.bufferTargetFrames || this.eof)) this.needsBuffer = false;
      for (let frame = 0; !this.needsBuffer && frame < frameCount; frame += 1) {
        if (!this.queue.length) break;
        for (let channel = 0; channel < output.length; channel += 1) {
          output[channel][frame] = this.readSample(frame, Math.min(channel, this.channels - 1));
        }
        this.consumeFrame();
      }
      if (!this.pendingFrames && !this.eof && !this.needsBuffer) {
        this.needsBuffer = true;
        this.bufferTargetFrames = this.rebufferFrames;
      }
      this.reportState(this.position > previousPosition ? "playing" : "buffering");
      if (this.pendingFrames < this.lowWaterFrames && !this.needDataSent) {
        this.needDataSent = true;
        this.report("need-data");
      }
      if (this.position - this.lastReport >= this.reportFrames) {
        this.lastReport = this.position;
        this.report("position");
      }
      if (this.eof && this.pendingFrames === 0 && !this.ended) {
        this.ended = true;
        this.playing = false;
        this.report("ended");
      }
    }
    return true;
  }
}

registerProcessor("splayer-alac-pcm", AlacPcmProcessor);
