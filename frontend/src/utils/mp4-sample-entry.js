// Older Apple Music downloads labelled decrypted AAC as `alac` in the MP4
// sample entry. Chromium decodes from `esds` anyway; Safari trusts the fourcc
// and rejects the file ("NotSupportedError"). The fix is four bytes in place.

const MOOV_LIMIT = 4 * 1024 * 1024;

function type(bytes, at) {
  return String.fromCharCode(bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]);
}

function* boxes(bytes, start, end) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let pos = start;
  while (pos + 8 <= end) {
    let size = view.getUint32(pos);
    let header = 8;
    if (size === 1) {
      if (pos + 16 > end) return;
      size = Number(view.getBigUint64(pos + 8));
      header = 16;
    } else if (size === 0) {
      size = end - pos;
    }
    if (size < header || pos + size > end) return;
    yield { start: pos, type: type(bytes, pos + 4), end: pos + size };
    pos += size;
  }
}

function child(bytes, start, end, wanted) {
  for (const box of boxes(bytes, start, end)) if (box.type === wanted) return box;
  return null;
}

/**
 * Offset of a mislabelled `alac` fourcc of an AAC sample entry inside
 * `bytes`, which must start at the file's first byte and hold its `moov`.
 */
export function mislabeledAacOffset(bytes) {
  const moov = child(bytes, 0, bytes.length, "moov");
  if (!moov) return -1;
  for (const trak of boxes(bytes, moov.start + 8, moov.end)) {
    if (trak.type !== "trak") continue;
    let box = trak;
    for (const name of ["mdia", "minf", "stbl", "stsd"]) {
      box = box && child(bytes, box.start + 8, box.end, name);
    }
    if (!box) continue;
    for (const entry of boxes(bytes, box.start + 16, box.end)) {
      // Audio sample entries carry 28 bytes of fixed fields.
      if (entry.type === "alac" && child(bytes, entry.start + 36, entry.end, "esds")) {
        return entry.start + 4;
      }
    }
  }
  return -1;
}

/** A corrected copy of `blob`, or null when it needs no repair. */
export async function repairMislabeledAacBlob(blob) {
  if (!(blob instanceof Blob) || blob.size < 16) return null;
  // Read top-level boxes up to the end of `moov`.
  const first = new Uint8Array(await blob.slice(0, Math.min(blob.size, 64 * 1024)).arrayBuffer());
  let moovEnd = 0;
  for (const box of boxes(first, 0, first.length)) {
    if (box.type === "moov") { moovEnd = box.end; break; }
  }
  if (!moovEnd) {
    // `moov` may run past the first read; find its declared end.
    const view = new DataView(first.buffer);
    let pos = 0;
    while (pos + 8 <= first.length) {
      const size = view.getUint32(pos);
      const name = type(first, pos + 4);
      if (size < 8 || name === "mdat") return null;
      if (name === "moov") { moovEnd = pos + size; break; }
      pos += size;
    }
  }
  if (!moovEnd || moovEnd > MOOV_LIMIT || moovEnd > blob.size) return null;
  const header = moovEnd <= first.length
    ? first.subarray(0, moovEnd)
    : new Uint8Array(await blob.slice(0, moovEnd).arrayBuffer());
  const offset = mislabeledAacOffset(header);
  if (offset < 0) return null;
  const patched = header.slice(0, offset + 4);
  patched.set([0x6d, 0x70, 0x34, 0x61], offset); // "mp4a"
  return new Blob([patched, blob.slice(offset + 4)], { type: blob.type });
}
