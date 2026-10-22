//! Incremental fMP4 utilities used by the Apple Music streaming path.
//!
//! This module deliberately has no Axum or HTTP dependencies.  It provides
//! the two pieces that are needed by an eventual chunked player endpoint:
//! an append-only fMP4 parser and a small Temari decryptor that works one
//! complete `moof`/`mdat` fragment at a time.  The parser never emits a
//! partial box, which means callers can safely write each returned fragment
//! to a growing response or file.

use std::{fmt, ops::Range, sync::Arc};

use temari::rounds::{Template, decrypt_par};

/// Errors produced by the incremental parser and decryptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamError {
    Truncated(&'static str),
    Invalid(&'static str),
    InvalidOwned(String),
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated(value) => write!(f, "truncated {value}"),
            Self::Invalid(value) => write!(f, "invalid {value}"),
            Self::InvalidOwned(value) => f.write_str(value),
        }
    }
}

impl std::error::Error for StreamError {}

/// A complete top-level fMP4 unit returned by [`AppendOnlyFmp4Parser`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fmp4Chunk {
    /// The initialization segment (`ftyp`, `moov`, and any leading boxes).
    Init(Vec<u8>),
    /// One complete media fragment (`moof` followed by its `mdat`).
    Fragment(Vec<u8>),
}

/// Incrementally parses a stream of top-level MP4 boxes.
///
/// Input can be split at arbitrary byte boundaries.  The parser emits the
/// initialization segment once the first complete `moof` header is present,
/// then emits each complete `moof` + `mdat` pair.  Incomplete input remains in
/// the internal buffer and is considered normal, rather than an error.
#[derive(Debug, Default)]
pub struct AppendOnlyFmp4Parser {
    buffer: Vec<u8>,
    cursor: usize,
    pending_moof: Option<usize>,
    emitted_init: bool,
}

impl AppendOnlyFmp4Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends bytes and returns every newly completed init or media chunk.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Fmp4Chunk>, StreamError> {
        self.buffer.extend_from_slice(bytes);
        let mut output = Vec::new();

        while let Some(header) = read_box_header(&self.buffer, self.cursor)? {
            let start = self.cursor;
            let end = self
                .cursor
                .checked_add(header.size)
                .ok_or(StreamError::Invalid("MP4 box size overflow"))?;

            if header.typ == *b"moof" {
                if let Some(previous) = self.pending_moof {
                    if previous != start {
                        return Err(StreamError::Invalid(
                            "fMP4 moof appeared before the previous mdat",
                        ));
                    }
                } else {
                    self.pending_moof = Some(start);
                }
                if !self.emitted_init {
                    // Emit the init as soon as the moof header is known.  Its
                    // body may still be arriving, but the init itself is
                    // already complete and can start playback immediately.
                    output.push(Fmp4Chunk::Init(self.buffer[..start].to_vec()));
                    self.emitted_init = true;
                }
            }
            if end > self.buffer.len() {
                break;
            }
            self.cursor = end;

            if header.typ == *b"mdat"
                && let Some(moof_start) = self.pending_moof.take()
            {
                output.push(Fmp4Chunk::Fragment(self.buffer[moof_start..end].to_vec()));
                // Everything before this mdat has been emitted.  Drop it
                // so long tracks do not retain the complete history.
                self.buffer.drain(..end);
                self.cursor = 0;
            }
        }
        Ok(output)
    }

    /// Returns whether the parser has emitted an initialization segment.
    pub fn has_init(&self) -> bool {
        self.emitted_init
    }

    /// Bytes currently buffered but not yet emitted.  Useful for diagnostics.
    pub fn buffered_len(&self) -> usize {
        self.buffer.len().saturating_sub(self.cursor)
    }

    /// Marks the input as complete and reports a trailing partial fragment.
    /// Callers that intentionally keep a live response open can omit this.
    pub fn finish(&self) -> Result<(), StreamError> {
        if self.pending_moof.is_some() || self.buffered_len() > 0 {
            return Err(StreamError::Truncated("fMP4 stream"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct BoxHeader {
    size: usize,
    typ: [u8; 4],
    header_size: usize,
}

fn read_box_header(data: &[u8], start: usize) -> Result<Option<BoxHeader>, StreamError> {
    if start >= data.len() {
        return Ok(None);
    }
    if data.len() - start < 8 {
        return Ok(None);
    }
    let size32 = u32::from_be_bytes(data[start..start + 4].try_into().unwrap());
    let typ: [u8; 4] = data[start + 4..start + 8].try_into().unwrap();
    let (size, mut header_size): (usize, usize) = match size32 {
        0 => {
            // A size-zero box extends to the end of its containing file.  It
            // has no meaningful end in an append-only stream, so wait for a
            // regular sized box instead of accidentally emitting partial data.
            return Err(StreamError::Invalid("size-zero MP4 box in stream"));
        }
        1 => {
            if data.len() - start < 16 {
                return Ok(None);
            }
            let size = u64::from_be_bytes(data[start + 8..start + 16].try_into().unwrap());
            let size = usize::try_from(size)
                .map_err(|_| StreamError::Invalid("MP4 largesize overflows usize"))?;
            (size, 16)
        }
        value => (value as usize, 8),
    };
    // UUID boxes carry a 16-byte user type immediately after the regular
    // header. Treat it as part of the header so an append boundary inside the
    // user type is never mistaken for a child MP4 box.
    if typ == *b"uuid" {
        header_size = header_size.saturating_add(16);
    }
    if size < header_size {
        return Err(StreamError::Invalid("MP4 box is smaller than its header"));
    }
    Ok(Some(BoxHeader {
        size,
        typ,
        header_size,
    }))
}

/// A byte range in an HTTP media resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteRange {
    pub offset: u64,
    pub length: u64,
}

impl ByteRange {
    pub fn new(offset: u64, length: u64) -> Option<Self> {
        (length > 0).then_some(Self { offset, length })
    }

    pub fn end(self) -> u64 {
        self.offset.saturating_add(self.length)
    }
}

/// Coalesces adjacent/overlapping HLS byte ranges.
///
/// `max_gap` allows callers to avoid a large number of tiny HTTP Range
/// requests when the playlist uses almost-contiguous ranges.  Returned ranges
/// are sorted by offset and contain no overlap.
pub fn coalesce_ranges(ranges: &[ByteRange], max_gap: u64) -> Vec<ByteRange> {
    let mut sorted = ranges
        .iter()
        .copied()
        .filter(|range| range.length > 0)
        .collect::<Vec<_>>();
    sorted.sort_by_key(|range| (range.offset, range.length));
    let mut merged = Vec::with_capacity(sorted.len());
    for range in sorted {
        let Some(last) = merged.last_mut() else {
            merged.push(range);
            continue;
        };
        if range.offset <= last.end().saturating_add(max_gap) {
            let end = last.end().max(range.end());
            last.length = end.saturating_sub(last.offset);
        } else {
            merged.push(range);
        }
    }
    merged
}

/// Splits a byte range into bounded requests for a resumable downloader.
pub fn split_range(range: ByteRange, chunk_size: u64) -> Vec<ByteRange> {
    if range.length == 0 || chunk_size == 0 {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut offset = range.offset;
    let end = range.end();
    while offset < end {
        let next = offset.saturating_add(chunk_size).min(end);
        result.push(ByteRange {
            offset,
            length: next.saturating_sub(offset),
        });
        offset = next;
    }
    result
}

/// Decrypts the samples in one complete fMP4 fragment with a Temari template.
///
/// This fast path intentionally supports the common CENC layout where each
/// `trun` carries explicit sample sizes and samples have no `senc` subsample
/// clear ranges.  Pattern encryption and subsample layouts should continue to
/// use the existing conservative wrapper path until a stream-specific parser
/// is selected.
#[derive(Clone)]
pub struct TemariFragmentDecryptor {
    template: Arc<Template>,
    batch_size: usize,
}

impl TemariFragmentDecryptor {
    pub fn new(template: Arc<Template>, batch_size: usize) -> Self {
        Self {
            template,
            batch_size: batch_size.clamp(1, 4096),
        }
    }

    pub fn decrypt_fragment(&self, fragment: &[u8]) -> Result<Vec<u8>, StreamError> {
        let ranges = sample_ranges(fragment)?;
        if ranges.is_empty() {
            return Err(StreamError::Invalid(
                "fMP4 fragment has no encrypted samples",
            ));
        }
        let mut output = fragment.to_vec();
        for batch in ranges.chunks(self.batch_size) {
            let payloads = batch
                .iter()
                .map(|range| output[range.clone()].to_vec())
                .collect::<Vec<_>>();
            let refs = payloads.iter().map(Vec::as_slice).collect::<Vec<_>>();
            let plaintexts = decrypt_par(self.template.as_ref(), &refs);
            if plaintexts.len() != batch.len() {
                return Err(StreamError::Invalid(
                    "Temari returned an unexpected sample count",
                ));
            }
            for (range, plaintext) in batch.iter().zip(plaintexts) {
                if plaintext.len() != range.len() {
                    return Err(StreamError::Invalid("Temari changed a sample length"));
                }
                output[range.clone()].copy_from_slice(&plaintext);
            }
        }
        Ok(output)
    }
}

#[derive(Debug, Clone, Copy)]
struct FragmentBox {
    start: usize,
    end: usize,
    payload_start: usize,
    typ: [u8; 4],
}

fn read_fragment_box(data: &[u8], start: usize) -> Result<Option<FragmentBox>, StreamError> {
    let Some(header) = read_box_header(data, start)? else {
        return Ok(None);
    };
    let end = start
        .checked_add(header.size)
        .ok_or(StreamError::Invalid("MP4 box size overflow"))?;
    if end > data.len() {
        return Err(StreamError::Truncated("fMP4 box"));
    }
    Ok(Some(FragmentBox {
        start,
        end,
        payload_start: start + header.header_size,
        typ: header.typ,
    }))
}

fn sample_ranges(fragment: &[u8]) -> Result<Vec<Range<usize>>, StreamError> {
    let mut moof = None;
    let mut mdat = None;
    let mut pos = 0;
    while let Some(box_info) = read_fragment_box(fragment, pos)? {
        match &box_info.typ {
            b"moof" => moof = Some(box_info),
            b"mdat" => mdat = Some(box_info),
            _ => {}
        }
        pos = box_info.end;
    }
    let moof = moof.ok_or(StreamError::Invalid("fMP4 fragment has no moof"))?;
    let mdat = mdat.ok_or(StreamError::Invalid("fMP4 fragment has no mdat"))?;
    if mdat.start < moof.end {
        return Err(StreamError::Invalid("mdat precedes moof"));
    }

    let mut ranges = Vec::new();
    let mut child = moof.payload_start;
    while let Some(box_info) = read_fragment_box(fragment, child)? {
        if box_info.end > moof.end {
            break;
        }
        if box_info.typ == *b"traf" {
            parse_traf_sample_ranges(fragment, box_info, moof.start, mdat, &mut ranges)?;
        }
        child = box_info.end;
    }
    Ok(ranges)
}

fn parse_traf_sample_ranges(
    data: &[u8],
    traf: FragmentBox,
    moof_start: usize,
    mdat: FragmentBox,
    output: &mut Vec<Range<usize>>,
) -> Result<(), StreamError> {
    let mut tfhd_defaults = 0u32;
    let mut base_data_offset = None::<usize>;
    let mut default_base_is_moof = false;
    let mut previous_sample_end = None::<usize>;
    let mut child = traf.payload_start;
    while let Some(box_info) = read_fragment_box(data, child)? {
        if box_info.end > traf.end {
            break;
        }
        match &box_info.typ {
            b"tfhd" => {
                let body = &data[box_info.payload_start..box_info.end];
                if body.len() < 8 {
                    return Err(StreamError::Truncated("tfhd"));
                }
                let flags = u32::from_be_bytes([0, body[1], body[2], body[3]]);
                let mut offset = 8usize;
                if flags & 0x000001 != 0 {
                    if offset + 8 > body.len() {
                        return Err(StreamError::Truncated("tfhd base data offset"));
                    }
                    let value = u64::from_be_bytes(body[offset..offset + 8].try_into().unwrap());
                    base_data_offset = Some(usize::try_from(value).map_err(|_| {
                        StreamError::Invalid("tfhd base data offset overflows usize")
                    })?);
                    offset += 8;
                }
                if flags & 0x000002 != 0 {
                    offset += 4;
                }
                if flags & 0x000008 != 0 {
                    offset += 4;
                }
                if flags & 0x000010 != 0 {
                    if offset + 4 > body.len() {
                        return Err(StreamError::Truncated("tfhd default sample size"));
                    }
                    tfhd_defaults =
                        u32::from_be_bytes(body[offset..offset + 4].try_into().unwrap());
                }
                default_base_is_moof = flags & 0x020000 != 0;
            }
            b"trun" => {
                let body = &data[box_info.payload_start..box_info.end];
                if body.len() < 8 {
                    return Err(StreamError::Truncated("trun"));
                }
                let flags = u32::from_be_bytes([0, body[1], body[2], body[3]]);
                let count = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
                let mut offset = 8usize;
                let data_offset = if flags & 0x000001 != 0 {
                    if offset + 4 > body.len() {
                        return Err(StreamError::Truncated("trun data offset"));
                    }
                    let value = i32::from_be_bytes(body[offset..offset + 4].try_into().unwrap());
                    offset += 4;
                    Some(value)
                } else {
                    None
                };
                if flags & 0x000004 != 0 {
                    offset = offset.saturating_add(4);
                }
                let mut sample_pos = if let Some(value) = data_offset {
                    // Apple HLS fragments normally set
                    // `default-base-is-moof`; older files omit the flag but
                    // still use the moof-relative offset.  An explicit
                    // `base_data_offset` wins in either case.
                    let base = base_data_offset
                        .or_else(|| default_base_is_moof.then_some(moof_start))
                        .unwrap_or({
                            // Legacy Apple fragments use moof-relative offsets
                            // even when this flag is omitted.
                            moof_start
                        });
                    usize::try_from(base as i64 + i64::from(value))
                        .map_err(|_| StreamError::Invalid("trun data offset is negative"))?
                } else if let Some(previous) = previous_sample_end {
                    previous
                } else if let Some(base) = base_data_offset {
                    base
                } else if default_base_is_moof {
                    moof_start
                } else {
                    mdat.payload_start
                };
                for _ in 0..count {
                    if flags & 0x000100 != 0 {
                        offset = offset.saturating_add(4);
                    }
                    let size = if flags & 0x000200 != 0 {
                        if offset + 4 > body.len() {
                            return Err(StreamError::Truncated("trun sample size"));
                        }
                        let value =
                            u32::from_be_bytes(body[offset..offset + 4].try_into().unwrap());
                        offset += 4;
                        value
                    } else {
                        tfhd_defaults
                    };
                    if flags & 0x000400 != 0 {
                        offset = offset.saturating_add(4);
                    }
                    if flags & 0x000800 != 0 {
                        offset = offset.saturating_add(4);
                    }
                    if size == 0 {
                        return Err(StreamError::Invalid("trun sample size is missing"));
                    }
                    let end = sample_pos
                        .checked_add(size as usize)
                        .ok_or(StreamError::Invalid("sample range overflow"))?;
                    if sample_pos < mdat.payload_start || end > mdat.end {
                        return Err(StreamError::Invalid("sample range is outside mdat"));
                    }
                    output.push(sample_pos..end);
                    sample_pos = end;
                }
                previous_sample_end = Some(sample_pos);
            }
            b"senc" => {
                // The fast path can decrypt a complete sample, but it must
                // not touch layouts that intentionally leave clear bytes in
                // the middle of a sample.  The subsample flag is bit 1 of
                // the full-box flags.
                let body = &data[box_info.payload_start..box_info.end];
                if body.len() < 4 {
                    return Err(StreamError::Truncated("senc"));
                }
                let flags = u32::from_be_bytes([0, body[1], body[2], body[3]]);
                if flags & 0x000002 != 0 {
                    return Err(StreamError::Invalid(
                        "fMP4 senc subsample encryption is unsupported by the streaming fast path",
                    ));
                }
            }
            _ => {}
        }
        child = box_info.end;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (payload.len() + 8) as u32;
        let mut output = size.to_be_bytes().to_vec();
        output.extend_from_slice(typ);
        output.extend_from_slice(payload);
        output
    }

    fn make_fragment(sample_sizes: &[u32]) -> Vec<u8> {
        let mdat_payload = sample_sizes
            .iter()
            .enumerate()
            .flat_map(|(index, size)| std::iter::repeat_n(index as u8 + 1, *size as usize))
            .collect::<Vec<_>>();
        // trun: version/flags (data offset + sample size), sample count,
        // data offset from moof start, then one size per sample.
        let mut trun_payload = vec![0, 0, 2, 1];
        trun_payload.extend_from_slice(&(sample_sizes.len() as u32).to_be_bytes());
        // moof has a fixed size in this synthetic fixture: traf + trun.
        let trun_size = 8 + trun_payload.len() + 4 + sample_sizes.len() * 4;
        let traf_size = 8 + trun_size;
        let moof_size = 8 + traf_size;
        let data_offset = (moof_size + 8) as i32;
        trun_payload[4..8].copy_from_slice(&(sample_sizes.len() as u32).to_be_bytes());
        trun_payload.extend_from_slice(&data_offset.to_be_bytes());
        for size in sample_sizes {
            trun_payload.extend_from_slice(&size.to_be_bytes());
        }
        let trun = mp4_box(b"trun", &trun_payload);
        let traf = mp4_box(b"traf", &trun);
        let moof = mp4_box(b"moof", &traf);
        let mdat = mp4_box(b"mdat", &mdat_payload);
        [moof, mdat].concat()
    }

    fn make_fragment_with_explicit_base(sample_sizes: &[u32]) -> Vec<u8> {
        let mdat_payload = sample_sizes
            .iter()
            .enumerate()
            .flat_map(|(index, size)| std::iter::repeat_n(index as u8 + 1, *size as usize))
            .collect::<Vec<_>>();
        let mut tfhd_payload = vec![0, 0, 0, 1]; // base_data_offset present
        tfhd_payload.extend_from_slice(&1u32.to_be_bytes());
        tfhd_payload.extend_from_slice(&0u64.to_be_bytes());
        let trun_size = 8 + 8 + 4 + sample_sizes.len() * 4;
        let moof_size = 8 + 8 + (8 + tfhd_payload.len()) + trun_size;
        let mdat_payload_start = moof_size + 8;
        let base_data_offset = (mdat_payload_start - 8) as u64;
        tfhd_payload[8..16].copy_from_slice(&base_data_offset.to_be_bytes());
        let tfhd = mp4_box(b"tfhd", &tfhd_payload);

        let mut trun_payload = vec![0, 0, 2, 1]; // data offset + sample size
        trun_payload.extend_from_slice(&(sample_sizes.len() as u32).to_be_bytes());
        trun_payload.extend_from_slice(&8i32.to_be_bytes());
        for size in sample_sizes {
            trun_payload.extend_from_slice(&size.to_be_bytes());
        }
        let trun = mp4_box(b"trun", &trun_payload);
        let traf = mp4_box(b"traf", &[tfhd, trun].concat());
        let moof = mp4_box(b"moof", &traf);
        let mdat = mp4_box(b"mdat", &mdat_payload);
        [moof, mdat].concat()
    }

    #[test]
    fn parser_handles_arbitrary_append_boundaries() {
        let init = [mp4_box(b"ftyp", b"init"), mp4_box(b"moov", b"meta")].concat();
        let fragment = make_fragment(&[3, 4]);
        let input = [init.clone(), fragment.clone()].concat();
        let mut parser = AppendOnlyFmp4Parser::new();
        let mut chunks = Vec::new();
        for byte in input {
            chunks.extend(parser.push(&[byte]).unwrap());
        }
        assert_eq!(
            chunks,
            vec![Fmp4Chunk::Init(init), Fmp4Chunk::Fragment(fragment)]
        );
        assert_eq!(parser.buffered_len(), 0);
    }

    #[test]
    fn parser_waits_for_complete_mdat() {
        let init = mp4_box(b"ftyp", b"x");
        let fragment = make_fragment(&[5]);
        let mut parser = AppendOnlyFmp4Parser::new();
        assert_eq!(parser.push(&init).unwrap(), Vec::new());
        let early = parser.push(&fragment[..8]).unwrap();
        assert!(matches!(early.as_slice(), [Fmp4Chunk::Init(_)]));
        assert!(
            parser
                .push(&fragment[8..fragment.len() - 1])
                .unwrap()
                .is_empty()
        );
        let output = parser.push(&fragment[fragment.len() - 1..]).unwrap();
        assert_eq!(output.len(), 1);
        assert!(matches!(output[0], Fmp4Chunk::Fragment(_)));
    }

    #[test]
    fn ranges_are_coalesced_and_split() {
        let ranges = [
            ByteRange::new(100, 10).unwrap(),
            ByteRange::new(115, 5).unwrap(),
            ByteRange::new(300, 1).unwrap(),
        ];
        assert_eq!(
            coalesce_ranges(&ranges, 5),
            vec![
                ByteRange::new(100, 20).unwrap(),
                ByteRange::new(300, 1).unwrap()
            ]
        );
        assert_eq!(
            split_range(ByteRange::new(0, 10).unwrap(), 4),
            vec![
                ByteRange::new(0, 4).unwrap(),
                ByteRange::new(4, 4).unwrap(),
                ByteRange::new(8, 2).unwrap(),
            ]
        );
    }

    #[test]
    fn sample_ranges_are_derived_from_trun() {
        let fragment = make_fragment(&[3, 4]);
        let ranges = sample_ranges(&fragment).unwrap();
        let mdat_payload_start = fragment.len() - 7;
        assert_eq!(
            ranges,
            vec![
                mdat_payload_start..mdat_payload_start + 3,
                mdat_payload_start + 3..mdat_payload_start + 7
            ]
        );
    }

    #[test]
    fn explicit_tfhd_base_is_used_for_trun_offset() {
        let fragment = make_fragment_with_explicit_base(&[3, 4]);
        let ranges = sample_ranges(&fragment).unwrap();
        let mdat_payload_start = fragment.len() - 7;
        assert_eq!(
            ranges,
            vec![
                mdat_payload_start..mdat_payload_start + 3,
                mdat_payload_start + 3..mdat_payload_start + 7
            ]
        );
    }
}
