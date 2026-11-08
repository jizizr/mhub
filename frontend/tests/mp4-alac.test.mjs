import test from "node:test";
import assert from "node:assert/strict";
import { Mp4AlacStreamParser, looksLikeMp4 } from "../src/core/player/mp4-alac.js";
import { m4aFixture, alacCookie } from "./helpers/m4a-fixture.mjs";

function parse(file, chunkSize) {
  const parser = new Mp4AlacStreamParser();
  const packets = [];
  for (let at = 0; at < file.length; at += chunkSize) {
    for (const packet of parser.push(file.slice(at, at + chunkSize))) packets.push({ duration: packet.duration, bytes: [...packet.bytes] });
  }
  parser.finish();
  return { parser, packets };
}

test("a moov-first ALAC M4A yields every packet in order for any chunking", () => {
  const { file, samples } = m4aFixture({ packets: 12, perChunk: 5, lastFrames: 1000 });
  assert.ok(looksLikeMp4(file));
  for (const size of [1, 7, 64, 1000, file.length]) {
    const { parser, packets } = parse(file, size);
    assert.deepEqual(packets.map((packet) => packet.bytes), samples.map((sample) => [...sample]), `chunk ${size}`);
    assert.deepEqual(packets.map((packet) => packet.duration), [...Array(11).fill(4096), 1000]);
    assert.equal(parser.totalFrames, 11 * 4096 + 1000);
    assert.deepEqual(parser.header, { sampleRate: 48000, channels: 2, bits: 24, maxSamples: 4096, cookie: alacCookie() });
  }
});

test("packets stream before the file ends when moov comes first", () => {
  const { file } = m4aFixture({ packets: 12 });
  const parser = new Mp4AlacStreamParser();
  const packets = parser.push(file.slice(0, file.length - 20));
  assert.ok(packets.length >= 8 && packets.length < 12, `got ${packets.length}`);
  assert.throws(() => parser.finish(), /完整 packet/);
});

test("a moov-last M4A still decodes once its index arrives", () => {
  const { file, samples } = m4aFixture({ packets: 9, moovLast: true });
  const parser = new Mp4AlacStreamParser();
  assert.deepEqual(parser.push(file.slice(0, file.length - 40)), []);
  const { packets } = parse(file, 33);
  assert.deepEqual(packets.map((packet) => packet.bytes), samples.map((sample) => [...sample]));
});

test("the header bytes are the SPLALAC1 v2 header so cached Blobs keep one format", () => {
  const { file } = m4aFixture({ sampleRate: 96000 });
  const { parser } = parse(file, file.length);
  const view = new DataView(parser.headerBytes.buffer);
  assert.equal(new TextDecoder().decode(parser.headerBytes.subarray(0, 8)), "SPLALAC1");
  assert.equal(view.getUint32(8), 2);
  assert.equal(view.getUint32(12), 96000);
  assert.equal(parser.headerBytes[16], 2);
  assert.equal(parser.headerBytes[17], 24);
  assert.equal(view.getUint16(18), 24);
  assert.equal(view.getUint32(20), 4096);
  assert.deepEqual([...parser.headerBytes.subarray(24)], [...alacCookie({ sampleRate: 96000 })]);
});

test("a non-ALAC M4A is rejected instead of being fed to the decoder", () => {
  const { file } = m4aFixture({ codec: "mp4a" });
  assert.throws(() => new Mp4AlacStreamParser().push(file), /不是 ALAC/);
});
