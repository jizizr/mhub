// Incremental demuxer for a progressive (non-fragmented) ALAC M4A, the form
// Telegram keeps for Apple Music lossless songs. It yields the same packets
// as the SPLALAC1 transport, so completed songs need no server conversion:
// the browser reads the stored file directly and decodes it with WASM.
//
// The sample table lives in `moov`. Files written with `moov` first (all
// Apple Music remuxes) stream packet by packet; a file with `moov` after
// `mdat` is still accepted, but its packets only appear once `moov` arrives.

const STREAM_MAGIC = "SPLALAC1";
const STREAM_VERSION = 2;
const MAX_PACKET_BYTES = 16 * 1024 * 1024;
const MAX_SAMPLES = 1 << 24;

function readU32(bytes, offset) {
  return ((bytes[offset] * 0x1000000) + (bytes[offset + 1] << 16) + (bytes[offset + 2] << 8) + bytes[offset + 3]) >>> 0;
}

function readU64(bytes, offset) {
  const value = readU32(bytes, offset) * 0x100000000 + readU32(bytes, offset + 4);
  if (!Number.isSafeInteger(value)) throw new Error("m4a 偏移超出范围");
  return value;
}

function boxType(bytes, offset) {
  return String.fromCharCode(bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]);
}

/** Read the box header at `offset`; null when it is not yet buffered. */
function readBoxHeader(bytes, offset, limit) {
  if (limit - offset < 8) return null;
  let size = readU32(bytes, offset);
  const type = boxType(bytes, offset + 4);
  let header = 8;
  if (size === 1) {
    if (limit - offset < 16) return null;
    size = readU64(bytes, offset + 8);
    header = 16;
  }
  return { type, size, header };
}

function children(bytes, start, end) {
  const boxes = [];
  let offset = start;
  while (end - offset >= 8) {
    const box = readBoxHeader(bytes, offset, end);
    if (!box) break;
    const size = box.size === 0 ? end - offset : box.size;
    if (size < box.header || offset + size > end) throw new Error("m4a 结构损坏");
    boxes.push({ type: box.type, start: offset + box.header, end: offset + size });
    offset += size;
  }
  return boxes;
}

function child(bytes, box, type) {
  return box && children(bytes, box.start, box.end).find((item) => item.type === type);
}

function soundTrack(bytes, moov) {
  for (const trak of children(bytes, moov.start, moov.end).filter((box) => box.type === "trak")) {
    const mdia = child(bytes, trak, "mdia");
    const hdlr = child(bytes, mdia, "hdlr");
    if (hdlr && hdlr.end - hdlr.start >= 12 && boxType(bytes, hdlr.start + 8) === "soun") return mdia;
  }
  throw new Error("m4a 没有音频轨");
}

function timescale(bytes, mdhd) {
  if (!mdhd) return 0;
  const version = bytes[mdhd.start];
  const offset = mdhd.start + (version === 1 ? 20 : 12);
  return offset + 4 <= mdhd.end ? readU32(bytes, offset) : 0;
}

function alacCookie(bytes, stsd) {
  if (!stsd || stsd.end - stsd.start < 16) throw new Error("m4a 缺少 stsd");
  const entry = readBoxHeader(bytes, stsd.start + 8, stsd.end);
  if (!entry || entry.type !== "alac") throw new Error("m4a 不是 ALAC 编码");
  const entryStart = stsd.start + 8;
  const entryEnd = entryStart + entry.size;
  if (entryEnd > stsd.end) throw new Error("m4a sample entry 损坏");
  // Audio sample entry fields take 28 bytes before its child atoms.
  const config = children(bytes, entryStart + entry.header + 28, entryEnd).find((box) => box.type === "alac");
  // The config atom carries version/flags, then the 24-byte magic cookie.
  if (!config || config.end - config.start < 28) throw new Error("m4a 缺少 ALAC 配置");
  return bytes.slice(config.start + 4, config.start + 28);
}

function fullBoxEntries(bytes, box, entrySize, name, headerExtra = 0) {
  if (!box || box.end - box.start < 8 + headerExtra) throw new Error(`m4a 缺少 ${name}`);
  const count = readU32(bytes, box.start + 4 + headerExtra);
  const first = box.start + 8 + headerExtra;
  if (count > MAX_SAMPLES || first + count * entrySize > box.end) throw new Error(`m4a ${name} 损坏`);
  return { count, first };
}

/** Absolute file offset, size and duration of every sample, in order. */
function sampleTable(bytes, stbl) {
  const stsz = child(bytes, stbl, "stsz");
  if (!stsz || stsz.end - stsz.start < 12) throw new Error("m4a 缺少 stsz");
  const fixedSize = readU32(bytes, stsz.start + 4);
  const sampleCount = readU32(bytes, stsz.start + 8);
  if (!sampleCount || sampleCount > MAX_SAMPLES || (!fixedSize && stsz.start + 12 + sampleCount * 4 > stsz.end)) {
    throw new Error("m4a stsz 损坏");
  }
  const sizes = new Uint32Array(sampleCount);
  for (let index = 0; index < sampleCount; index += 1) {
    sizes[index] = fixedSize || readU32(bytes, stsz.start + 12 + index * 4);
  }

  const stco = child(bytes, stbl, "stco");
  const co64 = stco ? null : child(bytes, stbl, "co64");
  const chunkBox = stco || co64;
  const chunks = fullBoxEntries(bytes, chunkBox, co64 ? 8 : 4, "stco");
  const chunkOffset = (index) => co64 ? readU64(bytes, chunks.first + index * 8) : readU32(bytes, chunks.first + index * 4);

  const stsc = fullBoxEntries(bytes, child(bytes, stbl, "stsc"), 12, "stsc");
  const stts = fullBoxEntries(bytes, child(bytes, stbl, "stts"), 8, "stts");

  const offsets = new Float64Array(sampleCount);
  let sample = 0;
  for (let entry = 0; entry < stsc.count && sample < sampleCount; entry += 1) {
    const at = stsc.first + entry * 12;
    const firstChunk = readU32(bytes, at) - 1;
    const perChunk = readU32(bytes, at + 4);
    const lastChunk = entry + 1 < stsc.count ? readU32(bytes, at + 12) - 1 : chunks.count;
    if (firstChunk < 0 || lastChunk > chunks.count || lastChunk < firstChunk || !perChunk) throw new Error("m4a stsc 损坏");
    for (let chunk = firstChunk; chunk < lastChunk && sample < sampleCount; chunk += 1) {
      let offset = chunkOffset(chunk);
      for (let index = 0; index < perChunk && sample < sampleCount; index += 1) {
        offsets[sample] = offset;
        offset += sizes[sample];
        sample += 1;
      }
    }
  }
  if (sample !== sampleCount) throw new Error("m4a 样本索引不完整");

  const durations = new Uint32Array(sampleCount);
  sample = 0;
  for (let entry = 0; entry < stts.count; entry += 1) {
    const at = stts.first + entry * 8;
    const count = readU32(bytes, at);
    const delta = readU32(bytes, at + 4);
    if (!delta || count > sampleCount - sample) throw new Error("m4a 时间索引与样本数不一致");
    durations.fill(delta, sample, sample + count);
    sample += count;
  }
  if (sample !== sampleCount) throw new Error("m4a 时间索引与样本数不一致");

  for (let index = 0; index < sampleCount; index += 1) {
    if (!sizes[index] || sizes[index] > MAX_PACKET_BYTES) throw new Error("m4a 包含非法 packet 长度");
    // Packets are emitted as their bytes arrive, so they must be laid out
    // in playback order without overlapping.
    if (index && offsets[index] < offsets[index - 1] + sizes[index - 1]) throw new Error("m4a 样本顺序不受支持");
  }
  return { sizes, offsets, durations };
}

/** The SPLALAC1 v2 header for this track, so cached Blobs stay one format. */
function packetStreamHeader({ sampleRate, channels, bits, maxSamples, cookie }) {
  const header = new Uint8Array(24 + cookie.byteLength);
  const view = new DataView(header.buffer);
  header.set(new TextEncoder().encode(STREAM_MAGIC), 0);
  view.setUint32(8, STREAM_VERSION);
  view.setUint32(12, sampleRate);
  header[16] = channels;
  header[17] = bits;
  view.setUint16(18, cookie.byteLength);
  view.setUint32(20, maxSamples);
  header.set(cookie, 24);
  return header;
}

export function looksLikeMp4(bytes) {
  return bytes?.byteLength >= 8 && boxType(bytes, 4) === "ftyp";
}

export class Mp4AlacStreamParser {
  constructor() {
    this.buffer = new Uint8Array(0);
    // Absolute file offset of buffer[0].
    this.bufferStart = 0;
    // Absolute offset of the next top-level box while looking for moov.
    this.cursor = 0;
    // An mdat seen before moov must be retained until its index arrives.
    this.retainFrom = null;
    this.header = null;
    this.headerBytes = null;
    this.table = null;
    this.next = 0;
    this.totalFrames = 0;
  }

  get bufferEnd() { return this.bufferStart + this.buffer.byteLength; }

  append(chunk) {
    // Keep only bytes something still needs: the next box header before the
    // index is known, the next packet afterwards.
    let keep = this.bufferStart;
    if (this.table) keep = this.next < this.table.offsets.length ? this.table.offsets[this.next] : this.bufferEnd;
    else keep = this.retainFrom ?? this.cursor;
    keep = Math.min(Math.max(keep, this.bufferStart), this.bufferEnd);
    const rest = this.buffer.subarray(keep - this.bufferStart);
    const joined = new Uint8Array(rest.byteLength + chunk.byteLength);
    joined.set(rest, 0);
    joined.set(chunk, rest.byteLength);
    this.buffer = joined;
    this.bufferStart = keep;
  }

  readBoxes() {
    while (!this.table) {
      const at = this.cursor - this.bufferStart;
      if (at < 0) throw new Error("m4a 读取位置无效");
      const box = readBoxHeader(this.buffer, at, this.buffer.byteLength);
      if (!box) return;
      if (box.size === 0 && box.type !== "mdat") throw new Error("m4a 结构损坏");
      if (box.size !== 0 && box.size < box.header) throw new Error("m4a 结构损坏");
      if (box.type === "moov") {
        if (this.buffer.byteLength - at < box.size) return;
        this.parseMoov({ start: at + box.header, end: at + box.size });
        this.cursor += box.size;
        return;
      }
      if (box.type === "mdat") {
        // Without the index the payload cannot be split yet; keep it.
        this.retainFrom ??= this.cursor;
        if (box.size === 0) return;
      }
      this.cursor += box.size;
    }
  }

  parseMoov(moov) {
    const bytes = this.buffer;
    const mdia = soundTrack(bytes, moov);
    const sampleRate = timescale(bytes, child(bytes, mdia, "mdhd"));
    const stbl = child(bytes, child(bytes, mdia, "minf"), "stbl");
    if (!stbl) throw new Error("m4a 缺少 stbl");
    const cookie = alacCookie(bytes, child(bytes, stbl, "stsd"));
    const maxSamples = readU32(cookie, 0);
    const header = { sampleRate, channels: cookie[9], bits: cookie[5], maxSamples, cookie };
    if (!sampleRate || !header.channels || !maxSamples) throw new Error("m4a ALAC 参数无效");
    this.table = sampleTable(bytes, stbl);
    this.totalFrames = this.table.durations.reduce((sum, value) => sum + value, 0);
    this.header = header;
    this.headerBytes = packetStreamHeader(header);
    this.retainFrom = null;
  }

  push(chunk) {
    if (chunk?.byteLength) this.append(chunk);
    this.readBoxes();
    const packets = [];
    if (!this.table) return packets;
    const { offsets, sizes, durations } = this.table;
    while (this.next < offsets.length) {
      const start = offsets[this.next] - this.bufferStart;
      if (start < 0) throw new Error("m4a packet 数据已丢失");
      const end = start + sizes[this.next];
      if (end > this.buffer.byteLength) break;
      packets.push({ index: this.next, duration: durations[this.next], bytes: this.buffer.subarray(start, end) });
      this.next += 1;
    }
    return packets;
  }

  finish() {
    if (!this.table || this.next !== this.table.offsets.length) throw new Error("m4a 在完整 packet 边界前结束");
  }

  /** Byte range [start, end) of packets [first, last). */
  byteRange(first, last) {
    const { offsets, sizes } = this.table;
    return { start: offsets[first], end: offsets[last - 1] + sizes[last - 1] };
  }

  /** Continue from packet `index`: the next bytes pushed start at its offset. */
  seekTo(index) {
    if (!this.table || index < 0 || index >= this.table.offsets.length) throw new Error("m4a 跳转位置无效");
    this.buffer = new Uint8Array(0);
    this.bufferStart = this.table.offsets[index];
    this.next = index;
  }
}
