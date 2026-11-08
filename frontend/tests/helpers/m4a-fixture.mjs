// Builds a small progressive ALAC M4A: ftyp, moov (one sound track with a
// complete sample table) and mdat. Packet bytes follow the fake decoder in
// alac-player.test.mjs: u32 frame count, then a one-byte sample value.

function box(type, ...parts) {
  const body = parts.flatMap((part) => [...part]);
  const out = new Uint8Array(8 + body.length);
  new DataView(out.buffer).setUint32(0, out.length);
  out.set(new TextEncoder().encode(type), 4);
  out.set(body, 8);
  return out;
}
const u32 = (...values) => {
  const out = new Uint8Array(values.length * 4);
  values.forEach((value, index) => new DataView(out.buffer).setUint32(index * 4, value));
  return out;
};
const full = (type, ...parts) => box(type, u32(0), ...parts);

export function alacCookie({ frameLength = 4096, bits = 24, channels = 2, sampleRate = 48000 } = {}) {
  const cookie = new Uint8Array(24);
  const view = new DataView(cookie.buffer);
  view.setUint32(0, frameLength);
  cookie[5] = bits; cookie[6] = 40; cookie[7] = 10; cookie[8] = 14; cookie[9] = channels;
  view.setUint16(10, 255);
  view.setUint32(20, sampleRate);
  return cookie;
}

export function packetBytes(index, frames = 4096) {
  const packet = new Uint8Array(5 + (index % 3));
  new DataView(packet.buffer).setUint32(0, frames);
  packet[4] = index + 1;
  return packet;
}

/**
 * @param {object} options
 * @param {number} options.packets   number of ALAC packets
 * @param {boolean} options.moovLast write mdat before moov
 * @param {number} options.perChunk  samples per stco chunk
 */
export function m4aFixture({ packets = 12, sampleRate = 48000, moovLast = false, perChunk = 5, lastFrames = 1000, codec = "alac" } = {}) {
  const samples = Array.from({ length: packets }, (_, index) => packetBytes(index, index === packets - 1 ? lastFrames : 4096));
  const ftyp = box("ftyp", new TextEncoder().encode("M4A "), u32(0), new TextEncoder().encode("M4A isom"));
  const mdatPayload = samples.flatMap((sample) => [...sample]);
  const mdat = box("mdat", mdatPayload);
  const chunkCount = Math.ceil(packets / perChunk);
  const build = (mdatStart) => {
    const offsets = [];
    let offset = mdatStart + 8;
    for (let chunk = 0; chunk < chunkCount; chunk += 1) {
      offsets.push(offset);
      for (let index = chunk * perChunk; index < Math.min(packets, (chunk + 1) * perChunk); index += 1) offset += samples[index].length;
    }
    const lastChunkSamples = packets - (chunkCount - 1) * perChunk;
    const stscEntries = lastChunkSamples === perChunk || chunkCount === 1
      ? [[1, chunkCount === 1 ? lastChunkSamples : perChunk, 1]]
      : [[1, perChunk, 1], [chunkCount, lastChunkSamples, 1]];
    const entry = box(codec, new Uint8Array(6), new Uint8Array([0, 1]), new Uint8Array(20), full("alac", alacCookie({ sampleRate })));
    const stbl = box("stbl",
      full("stsd", u32(1), entry),
      full("stts", ...(lastFrames === 4096 ? [u32(1, packets, 4096)] : [u32(2, packets - 1, 4096, 1, lastFrames)])),
      full("stsc", u32(stscEntries.length), ...stscEntries.map((values) => u32(...values))),
      full("stsz", u32(0, packets), u32(...samples.map((sample) => sample.length))),
      full("stco", u32(chunkCount, ...offsets)));
    const mdhd = full("mdhd", u32(0, 0, sampleRate, packets * 4096), new Uint8Array(4));
    const hdlr = full("hdlr", u32(0), new TextEncoder().encode("soun"), new Uint8Array(13));
    const trak = box("trak", box("mdia", mdhd, hdlr, box("minf", stbl)));
    return box("moov", full("mvhd", new Uint8Array(96)), trak);
  };
  let moov;
  let parts;
  if (moovLast) {
    moov = build(ftyp.length);
    parts = [ftyp, mdat, moov];
  } else {
    // moov's size does not depend on the offsets it holds.
    moov = build(ftyp.length + build(0).length);
    parts = [ftyp, moov, mdat];
  }
  const file = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let at = 0;
  for (const part of parts) { file.set(part, at); at += part.length; }
  return { file, samples };
}
