//! 逐字（逐音节）歌词的统一时间轴。
//!
//! 网易云 YRC、QQ 音乐 QRC、酷狗 KRC、酷我 LRCX 与 Apple Music 逐音节 TTML
//! 都在这里转换为同一结构，前端按词渲染；LRC 只是由它派生的逐行视图。
//! 词的时间一律为毫秒绝对时间。

use serde::Serialize;

/// Native lyrics may graft online word timing only when this share of their
/// lines starts close to an online line with matching text.
const MIN_ALIGNED_SHARE: f64 = 0.7;
const ALIGN_TOLERANCE_MS: i64 = 1_000;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct WordLine {
    pub start: i64,
    pub end: i64,
    /// Background vocals render under the preceding main line.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bg: bool,
    pub words: Vec<Word>,
}

/// `[start, end, text]` keeps the JSON compact; a long song has hundreds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Word(pub i64, pub i64, pub String);

impl WordLine {
    fn new(start: i64, end: i64, words: Vec<Word>) -> Self {
        Self {
            start,
            end,
            bg: false,
            words,
        }
    }

    pub fn text(&self) -> String {
        self.words.iter().map(|word| word.2.as_str()).collect()
    }
}

/// Only keep a timeline that really splits lines into timed words; a source
/// that wraps each whole line in one word is just LRC with extra syntax.
fn finish(mut lines: Vec<WordLine>) -> Vec<WordLine> {
    lines.retain(|line| !line.text().trim().is_empty());
    for line in &mut lines {
        drop_cjk_gaps(&mut line.words);
        line.words.retain(|word| !word.2.is_empty());
        line.words.sort_by_key(|word| word.0);
        let words_end = line
            .words
            .iter()
            .map(|word| word.1)
            .max()
            .unwrap_or(line.start);
        line.end = line.end.max(words_end).max(line.start + 1);
    }
    if lines.iter().any(|line| line.words.len() > 1) {
        lines
    } else {
        Vec::new()
    }
}

fn is_cjk(character: char) -> bool {
    matches!(character as u32,
        0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff)
}

/// Some sources mark breaths with spaces between Chinese characters
/// (`饮 泣的 你`), which plain lyrics never show. Spaces between Latin words
/// are real and stay.
fn drop_cjk_gaps(words: &mut [Word]) {
    for index in 0..words.len() {
        let text = &words[index].2;
        let trimmed = text.trim_end();
        if trimmed.len() == text.len() {
            continue;
        }
        let before = trimmed.chars().last().or_else(|| {
            words[..index]
                .iter()
                .rev()
                .find_map(|word| word.2.trim_end().chars().last())
        });
        let after = words[index + 1..]
            .iter()
            .find_map(|word| word.2.trim_start().chars().next());
        if before.is_some_and(is_cjk) && after.is_some_and(is_cjk) {
            words[index].2 = trimmed.to_string();
        }
    }
}

/// `[start,duration]` line header shared by YRC, QRC and KRC.
fn line_header(line: &str) -> Option<(i64, i64, &str)> {
    let (header, body) = line.trim().strip_prefix('[')?.split_once(']')?;
    let (start, duration) = header.split_once(',')?;
    let start = start.trim().parse::<i64>().ok()?;
    let duration = duration.trim().parse::<i64>().ok()?;
    (start >= 0 && duration >= 0).then_some((start, start + duration, body))
}

/// NetEase YRC: `[start,duration](wordStart,wordDuration,0)word…`.
pub fn parse_yrc(value: &str) -> Vec<WordLine> {
    let marker = regex::Regex::new(r"\((\d+),(\d+),-?\d+\)").unwrap();
    let lines = value
        .lines()
        .filter_map(|line| {
            let (start, end, body) = line_header(line)?;
            let markers = marker.captures_iter(body).collect::<Vec<_>>();
            let words = markers
                .iter()
                .enumerate()
                .filter_map(|(index, captures)| {
                    let whole = captures.get(0)?;
                    let text_end = markers
                        .get(index + 1)
                        .and_then(|next| next.get(0))
                        .map_or(body.len(), |next| next.start());
                    let word_start = captures[1].parse::<i64>().ok()?;
                    let duration = captures[2].parse::<i64>().ok()?;
                    Some(Word(
                        word_start,
                        word_start + duration,
                        body[whole.end()..text_end].to_string(),
                    ))
                })
                .collect();
            Some(WordLine::new(start, end, words))
        })
        .collect();
    finish(lines)
}

/// QQ Music QRC: `[start,duration]word(wordStart,wordDuration)…`; the text
/// comes before its timing. `value` is the decrypted lyric content.
pub fn parse_qrc(value: &str) -> Vec<WordLine> {
    let marker = regex::Regex::new(r"\((\d+),(\d+)\)").unwrap();
    let lines = value
        .lines()
        .filter_map(|line| {
            let (start, end, body) = line_header(line)?;
            let mut text_start = 0;
            let mut words = Vec::new();
            for captures in marker.captures_iter(body) {
                let whole = captures.get(0)?;
                let word_start = captures[1].parse::<i64>().ok()?;
                let duration = captures[2].parse::<i64>().ok()?;
                words.push(Word(
                    word_start,
                    word_start + duration,
                    body[text_start..whole.start()].to_string(),
                ));
                text_start = whole.end();
            }
            Some(WordLine::new(start, end, words))
        })
        .collect();
    finish(lines)
}

/// Kugou KRC: `[start,duration]<offset,duration,0>word…`; offsets are
/// relative to the line start.
pub fn parse_krc(value: &str) -> Vec<WordLine> {
    let marker = regex::Regex::new(r"<(\d+),(\d+),-?\d+>([^<]*)").unwrap();
    let lines = value
        .lines()
        .filter_map(|line| {
            let (start, end, body) = line_header(line)?;
            let words = marker
                .captures_iter(body)
                .filter_map(|captures| {
                    let offset = captures[1].parse::<i64>().ok()?;
                    let duration = captures[2].parse::<i64>().ok()?;
                    Some(Word(
                        start + offset,
                        start + offset + duration,
                        captures[3].to_string(),
                    ))
                })
                .collect();
            Some(WordLine::new(start, end, words))
        })
        .collect();
    finish(lines)
}

/// Kuwo LRCX: `[mm:ss.xxx]<a,b>word…`. With the octal `[kuwo:N]` scale the
/// word offset is `(a+b)/(2·N/10)` and its duration `(a-b)/(2·N%10)`.
/// All-zero markers mark translation or romanization lines, which are skipped.
pub fn parse_kuwo(value: &str) -> Vec<WordLine> {
    let scale = regex::Regex::new(r"(?i)\[kuwo:([0-7]+)\]").unwrap();
    let timestamp = regex::Regex::new(r"^\[(\d{1,3}):([0-5]\d)(?:[.:](\d{1,3}))?\]").unwrap();
    let marker = regex::Regex::new(r"<(-?\d+),(-?\d+)>([^<]*)").unwrap();
    let (begin_divisor, duration_divisor) = value
        .lines()
        .find_map(|line| scale.captures(line))
        .and_then(|captures| i64::from_str_radix(&captures[1], 8).ok())
        .map(|encoded| (encoded / 10 * 2, encoded % 10 * 2))
        .filter(|(begin, duration)| *begin > 0 && *duration > 0)
        .unwrap_or((2, 2));
    let mut lines: Vec<WordLine> = Vec::new();
    for line in value.lines() {
        let line = line.trim();
        let Some(captures) = timestamp.captures(line) else {
            continue;
        };
        let fraction = captures.get(3).map_or("", |value| value.as_str());
        let fraction = fraction.parse::<i64>().unwrap_or(0) * 10_i64.pow(3 - fraction.len() as u32);
        let start = captures[1].parse::<i64>().unwrap_or(0) * 60_000
            + captures[2].parse::<i64>().unwrap_or(0) * 1_000
            + fraction;
        let payload = &line[captures.get(0).unwrap().end()..];
        let markers = marker
            .captures_iter(payload)
            .filter_map(|captures| {
                let first = captures[1].parse::<i64>().ok()?;
                let second = captures[2].parse::<i64>().ok()?;
                Some((first, second, captures[3].to_string()))
            })
            .collect::<Vec<_>>();
        if markers
            .iter()
            .all(|(first, second, _)| *first == 0 && *second == 0)
        {
            continue;
        }
        let words = markers
            .into_iter()
            .filter_map(|(first, second, text)| {
                let offset = (first + second).div_euclid(begin_divisor);
                let duration = (first - second).div_euclid(duration_divisor);
                (duration > 0 && offset >= 0 && !text.trim().is_empty())
                    .then(|| Word(start + offset, start + offset + duration, text))
            })
            .collect::<Vec<_>>();
        if let Some(previous) = lines.last_mut()
            && previous.end <= previous.start + 1
        {
            previous.end = start;
        }
        lines.push(WordLine::new(start, start, words));
    }
    finish(lines)
}

/// Apple Music syllable TTML: `<p begin end><span begin end>word</span>…`,
/// with background vocals in `ttm:role="x-bg"` spans. Line-timed TTML has no
/// timed spans and yields an empty timeline.
pub fn parse_ttml(value: &str, parse_time: impl Fn(&str) -> u64) -> Vec<WordLine> {
    let Ok(document) = roxmltree::Document::parse(value) else {
        return Vec::new();
    };
    let time = |node: roxmltree::Node<'_, '_>, name: &str| {
        node.attribute(name)
            .filter(|value| !value.trim().is_empty())
            .map(|value| parse_time(value) as i64)
    };
    let mut lines = Vec::new();
    for paragraph in document.descendants().filter(|node| node.has_tag_name("p")) {
        let (Some(start), Some(end)) = (time(paragraph, "begin"), time(paragraph, "end")) else {
            continue;
        };
        let mut main = Vec::new();
        let mut background = Vec::new();
        collect_ttml_words(paragraph, false, &mut main, &mut background, &time);
        for (words, bg) in [(main, false), (background, true)] {
            if words.is_empty() {
                continue;
            }
            let line_start = if bg { words[0].0 } else { start };
            let line_end = if bg {
                words.iter().map(|word| word.1).max().unwrap_or(end)
            } else {
                end
            };
            lines.push(WordLine {
                start: line_start,
                end: line_end,
                bg,
                words,
            });
        }
    }
    let mut lines = finish(lines);
    // Background voices keep their word timing, but trim pretty-print padding.
    for line in lines.iter_mut().filter(|line| line.bg) {
        if let Some(first) = line.words.first_mut() {
            first.2 = first.2.trim_start().to_string();
        }
    }
    lines
}

fn collect_ttml_words(
    node: roxmltree::Node<'_, '_>,
    background: bool,
    main: &mut Vec<Word>,
    secondary: &mut Vec<Word>,
    time: &impl Fn(roxmltree::Node<'_, '_>, &str) -> Option<i64>,
) {
    for child in node.children() {
        let target = if background {
            &mut *secondary
        } else {
            &mut *main
        };
        if child.is_text() {
            // A literal space between spans separates words; indentation
            // from pretty-printed documents does not.
            let text = child.text().unwrap_or_default();
            if !text.contains('\n')
                && text.trim().is_empty()
                && let Some(last) = target.last_mut()
            {
                last.2.push_str(text);
            }
            continue;
        }
        if !child.is_element() {
            continue;
        }
        let role = child
            .attributes()
            .iter()
            .find(|attribute| attribute.name() == "role")
            .map(|attribute| attribute.value())
            .unwrap_or_default();
        if matches!(role, "x-translation" | "x-roman") {
            continue;
        }
        let child_background = background || role == "x-bg";
        let has_timed_children = child
            .descendants()
            .skip(1)
            .any(|item| item.has_tag_name("span") && item.attribute("begin").is_some());
        if child.has_tag_name("span")
            && !has_timed_children
            && let (Some(start), Some(end)) = (time(child, "begin"), time(child, "end"))
        {
            let target = if child_background {
                &mut *secondary
            } else {
                &mut *main
            };
            let text = child
                .descendants()
                .filter(|item| item.is_text())
                .filter_map(|item| item.text())
                .collect::<String>();
            if !text.trim().is_empty() && end > start {
                target.push(Word(start, end, text));
            }
            continue;
        }
        collect_ttml_words(child, child_background, main, secondary, time);
    }
}

/// Main-vocal lines as LRC, for the line-synchronized `plain` field.
pub fn to_lrc(lines: &[WordLine]) -> String {
    lines
        .iter()
        .filter(|line| !line.bg)
        .map(|line| {
            let start = line.start.max(0);
            format!(
                "[{:02}:{:02}.{:03}]{}",
                start / 60_000,
                start / 1_000 % 60,
                start % 1_000,
                line.text().trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a word timeline belongs to the same recording as `lines`
/// (`(start, text)` rows of the native LRC): most native lines must have an
/// online line that starts within a second and shares most of its text.
pub fn aligns_with(words: &[WordLine], lines: &[(i64, String)]) -> bool {
    let lines = lines
        .iter()
        .map(|(start, text)| (*start, comparable(text)))
        .filter(|(_, text)| !text.is_empty())
        .collect::<Vec<_>>();
    if words.is_empty() || lines.is_empty() {
        return false;
    }
    let online = words
        .iter()
        .filter(|line| !line.bg)
        .map(|line| (line.start, comparable(&line.text())))
        .collect::<Vec<_>>();
    let aligned = lines
        .iter()
        .filter(|(start, text)| {
            online.iter().any(|(candidate, candidate_text)| {
                (candidate - start).abs() <= ALIGN_TOLERANCE_MS && similar(text, candidate_text)
            })
        })
        .count();
    aligned as f64 >= lines.len() as f64 * MIN_ALIGNED_SHARE
}

fn comparable(text: &str) -> Vec<char> {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Character overlap rather than equality: sources differ in punctuation,
/// full-width forms and occasionally in simplified/traditional script.
fn similar(left: &[char], right: &[char]) -> bool {
    if left == right {
        return true;
    }
    let mut remaining = right.to_vec();
    let shared = left
        .iter()
        .filter(|character| {
            remaining
                .iter()
                .position(|candidate| candidate == *character)
                .map(|index| remaining.swap_remove(index))
                .is_some()
        })
        .count();
    shared * 2 >= left.len().max(right.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(start: i64, end: i64, text: &str) -> Word {
        Word(start, end, text.to_string())
    }

    #[test]
    fn parses_yrc_words_and_skips_metadata() {
        let yrc = "{\"t\":0,\"c\":[{\"tx\":\"作词: \"}]}\n[1000,2000](1000,700,0)Hello (1700,500,0)(world)";
        let lines = parse_yrc(yrc);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].start, 1000);
        assert_eq!(lines[0].end, 3000);
        assert_eq!(
            lines[0].words,
            vec![word(1000, 1700, "Hello "), word(1700, 2200, "(world)")]
        );
    }

    #[test]
    fn drops_breath_spaces_between_chinese_characters_only() {
        let lines = parse_yrc(
            "[0,3000](0,500,0)饮 (500,500,0)泣(1000,100,0) (1100,500,0)的\n[3000,1000](3000,500,0)Hello (3500,500,0)world",
        );
        assert_eq!(lines[0].text(), "饮泣的");
        assert_eq!(lines[0].words.len(), 3);
        assert_eq!(lines[1].text(), "Hello world");
    }

    #[test]
    fn parses_qrc_text_before_timing() {
        let lines = parse_qrc("[1000,2000]你(1000,700)好(1700,300)\n[3000,500]世界(3000,500)");
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0].words,
            vec![word(1000, 1700, "你"), word(1700, 2000, "好")]
        );
        assert_eq!(lines[1].text(), "世界");
    }

    #[test]
    fn parses_krc_relative_offsets() {
        let lines = parse_krc("[id:1]\n[1000,2000]<0,1000,0>你<1000,800,0>好");
        assert_eq!(
            lines[0].words,
            vec![word(1000, 2000, "你"), word(2000, 2800, "好")]
        );
    }

    #[test]
    fn parses_kuwo_scaled_markers_and_skips_auxiliary_lines() {
        // Default scale: offset (a+b)/2, duration (a-b)/2.
        let raw =
            "[00:01.000]<300,-300>你<1300,-100>好\n[00:01.000]<0,0>Hello\n[00:03.000]<400,-400>再";
        let lines = parse_kuwo(raw);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0].words,
            vec![word(1000, 1300, "你"), word(1600, 2300, "好")]
        );
        assert_eq!(lines[0].end, 3000);
        assert_eq!(lines[1].words, vec![word(3000, 3400, "再")]);
    }

    #[test]
    fn rejects_line_level_sources() {
        assert!(parse_qrc("[1000,2000]整行(1000,2000)").is_empty());
        let ttml = r#"<tt><body><div><p begin="1.0" end="2.0">Line only</p></div></body></tt>"#;
        assert!(
            parse_ttml(ttml, |value| (value.parse::<f64>().unwrap() * 1000.0)
                as u64)
            .is_empty()
        );
    }

    #[test]
    fn parses_apple_syllable_ttml_with_background_vocals() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata"><body><div>
            <p begin="1.000" end="3.000"><span begin="1.000" end="1.500">Hel</span><span begin="1.500" end="2.000">lo</span> <span begin="2.000" end="2.500">you</span><span ttm:role="x-bg"><span begin="2.500" end="2.800">(oh</span> <span begin="2.800" end="3.000">yeah)</span></span></p>
            <p begin="3.000" end="4.000"><span begin="3.000" end="3.400">Next</span> <span begin="3.400" end="4.000">line</span><span ttm:role="x-translation">翻译</span></p>
        </div></body></tt>"#;
        let lines = parse_ttml(ttml, |value| {
            (value.parse::<f64>().unwrap() * 1000.0).round() as u64
        });
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text(), "Hello you");
        assert_eq!(lines[0].words[1], word(1500, 2000, "lo "));
        assert!(lines[1].bg);
        assert_eq!(lines[1].text(), "(oh yeah)");
        assert_eq!((lines[1].start, lines[1].end), (2500, 3000));
        assert_eq!(lines[2].text(), "Next line");
        assert_eq!(to_lrc(&lines), "[00:01.000]Hello you\n[00:03.000]Next line");
    }

    #[test]
    fn alignment_requires_matching_times_and_text() {
        let words = parse_yrc(
            "[1000,1000](1000,500,0)你(1500,500,0)好\n[3000,1000](3000,500,0)世(3500,500,0)界",
        );
        let native = vec![(1200, "你好！".to_string()), (3100, "世界".to_string())];
        assert!(aligns_with(&words, &native));
        let shifted = vec![(5200, "你好".to_string()), (7100, "世界".to_string())];
        assert!(!aligns_with(&words, &shifted));
        let other_song = vec![(1000, "再见".to_string()), (3000, "明天".to_string())];
        assert!(!aligns_with(&words, &other_song));
    }
}
