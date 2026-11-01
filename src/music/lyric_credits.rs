//! Credit lines ("作词 : 林夕", "Producer: …") that providers embed as timed
//! lyrics. They are moved out of the lyric text so the player can show them
//! once, below the song, the way Apple Music lists songwriters.

use super::provider::MusicLyrics;
use super::word_lyrics::WordLine;
use std::collections::HashSet;

/// Roles are matched by substring, so "作词/作曲" and "Mixing Engineer" count.
const ROLE_KEYWORDS: &[&str] = &[
    "作词",
    "作曲",
    "填词",
    "词曲",
    "编曲",
    "制作",
    "监制",
    "出品",
    "发行",
    "版权",
    "企划",
    "统筹",
    "策划",
    "和声",
    "和音",
    "合声",
    "配唱",
    "人声",
    "录音",
    "混音",
    "缩混",
    "母带",
    "吉他",
    "贝斯",
    "鼓",
    "键盘",
    "钢琴",
    "弦乐",
    "小提琴",
    "大提琴",
    "管乐",
    "打击乐",
    "乐队",
    "原唱",
    "演唱",
    "翻唱",
    "伴奏",
    "OP",
    "SP",
    "ISRC",
    "lyric",
    "lyricist",
    "written",
    "writer",
    "compose",
    "arrange",
    "produc",
    "coordinat",
    "mix",
    "master",
    "record",
    "engineer",
    "vocal",
    "guitar",
    "bass",
    "drum",
    "keyboard",
    "piano",
    "string",
    "publish",
    "label",
    "percussion",
    "programming",
    "conduct",
];
const MAX_ROLE_CHARS: usize = 28;

/// Verb stems of English "Mixed by …" / "Vocals Recorded at …" credits,
/// which carry no colon.
const ENGLISH_CREDIT_VERBS: &[&str] = &[
    "written", "lyrics", "music", "compos", "arrang", "produc", "record", "mix", "master",
    "engineer", "conduct", "program",
];

/// `role：names` when `text` is a credit line, otherwise `None`.
pub(super) fn credit_of(text: &str) -> Option<String> {
    let text = text.trim().trim_end_matches([',', '，']).trim_end();
    if let Some(credit) = english_by_credit(text) {
        return Some(credit);
    }
    let split = text.find([':', '：'])?;
    let (role, names) = text.split_at(split);
    let role = role.trim();
    let names = names[names.chars().next()?.len_utf8()..].trim();
    if role.is_empty() || names.is_empty() || role.chars().count() > MAX_ROLE_CHARS {
        return None;
    }
    let lower = role.to_lowercase();
    // QQ Music writes bare "词：" / "曲：", too short to match as substrings.
    let is_role = matches!(role, "词" | "曲" | "编曲" | "制作")
        || ROLE_KEYWORDS.iter().any(|keyword| {
            // Short ASCII abbreviations must be whole words ("OP", not "stop").
            if keyword.len() <= 4
                && keyword.is_ascii()
                && keyword.chars().all(|c| c.is_ascii_uppercase())
            {
                role.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|word| word == *keyword)
            } else {
                lower.contains(&keyword.to_lowercase())
            }
        });
    is_role.then(|| format!("{role}：{names}"))
}

/// "Strings Arranged & Conducted by X", "Vocals Recorded at Y": the words
/// before " by "/" at " must end in a credit verb, so "standing by" or
/// "mixed up by you" stay lyrics.
fn english_by_credit(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let split = [" by ", " at "]
        .iter()
        .filter_map(|separator| lower.find(separator))
        .min()?;
    let head = lower[..split].trim();
    let last = head.split_whitespace().last()?;
    let names = text[split + 4..].trim();
    let is_credit = head.split_whitespace().count() <= 6
        && !names.is_empty()
        && ENGLISH_CREDIT_VERBS
            .iter()
            .any(|verb| last.starts_with(verb));
    is_credit.then(|| text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Move credit lines from every lyric track into `lyrics.credits`. Translation
/// and romanization lines at a removed line's timestamp go with it.
pub(super) fn extract(lyrics: &mut MusicLyrics) {
    let mut credits = Vec::new();
    let mut removed = HashSet::new();

    lyrics.plain = strip_lrc(&lyrics.plain, &mut credits, &mut removed);
    lyrics.words.retain(|line| {
        let Some(credit) = credit_of(&word_line_text(line)) else {
            return true;
        };
        credits.push(credit);
        removed.insert(line.start / 10);
        false
    });
    for track in [&mut lyrics.translation, &mut lyrics.roman] {
        *track = drop_lrc_times(track, &removed);
    }

    // LRC and word-timed tracks repeat the same credits, sometimes with
    // full-width punctuation; keep the first spelling of each.
    let mut seen = lyrics
        .credits
        .iter()
        .map(|credit| dedupe_key(credit))
        .collect::<HashSet<_>>();
    for credit in credits {
        if seen.insert(dedupe_key(&credit)) {
            lyrics.credits.push(credit);
        }
    }
}

fn dedupe_key(credit: &str) -> String {
    credit
        .chars()
        .filter_map(|c| match c {
            '（' => Some('('),
            '）' => Some(')'),
            '：' => Some(':'),
            '，' => Some(','),
            '／' => Some('/'),
            c if c.is_whitespace() => None,
            c => Some(c.to_ascii_lowercase()),
        })
        .collect()
}

fn word_line_text(line: &WordLine) -> String {
    line.words.iter().map(|word| word.2.as_str()).collect()
}

/// Removes credit LRC lines, recording their times (in centiseconds, the
/// coarsest LRC precision) so sibling tracks can follow.
fn strip_lrc(text: &str, credits: &mut Vec<String>, removed: &mut HashSet<i64>) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    let mut kept = Vec::new();
    for line in text.lines() {
        let (times, body) = lrc_times(line);
        if !times.is_empty()
            && let Some(credit) = credit_of(body)
        {
            credits.push(credit);
            removed.extend(times);
            continue;
        }
        kept.push(line);
    }
    kept.join("\n")
}

fn drop_lrc_times(text: &str, removed: &HashSet<i64>) -> String {
    if removed.is_empty() || text.trim().is_empty() {
        return text.to_string();
    }
    text.lines()
        .filter(|line| {
            let (times, _) = lrc_times(line);
            times.is_empty() || !times.iter().all(|time| removed.contains(time))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Leading `[mm:ss.xx]` tags in centiseconds, and the text after them.
fn lrc_times(line: &str) -> (Vec<i64>, &str) {
    let mut rest = line.trim_start();
    let mut times = Vec::new();
    while let Some(tag) = rest.strip_prefix('[') {
        let Some(close) = tag.find(']') else { break };
        let Some(time) = parse_time(&tag[..close]) else {
            break;
        };
        times.push(time);
        rest = &tag[close + 1..];
    }
    (times, rest)
}

fn parse_time(tag: &str) -> Option<i64> {
    let (minutes, seconds) = tag.split_once(':')?;
    let minutes = minutes.trim().parse::<i64>().ok()?;
    let (whole, fraction) = seconds.split_once(['.', ':']).unwrap_or((seconds, "0"));
    let whole = whole.trim().parse::<i64>().ok()?;
    let fraction = fraction.trim();
    if fraction.is_empty() || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let millis = format!("{fraction:0<3}")[..3].parse::<i64>().ok()?;
    Some((minutes * 60_000 + whole * 1_000 + millis) / 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::word_lyrics::parse_yrc;

    #[test]
    fn recognises_credit_roles_but_not_dialogue() {
        assert_eq!(credit_of(" 作词 : 林夕").as_deref(), Some("作词：林夕"));
        assert_eq!(
            credit_of("Mixing Engineer: Someone").as_deref(),
            Some("Mixing Engineer：Someone")
        );
        assert_eq!(credit_of("OP：Universal").as_deref(), Some("OP：Universal"));
        assert_eq!(credit_of("词：周杰伦").as_deref(), Some("词：周杰伦"));
        assert_eq!(
            credit_of("Mixed by  Simon Li at nOiz").as_deref(),
            Some("Mixed by Simon Li at nOiz")
        );
        assert_eq!(credit_of("Written by"), None);
        assert_eq!(credit_of("Mixed bytes of love"), None);
        assert_eq!(
            credit_of("Strings Arranged & Conducted by 陈珀").as_deref(),
            Some("Strings Arranged & Conducted by 陈珀")
        );
        assert_eq!(
            credit_of("Vocals Recorded at Avon Studios,").as_deref(),
            Some("Vocals Recorded at Avon Studios")
        );
        assert_eq!(
            credit_of("Keyboards & Programming: 陈珀").as_deref(),
            Some("Keyboards & Programming：陈珀")
        );
        assert!(credit_of("Production Coordination: Stanley Leung").is_some());
        assert_eq!(credit_of("Standing by the window"), None);
        assert_eq!(credit_of("I was mixed up by you"), None);
        assert_eq!(credit_of("歌词：不是署名的一句歌"), None);
        assert_eq!(credit_of("我说：别走"), None);
        assert_eq!(credit_of("stop: now"), None);
        assert_eq!(credit_of("作词："), None);
        assert_eq!(credit_of("拦路雨偏似雪花 饮泣的你冻吗"), None);
    }

    #[test]
    fn moves_netease_credit_lines_out_of_every_track() {
        let mut lyrics = MusicLyrics {
            plain: "[00:00.000] 作词 : 林夕\n[00:00.675] 作曲 : Christopher Chak\n[00:01.000]Drums : Davy Chan (陈匡荣)\n[00:21.961]拦路雨偏似雪花\n[04:10.00]母带 : 某人".to_string(),
            translation: "[00:00.000]//\n[00:21.961]Rain like snow".to_string(),
            words: parse_yrc("[0,1000](0,1000,0) 作词 : 林夕\n[1000,900](1000,900,0)Drums : Davy Chan （陈匡荣）\n[22180,3000](22180,500,0)拦(22680,500,0)路"),
            roman: "[00:00.675]\n[00:21.961]lan lou".to_string(),
            credits: vec!["歌词贡献者：某用户".to_string()],
        };
        extract(&mut lyrics);
        assert_eq!(lyrics.plain, "[00:21.961]拦路雨偏似雪花");
        assert_eq!(lyrics.translation, "[00:21.961]Rain like snow");
        assert_eq!(lyrics.roman, "[00:21.961]lan lou");
        assert_eq!(lyrics.words.len(), 1);
        assert_eq!(
            lyrics.credits,
            vec![
                "歌词贡献者：某用户",
                "作词：林夕",
                "作曲：Christopher Chak",
                "Drums：Davy Chan (陈匡荣)",
                "母带：某人"
            ]
        );
    }

    #[test]
    fn leaves_lyrics_without_credits_untouched() {
        let original = MusicLyrics {
            plain: "[00:01.00]Hello\n[00:02.00]World".to_string(),
            ..Default::default()
        };
        let mut lyrics = original.clone();
        extract(&mut lyrics);
        assert_eq!(lyrics, original);
    }
}
