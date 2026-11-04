//! Lyrics written into the copy of a song uploaded to Telegram, so clients
//! that read them from the file (Nagram, exteraless) can show and follow
//! them while playing: ID3v2 `USLT` for MP3, `©lyr` for M4A and the
//! `LYRICS` Vorbis comment for FLAC. Those clients only parse the first two;
//! FLAC still carries them for players that keep the downloaded file.
//!
//! Only the uploaded bytes change. The local file a web playback serves stays
//! as downloaded, so one media generation never mixes two byte streams.

use crate::music::Lyrics;
use ferrous_opencc::{OpenCC, config::BuiltinConfig};
use lofty::{
    TextEncoding,
    config::{ParseOptions, WriteOptions},
    error::LoftyError,
    file::AudioFile,
    id3::v2::{Frame, FrameId, Id3v2Tag, UnsynchronizedTextFrame},
    mpeg::MpegFile,
};
use std::{borrow::Cow, collections::HashMap, io::Cursor, sync::LazyLock};

/// Built from configuration compiled into the crate, so it cannot fail at
/// run time.
static TO_SIMPLIFIED: LazyLock<OpenCC> =
    LazyLock::new(|| OpenCC::from_config(BuiltinConfig::T2s).expect("builtin OpenCC t2s config"));

/// `text` in Simplified Chinese. A file holds one set of lyrics, and the
/// bot shows Simplified by default; readers who want Traditional switch in
/// the bot's lyrics page.
pub fn to_simplified(text: &str) -> String {
    TO_SIMPLIFIED.convert(text)
}

/// What `music_cache.lyrics_tag` holds once an upload has been through
/// lyrics tagging; 0 means it has not (uploaded before lyrics were embedded,
/// or its lyrics could not be fetched at the time).
pub const LYRICS_TAGGED: i8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Container {
    Mp3,
    Flac,
    Mp4,
}

impl Container {
    /// Sniffs the container from its leading bytes; extensions lie (QQ FLAC
    /// behind an ID3 header, MP3 named `.flac`).
    pub fn sniff(audio: &[u8]) -> Option<Self> {
        let body = skip_id3v2(audio);
        if body.starts_with(b"fLaC") {
            Some(Self::Flac)
        } else if audio.len() >= 8 && &audio[4..8] == b"ftyp" {
            Some(Self::Mp4)
        } else if body.len() >= 2 && body[0] == 0xFF && body[1] & 0xE0 == 0xE0 {
            Some(Self::Mp3)
        } else if audio.starts_with(b"ID3") {
            // An ID3 header followed by something other than FLAC: MPEG audio
            // whose first frame does not start right after the tag.
            Some(Self::Mp3)
        } else {
            None
        }
    }
}

fn skip_id3v2(audio: &[u8]) -> &[u8] {
    if audio.len() < 10 || !audio.starts_with(b"ID3") {
        return audio;
    }
    let size = audio[6..10]
        .iter()
        .fold(0usize, |size, byte| (size << 7) | usize::from(byte & 0x7F));
    let footer = if audio[5] & 0x10 != 0 { 10 } else { 0 };
    audio.get(10 + size + footer..).unwrap_or_default()
}

/// The LRC text to embed: the original lines, each followed by its
/// translation under the same timestamp (how both clients show bilingual
/// lyrics). Untimed lyrics are embedded as plain text. `None` when there is
/// nothing worth embedding.
pub fn lyrics_text(lyrics: &Lyrics) -> Option<String> {
    let plain = lyrics.plain.trim();
    if plain.is_empty() {
        return None;
    }
    let translations = timed_lines(&lyrics.translation)
        .filter(|(_, _, text)| is_real_line(text))
        .map(|(time, _, text)| (time, text.to_string()))
        .collect::<HashMap<_, _>>();
    let mut out = String::with_capacity(plain.len() * 2);
    let mut timed = false;
    for row in plain.lines() {
        let row = row.trim_end();
        out.push_str(row);
        out.push('\n');
        let Some((time, tag, text)) = timed_line(row) else {
            continue;
        };
        timed = true;
        if let Some(translation) = translations.get(&time)
            && translation.trim() != text.trim()
        {
            out.push_str(tag);
            out.push_str(translation);
            out.push('\n');
        }
    }
    // A plain-text body only counts when it has words in it, not just tags.
    if !timed && !plain.lines().any(|row| is_real_line(row) && !is_meta(row)) {
        return None;
    }
    Some(out.trim_end().to_string())
}

/// `//` and blank strings are NetEase's "no translation for this line".
fn is_real_line(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && text != "//"
}

fn is_meta(row: &str) -> bool {
    let row = row.trim();
    row.starts_with('[')
        && row.ends_with(']')
        && row[1..]
            .split(':')
            .next()
            .is_some_and(|key| !key.is_empty() && key.chars().all(|c| c.is_ascii_alphabetic()))
}

fn timed_lines(text: &str) -> impl Iterator<Item = (u64, &str, &str)> {
    text.lines().filter_map(timed_line)
}

/// The first timestamp of a line starting with `[mm:ss(.xx)]`, the tag text
/// itself, and what follows all leading tags.
fn timed_line(row: &str) -> Option<(u64, &str, &str)> {
    let row = row.trim_start();
    let mut first: Option<(u64, &str)> = None;
    let mut rest = row;
    while let Some(after) = rest.strip_prefix('[') {
        let end = after.find(']')?;
        let Some(time) = parse_time(&after[..end]) else {
            break;
        };
        first.get_or_insert((time, &rest[..end + 2]));
        rest = &after[end + 1..];
    }
    first.map(|(time, tag)| (time, tag, rest))
}

fn parse_time(tag: &str) -> Option<u64> {
    let (minutes, seconds) = tag.split_once(':')?;
    let (seconds, fraction) = match seconds.split_once(['.', ':']) {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (seconds, None),
    };
    let digits = |value: &str, max: usize| {
        (!value.is_empty() && value.len() <= max && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| value.parse::<u64>().ok())
            .flatten()
    };
    let mut millis = digits(minutes, 3)? * 60_000 + digits(seconds, 2)? * 1000;
    if let Some(fraction) = fraction {
        let value = digits(fraction, 3)?;
        millis += match fraction.len() {
            1 => value * 100,
            2 => value * 10,
            _ => value,
        };
    }
    Some(millis)
}

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("不支持写入歌词的音频容器")]
    Unsupported,
    /// Lofty breaks fragment offsets when it grows `moov`.
    #[error("分片 MP4 不写入歌词")]
    Fragmented,
    #[error("写入歌词失败: {0}")]
    Tag(#[from] LoftyError),
    #[error("FLAC 元数据损坏")]
    MalformedFlac,
    #[error("歌词过长，无法写入 FLAC")]
    TooLarge,
    #[error("MP4 结构损坏")]
    MalformedMp4,
}

/// A copy of `audio` with `lyrics` in its native lyrics field, replacing any
/// lyrics already there. Audio data and other tags are kept.
pub fn embed(audio: &[u8], lyrics: &str) -> Result<Vec<u8>, EmbedError> {
    // No added padding: lofty 0.22 indexes out of bounds adding a PADDING
    // block to a FLAC that has none, and a panic aborts the whole server.
    let write = WriteOptions::default().preferred_padding(0);
    let container = Container::sniff(audio).ok_or(EmbedError::Unsupported)?;
    let options = ParseOptions::new().read_properties(false);
    let mut cursor = Cursor::new(audio.to_vec());
    match container {
        Container::Mp3 => {
            let mut file = MpegFile::read_from(&mut cursor, options)?;
            let mut tag = file.remove_id3v2().unwrap_or_else(Id3v2Tag::new);
            // Clients take the first USLT frame, so ours must be the only one.
            let _ = tag.remove(&FrameId::Valid(Cow::Borrowed("USLT")));
            tag.insert(Frame::UnsynchronizedText(UnsynchronizedTextFrame::new(
                TextEncoding::UTF8,
                *b"XXX",
                String::new(),
                lyrics.to_string(),
            )));
            file.set_id3v2(tag);
            cursor.set_position(0);
            file.save_to(&mut cursor, write)?;
        }
        Container::Flac => return embed_flac(audio, lyrics),
        Container::Mp4 => return embed_mp4(audio, lyrics),
    }
    Ok(cursor.into_inner())
}

const FLAC_STREAMINFO: u8 = 0;
const FLAC_VORBIS_COMMENT: u8 = 4;
const FLAC_MAX_BLOCK: usize = (1 << 24) - 1;

/// Rewrites the metadata blocks with one Vorbis comment block whose `LYRICS`
/// is `lyrics`, keeping its vendor, its other comments and every other block
/// in order. Done by hand: lofty 0.22 loses the last-block flag when a FLAC
/// has no PADDING block, which leaves the stream undecodable.
fn embed_flac(audio: &[u8], lyrics: &str) -> Result<Vec<u8>, EmbedError> {
    let prefix = audio.len() - skip_id3v2(audio).len();
    let flac = &audio[prefix..];
    let mut offset = 4;
    let mut blocks: Vec<(u8, &[u8])> = Vec::new();
    loop {
        let header = flac
            .get(offset..offset + 4)
            .ok_or(EmbedError::MalformedFlac)?;
        let last = header[0] & 0x80 != 0;
        let kind = header[0] & 0x7F;
        let size =
            usize::from(header[1]) << 16 | usize::from(header[2]) << 8 | usize::from(header[3]);
        let body = flac
            .get(offset + 4..offset + 4 + size)
            .ok_or(EmbedError::MalformedFlac)?;
        if kind == 127 || (blocks.is_empty() && kind != FLAC_STREAMINFO) {
            return Err(EmbedError::MalformedFlac);
        }
        blocks.push((kind, body));
        offset += 4 + size;
        if last {
            break;
        }
    }
    let frames = &flac[offset..];

    let (vendor, mut comments) = blocks
        .iter()
        .find(|(kind, _)| *kind == FLAC_VORBIS_COMMENT)
        .map(|(_, body)| parse_vorbis_comments(body))
        .transpose()?
        .unwrap_or_else(|| (b"MHub".as_slice(), Vec::new()));
    comments.retain(|comment| {
        !comment
            .split(|byte| *byte == b'=')
            .next()
            .is_some_and(|key| key.eq_ignore_ascii_case(b"LYRICS"))
    });
    let lyrics_comment = [b"LYRICS=".as_slice(), lyrics.as_bytes()].concat();
    comments.push(&lyrics_comment);

    let mut comment_block = Vec::new();
    push_u32_le(&mut comment_block, vendor.len())?;
    comment_block.extend_from_slice(vendor);
    push_u32_le(&mut comment_block, comments.len())?;
    for comment in &comments {
        push_u32_le(&mut comment_block, comment.len())?;
        comment_block.extend_from_slice(comment);
    }
    if comment_block.len() > FLAC_MAX_BLOCK {
        return Err(EmbedError::TooLarge);
    }

    let mut ordered: Vec<(u8, &[u8])> = Vec::with_capacity(blocks.len() + 1);
    ordered.push(blocks[0]);
    ordered.push((FLAC_VORBIS_COMMENT, &comment_block));
    ordered.extend(
        blocks[1..]
            .iter()
            .filter(|(kind, _)| *kind != FLAC_VORBIS_COMMENT),
    );

    let mut out = Vec::with_capacity(audio.len() + comment_block.len() + 4);
    out.extend_from_slice(&audio[..prefix]);
    out.extend_from_slice(b"fLaC");
    let count = ordered.len();
    for (index, (kind, body)) in ordered.into_iter().enumerate() {
        let last = if index + 1 == count { 0x80 } else { 0 };
        out.push(last | kind);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        out.extend_from_slice(body);
    }
    out.extend_from_slice(frames);
    Ok(out)
}

fn parse_vorbis_comments(body: &[u8]) -> Result<(&[u8], Vec<&[u8]>), EmbedError> {
    let mut rest = body;
    let mut take = |len: usize| -> Result<&[u8], EmbedError> {
        let (head, tail) = rest
            .split_at_checked(len)
            .ok_or(EmbedError::MalformedFlac)?;
        rest = tail;
        Ok(head)
    };
    let read_len = |bytes: &[u8]| u32::from_le_bytes(bytes.try_into().unwrap()) as usize;
    let vendor_len = read_len(take(4)?);
    let vendor = take(vendor_len)?;
    let count = read_len(take(4)?);
    let mut comments = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let len = read_len(take(4)?);
        comments.push(take(len)?);
    }
    Ok((vendor, comments))
}

fn push_u32_le(out: &mut Vec<u8>, value: usize) -> Result<(), EmbedError> {
    let value = u32::try_from(value).map_err(|_| EmbedError::TooLarge)?;
    out.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

/// One box of an MP4 file: where it starts, where its payload starts and
/// where it ends, all as offsets into the buffer it was read from.
#[derive(Clone, Copy, Debug)]
struct Mp4Box {
    kind: [u8; 4],
    start: usize,
    payload: usize,
    end: usize,
}

/// The boxes laid end to end between `start` and `end`. Anything that does
/// not tile that range exactly is an error, so a file is never rewritten
/// from a misread layout.
fn mp4_boxes(data: &[u8], start: usize, end: usize) -> Result<Vec<Mp4Box>, EmbedError> {
    let bad = || EmbedError::MalformedMp4;
    let mut boxes = Vec::new();
    let mut offset = start;
    while offset < end {
        let header = data.get(offset..offset + 8).ok_or_else(bad)?;
        let kind: [u8; 4] = header[4..8].try_into().unwrap();
        let (size, payload) = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
            0 => (end - offset, offset + 8),
            1 => {
                let large = data.get(offset + 8..offset + 16).ok_or_else(bad)?;
                let size = u64::from_be_bytes(large.try_into().unwrap());
                (usize::try_from(size).map_err(|_| bad())?, offset + 16)
            }
            size => (size as usize, offset + 8),
        };
        let box_end = offset.checked_add(size).ok_or_else(bad)?;
        if box_end > end || box_end < payload {
            return Err(bad());
        }
        boxes.push(Mp4Box {
            kind,
            start: offset,
            payload,
            end: box_end,
        });
        offset = box_end;
    }
    Ok(boxes)
}

fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 16);
    match u32::try_from(payload.len() + 8) {
        Ok(size) => out.extend_from_slice(&size.to_be_bytes()),
        Err(_) => {
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(&((payload.len() + 16) as u64).to_be_bytes());
            out.extend_from_slice(payload);
            return out;
        }
    }
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

const LYRICS_ATOM: &[u8; 4] = b"\xa9lyr";

fn lyrics_atom(lyrics: &str) -> Vec<u8> {
    // `data`: type 1 (UTF-8), locale 0, then the text.
    let mut data = Vec::with_capacity(lyrics.len() + 8);
    data.extend_from_slice(&1u32.to_be_bytes());
    data.extend_from_slice(&0u32.to_be_bytes());
    data.extend_from_slice(lyrics.as_bytes());
    mp4_box(LYRICS_ATOM, &mp4_box(b"data", &data))
}

/// `ilst` without its lyrics atoms, plus ours at the end.
fn rebuilt_ilst(data: &[u8], ilst: Option<Mp4Box>, lyrics: &str) -> Result<Vec<u8>, EmbedError> {
    let mut payload = Vec::new();
    if let Some(ilst) = ilst {
        for item in mp4_boxes(data, ilst.payload, ilst.end)? {
            if &item.kind != LYRICS_ATOM {
                payload.extend_from_slice(&data[item.start..item.end]);
            }
        }
    }
    payload.extend_from_slice(&lyrics_atom(lyrics));
    Ok(mp4_box(b"ilst", &payload))
}

fn rebuilt_meta(data: &[u8], meta: Option<Mp4Box>, lyrics: &str) -> Result<Vec<u8>, EmbedError> {
    let Some(meta) = meta else {
        // iTunes metadata: a full box holding an `mdir` handler and `ilst`.
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(b"mdirappl");
        hdlr.extend_from_slice(&[0; 9]);
        let mut payload = vec![0u8; 4];
        payload.extend_from_slice(&mp4_box(b"hdlr", &hdlr));
        payload.extend_from_slice(&rebuilt_ilst(data, None, lyrics)?);
        return Ok(mp4_box(b"meta", &payload));
    };
    // ISO `meta` is a full box (4 bytes of version and flags first); the
    // QuickTime form starts straight with its children.
    let full = data.get(meta.payload + 4..meta.payload + 8) != Some(b"hdlr".as_slice());
    let children_start = if full { meta.payload + 4 } else { meta.payload };
    let children = mp4_boxes(data, children_start, meta.end)?;
    let ilst = children
        .iter()
        .copied()
        .find(|child| &child.kind == b"ilst");
    let mut payload = data[meta.payload..children_start].to_vec();
    for child in &children {
        if &child.kind != b"ilst" {
            payload.extend_from_slice(&data[child.start..child.end]);
        }
    }
    payload.extend_from_slice(&rebuilt_ilst(data, ilst, lyrics)?);
    Ok(mp4_box(b"meta", &payload))
}

fn rebuilt_udta(data: &[u8], udta: Option<Mp4Box>, lyrics: &str) -> Result<Vec<u8>, EmbedError> {
    let mut payload = Vec::new();
    let mut meta = None;
    if let Some(udta) = udta {
        for child in mp4_boxes(data, udta.payload, udta.end)? {
            if &child.kind == b"meta" && meta.is_none() {
                meta = Some(child);
            } else {
                payload.extend_from_slice(&data[child.start..child.end]);
            }
        }
    }
    payload.extend_from_slice(&rebuilt_meta(data, meta, lyrics)?);
    Ok(mp4_box(b"udta", &payload))
}

const SAMPLE_TABLE_PATH: [&[u8; 4]; 5] = [b"moov", b"trak", b"mdia", b"minf", b"stbl"];

/// Every chunk offset table (`stco`/`co64`) under the boxes in `boxes`.
fn chunk_offset_tables(data: &[u8], boxes: &[Mp4Box]) -> Result<Vec<Mp4Box>, EmbedError> {
    let mut tables = Vec::new();
    for item in boxes {
        if &item.kind == b"stco" || &item.kind == b"co64" {
            tables.push(*item);
        } else if SAMPLE_TABLE_PATH.contains(&&item.kind) {
            tables.extend(chunk_offset_tables(
                data,
                &mp4_boxes(data, item.payload, item.end)?,
            )?);
        }
    }
    Ok(tables)
}

/// Calls `edit` with each chunk offset in `table`, writing back what it
/// returns.
fn edit_chunk_offsets(
    data: &mut [u8],
    table: Mp4Box,
    mut edit: impl FnMut(u64) -> Option<u64>,
) -> Result<(), EmbedError> {
    let wide = &table.kind == b"co64";
    let width = if wide { 8 } else { 4 };
    let count = data
        .get(table.payload + 4..table.payload + 8)
        .map(|bytes| u32::from_be_bytes(bytes.try_into().unwrap()) as usize)
        .ok_or(EmbedError::MalformedMp4)?;
    let first = table.payload + 8;
    if first + count * width > table.end {
        return Err(EmbedError::MalformedMp4);
    }
    for index in 0..count {
        let at = first + index * width;
        let value = if wide {
            u64::from_be_bytes(data[at..at + 8].try_into().unwrap())
        } else {
            u64::from(u32::from_be_bytes(data[at..at + 4].try_into().unwrap()))
        };
        let Some(value) = edit(value) else { continue };
        if wide {
            data[at..at + 8].copy_from_slice(&value.to_be_bytes());
        } else {
            let value = u32::try_from(value).map_err(|_| EmbedError::TooLarge)?;
            data[at..at + 4].copy_from_slice(&value.to_be_bytes());
        }
    }
    Ok(())
}

#[cfg(test)]
fn first_chunk_offset(data: &[u8], table: Mp4Box) -> Option<u64> {
    let wide = &table.kind == b"co64";
    let count = u32::from_be_bytes(
        data.get(table.payload + 4..table.payload + 8)?
            .try_into()
            .ok()?,
    );
    if count == 0 {
        return None;
    }
    let at = table.payload + 8;
    Some(if wide {
        u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?)
    } else {
        u64::from(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
    })
}

/// Writes `lyrics` into `moov/udta/meta/ilst/©lyr`, replacing lyrics there,
/// and moves every chunk offset that pointed past `moov` by the bytes
/// `moov` grew. Done by hand: lofty 0.22 shifts chunk offsets by the wrong
/// amount when `moov` already has a `udta` (as Apple Music files do), which
/// silences the song.
fn embed_mp4(audio: &[u8], lyrics: &str) -> Result<Vec<u8>, EmbedError> {
    let top = mp4_boxes(audio, 0, audio.len())?;
    if top.iter().any(|item| &item.kind == b"moof") {
        return Err(EmbedError::Fragmented);
    }
    let mut moovs = top.iter().filter(|item| &item.kind == b"moov");
    let (Some(&moov), None) = (moovs.next(), moovs.next()) else {
        return Err(EmbedError::MalformedMp4);
    };
    let children = mp4_boxes(audio, moov.payload, moov.end)?;
    let udta = children
        .iter()
        .copied()
        .find(|child| &child.kind == b"udta");
    let mut payload = Vec::with_capacity(moov.end - moov.payload + lyrics.len() + 128);
    for child in &children {
        if Some(child.start) == udta.map(|udta| udta.start) {
            continue;
        }
        payload.extend_from_slice(&audio[child.start..child.end]);
    }
    payload.extend_from_slice(&rebuilt_udta(audio, udta, lyrics)?);
    let mut new_moov = mp4_box(b"moov", &payload);

    let old_len = (moov.end - moov.start) as u64;
    let grown = new_moov.len() as u64 - old_len.min(new_moov.len() as u64);
    let shrunk = old_len.saturating_sub(new_moov.len() as u64);
    let moved_past = moov.end as u64;
    let new_top = mp4_boxes(&new_moov, 0, new_moov.len())?;
    let tables = chunk_offset_tables(&new_moov, &new_top)?;

    for table in tables {
        edit_chunk_offsets(&mut new_moov, table, |offset| {
            Some(if offset >= moved_past {
                offset + grown - shrunk
            } else {
                offset
            })
        })?;
    }

    let mut out = Vec::with_capacity(audio.len() + grown as usize);
    out.extend_from_slice(&audio[..moov.start]);
    out.extend_from_slice(&new_moov);
    out.extend_from_slice(&audio[moov.end..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lyrics(plain: &str, translation: &str) -> Lyrics {
        Lyrics {
            plain: plain.into(),
            translation: translation.into(),
            words: Vec::new(),
            roman: String::new(),
            credits: Vec::new(),
        }
    }

    #[test]
    fn translations_follow_their_line_under_the_same_tag() {
        let text = lyrics_text(&lyrics(
            "[ti:Song]\n[00:01.00]Hello\n[00:03.50]World\n[00:05.00]",
            "[00:01.000]你好\n[00:03.5]//\n[00:05.00]",
        ))
        .unwrap();
        assert_eq!(
            text,
            "[ti:Song]\n[00:01.00]Hello\n[00:01.00]你好\n[00:03.50]World\n[00:05.00]"
        );
    }

    #[test]
    fn traditional_lyrics_become_simplified() {
        assert_eq!(
            to_simplified("[00:01.00]還記得你說家是唯一的城堡"),
            "[00:01.00]还记得你说家是唯一的城堡"
        );
        assert_eq!(to_simplified("[00:01.00]Hello"), "[00:01.00]Hello");
    }

    #[test]
    fn untimed_lyrics_are_kept_and_empty_ones_dropped() {
        assert_eq!(
            lyrics_text(&lyrics("第一行\n第二行", "")).as_deref(),
            Some("第一行\n第二行")
        );
        assert_eq!(lyrics_text(&lyrics("  \n", "")), None);
        assert_eq!(lyrics_text(&lyrics("[ar:Singer]\n[ti:Song]", "")), None);
    }

    #[test]
    fn repeated_timestamps_use_the_first_for_translations() {
        let text = lyrics_text(&lyrics("[00:01.00][00:30.00]副歌", "[00:01.00]Chorus")).unwrap();
        assert_eq!(text, "[00:01.00][00:30.00]副歌\n[00:01.00]Chorus");
    }

    #[test]
    fn sniffs_containers_from_bytes() {
        assert_eq!(Container::sniff(b"fLaC\0\0\0\x22"), Some(Container::Flac));
        assert_eq!(
            Container::sniff(b"\0\0\0\x20ftypM4A \0\0\0\0"),
            Some(Container::Mp4)
        );
        assert_eq!(
            Container::sniff(&[0xFF, 0xFB, 0x90, 0x00]),
            Some(Container::Mp3)
        );
        let mut tagged_flac = b"ID3\x04\0\0\0\0\0\x02xx".to_vec();
        tagged_flac.extend_from_slice(b"fLaC");
        assert_eq!(Container::sniff(&tagged_flac), Some(Container::Flac));
        assert_eq!(Container::sniff(b"OggS\0\x02"), None);
    }

    #[test]
    fn flac_lyrics_replace_old_ones_and_keep_the_audio() {
        // STREAMINFO (last metadata block) followed by stand-in frame bytes.
        let mut flac = b"fLaC\x80\0\0\x22".to_vec();
        flac.extend_from_slice(&[0x10, 0x00, 0x10, 0x00]);
        flac.extend_from_slice(&[0; 6]);
        flac.extend_from_slice(&[0x0A, 0xC4, 0x40, 0xF0, 0, 0, 0, 0]);
        flac.extend_from_slice(&[0; 16]);
        let frames = [0xFF, 0xF8, 0x69, 0x08, 1, 2, 3, 4];
        flac.extend_from_slice(&frames);

        let once = embed(&flac, "[00:01.00]旧").unwrap();
        let twice = embed(&once, "[00:01.00]新").unwrap();
        assert!(twice.ends_with(&frames));
        let text = String::from_utf8_lossy(&twice);
        assert!(text.contains("LYRICS=[00:01.00]新"));
        assert!(!text.contains("旧"));
    }

    #[test]
    #[ignore = "needs LYRIC_FIXTURES pointing at real audio files"]
    fn real_files_round_trip() {
        let dir = std::env::var("LYRIC_FIXTURES").unwrap();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.starts_with("in") && !name.starts_with("nopad") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            if name.ends_with("alac.m4a") {
                let before = crate::music::alac_transport_from_progressive(&bytes).unwrap();
                let tagged = embed(&bytes, "[00:01.00]x").unwrap();
                let after = crate::music::alac_transport_from_progressive(&tagged).unwrap();
                assert_eq!(before, after, "{name}: packet stream changed");
            }
            match embed(&bytes, "[00:01.00]旧")
                .and_then(|out| embed(&out, "[00:01.00]第一行\n[00:01.00]First"))
            {
                Ok(out) => std::fs::write(path.with_file_name(format!("out-{name}")), out).unwrap(),
                Err(err) => println!("{name}: {err}"),
            }
        }
    }

    #[test]
    fn fragmented_mp4_is_left_alone() {
        let mut mp4 = b"\0\0\0\x10ftypiso6\0\0\0\0".to_vec();
        mp4.extend_from_slice(b"\0\0\0\x08moov\0\0\0\x08moof\0\0\0\x08mdat");
        assert!(matches!(embed(&mp4, "x"), Err(EmbedError::Fragmented)));
    }

    /// ftyp, then a moov whose single track points at the start of the
    /// mdat payload and which already has a udta (as Apple Music files do),
    /// then the mdat.
    fn apple_like_mp4() -> Vec<u8> {
        let frames = b"ALACFRAMES".to_vec();
        let build = |offset: u32| {
            let mut stco = vec![0u8; 4];
            stco.extend_from_slice(&1u32.to_be_bytes());
            stco.extend_from_slice(&offset.to_be_bytes());
            let stbl = mp4_box(b"stbl", &mp4_box(b"stco", &stco));
            let minf = mp4_box(b"minf", &stbl);
            let mdia = mp4_box(b"mdia", &minf);
            let trak = mp4_box(b"trak", &mdia);
            let udta = mp4_box(b"udta", &mp4_box(b"swre", b"wrapper"));
            let moov = mp4_box(b"moov", &[trak, udta].concat());
            let mut file = mp4_box(b"ftyp", b"M4A \0\0\0\0");
            file.extend_from_slice(&moov);
            file
        };
        let head = build(0);
        let payload = (head.len() + 8) as u32;
        let mut file = build(payload);
        file.extend_from_slice(&mp4_box(b"mdat", &frames));
        file
    }

    fn first_chunk_and_payload(file: &[u8]) -> (u64, usize) {
        let top = mp4_boxes(file, 0, file.len()).unwrap();
        let mdat = top.iter().find(|item| &item.kind == b"mdat").unwrap();
        let tables = chunk_offset_tables(file, &top).unwrap();
        (first_chunk_offset(file, tables[0]).unwrap(), mdat.payload)
    }

    #[test]
    fn mp4_chunk_offsets_follow_the_grown_moov() {
        let file = apple_like_mp4();
        let once = embed(&file, "[00:01.00]第一行").unwrap();
        let twice = embed(&once, "[00:01.00]新的").unwrap();
        for tagged in [&once, &twice] {
            let (chunk, payload) = first_chunk_and_payload(tagged);
            assert_eq!(chunk as usize, payload);
            assert!(tagged.ends_with(b"ALACFRAMES"));
        }
        let text = String::from_utf8_lossy(&twice);
        assert!(text.contains("新的") && !text.contains("第一行"));
        assert!(text.contains("swre"), "other udta children are kept");
    }
}
