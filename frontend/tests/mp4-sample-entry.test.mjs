import test from "node:test";
import assert from "node:assert/strict";
import { mislabeledAacOffset, repairMislabeledAacBlob } from "../src/utils/mp4-sample-entry.js";

function box(type, ...parts) {
  const payload = Buffer.concat(parts.map((part) => Buffer.from(part)));
  const header = Buffer.alloc(8);
  header.writeUInt32BE(payload.length + 8);
  header.write(type, 4, "latin1");
  return Buffer.concat([header, payload]);
}

function movie(entryType, config) {
  const entry = box(entryType, Buffer.alloc(28), box(config, Buffer.alloc(12)));
  const stsd = box("stsd", Buffer.from([0, 0, 0, 0, 0, 0, 0, 1]), entry);
  const stbl = box("stbl", stsd);
  const minf = box("minf", box("smhd", Buffer.alloc(8)), stbl);
  const mdia = box("mdia", box("mdhd", Buffer.alloc(24)), minf);
  const trak = box("trak", box("tkhd", Buffer.alloc(84)), mdia);
  const moov = box("moov", box("mvhd", Buffer.alloc(100)), trak);
  return Buffer.concat([box("ftyp", Buffer.from("iso5\0\0\0\x01", "latin1")), moov, box("mdat", Buffer.from([1, 2, 3]))]);
}

test("AAC labelled alac is relabelled mp4a without changing its size", async () => {
  const blob = new Blob([movie("alac", "esds")], { type: "audio/mp4" });
  const repaired = await repairMislabeledAacBlob(blob);
  assert.ok(repaired);
  assert.equal(repaired.size, blob.size);
  assert.equal(repaired.type, "audio/mp4");
  assert.deepEqual(Buffer.from(await repaired.arrayBuffer()), movie("mp4a", "esds"));
  assert.equal(await repairMislabeledAacBlob(repaired), null);
});

test("real ALAC, correct AAC and non-MP4 data are left alone", async () => {
  assert.equal(mislabeledAacOffset(new Uint8Array(movie("alac", "alac"))), -1);
  assert.equal(mislabeledAacOffset(new Uint8Array(movie("mp4a", "esds"))), -1);
  assert.equal(await repairMislabeledAacBlob(new Blob([Buffer.from("fLaC\0\0\0\x22 not an mp4 file")])), null);
});
