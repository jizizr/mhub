//! Cross-platform song matching for playlist import.
//!
//! A source track is compared with a target platform's search results by
//! title, artists and *version* (Live, remix, instrumental, ...). Titles and
//! names are folded first (case, full-width forms, Traditional -> Simplified
//! Chinese), so `陳奕迅` and `陈奕迅` or `Ｌｉｖｅ` and `live` compare equal.

use std::{collections::HashMap, sync::OnceLock};

/// Single-character Traditional/variant -> Simplified pairs generated from
/// OpenCC's dictionaries by `scripts/gen_zh_fold.mjs`.
const ZH_FOLD: &str = include_str!("data/zh_fold.txt");

fn zh_fold_table() -> &'static HashMap<char, char> {
    static TABLE: OnceLock<HashMap<char, char>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let characters = ZH_FOLD.trim_end().chars().collect::<Vec<_>>();
        characters
            .chunks_exact(2)
            .map(|pair| (pair[0], pair[1]))
            .collect()
    })
}

/// Case, width and script folding shared by every comparison.
fn fold(value: &str) -> String {
    let table = zh_fold_table();
    value
        .chars()
        .map(|character| match character {
            // Full-width ASCII (U+FF01..U+FF5E) and the ideographic space.
            '\u{ff01}'..='\u{ff5e}' => {
                char::from_u32(character as u32 - 0xfee0).unwrap_or(character)
            }
            '\u{3000}' => ' ',
            '‘' | '’' | '`' | '´' => '\'',
            '“' | '”' => '"',
            _ => *table.get(&character).unwrap_or(&character),
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// Letters, digits and CJK only; punctuation and spacing never decide a match.
fn compact(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

/// Recording variants that make two otherwise equal titles different songs
/// to a listener. Remaster/deluxe/explicit labels are deliberately absent:
/// they describe the same performance.
const VERSION_KEYWORDS: &[(&str, &[&str])] = &[
    ("live", &["live", "现场", "演唱会", "concert", "unplugged"]),
    ("remix", &["remix", "rmx", "混音", "mix)"]),
    (
        "instrumental",
        &[
            "instrumental",
            "inst.",
            "inst)",
            "伴奏",
            "纯音乐",
            "karaoke",
            "off vocal",
            "offvocal",
        ],
    ),
    ("acoustic", &["acoustic", "不插电"]),
    ("demo", &["demo"]),
    ("cover", &["cover", "翻唱", "翻自"]),
    ("dj", &["dj版", "dj ", " dj"]),
    ("female", &["女版", "女声版"]),
    ("male", &["男版", "男声版"]),
    (
        "speed",
        &[
            "sped up", "speed up", "加速", "0.8x", "1.2x", "slowed", "降速",
        ],
    ),
    ("clip", &["片段", "试听", "铃声"]),
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct TitleParts {
    /// Title without bracketed or ` - ` suffix annotations.
    base: String,
    /// Sorted version classes found anywhere in the title.
    versions: Vec<&'static str>,
}

fn title_parts(title: &str) -> TitleParts {
    let folded = fold(title);
    let mut base = String::new();
    // Bracketed text and " - " suffixes, each closed with ")" so keywords
    // such as "mix)" only match at the end of an annotation.
    let mut annotations = String::new();
    let mut nesting = 0usize;
    for character in folded.chars() {
        match character {
            '(' | '[' | '{' | '（' | '［' | '｛' | '【' | '〔' | '《' => {
                nesting += 1;
                annotations.push(' ');
            }
            ')' | ']' | '}' | '）' | '］' | '｝' | '】' | '〕' | '》' => {
                nesting = nesting.saturating_sub(1);
                annotations.push(')');
            }
            _ if nesting > 0 => annotations.push(character),
            _ => base.push(character),
        }
    }
    // "Song - Live at Wembley", "Song - Remastered 2011".
    let mut segments = base.split(" - ").flat_map(|part| part.split(" – "));
    let head = segments.next().unwrap_or_default().to_string();
    for suffix in segments {
        annotations.push(' ');
        annotations.push_str(suffix);
        annotations.push(')');
    }
    let mut versions = VERSION_KEYWORDS
        .iter()
        .filter(|(_, keywords)| keywords.iter().any(|keyword| annotations.contains(keyword)))
        .map(|(class, _)| *class)
        .collect::<Vec<_>>();
    versions.sort_unstable();
    let compact_base = compact(&head);
    TitleParts {
        // A title that is nothing but an annotation keeps its full text.
        base: if compact_base.is_empty() {
            compact(&folded)
        } else {
            compact_base
        },
        versions,
    }
}

fn artists(value: &str) -> Vec<String> {
    let folded = fold(value);
    let mut normalized = folded;
    for separator in [
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " featuring ",
        " x ",
        " × ",
        " and ",
        " with ",
    ] {
        normalized = normalized.replace(separator, "/");
    }
    let mut names = normalized
        .split(['&', ',', '，', '、', '/', '|', ';', '；', '+'])
        .map(compact)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

/// Sørensen–Dice similarity over character bigrams, for small title typos.
fn similarity(left: &str, right: &str) -> f32 {
    let bigrams = |value: &str| {
        let characters = value.chars().collect::<Vec<_>>();
        characters
            .windows(2)
            .map(|pair| (pair[0], pair[1]))
            .collect::<Vec<_>>()
    };
    let left = bigrams(left);
    let mut right = bigrams(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let total = (left.len() + right.len()) as f32;
    let mut shared = 0usize;
    for pair in left {
        if let Some(position) = right.iter().position(|candidate| *candidate == pair) {
            right.swap_remove(position);
            shared += 1;
        }
    }
    2.0 * shared as f32 / total
}

#[derive(Debug, Clone)]
pub struct Candidate<T> {
    pub item: T,
    pub score: u8,
    identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Matched,
    Review,
    Missing,
}

pub const MATCH_THRESHOLD: i32 = 88;
pub const REVIEW_THRESHOLD: i32 = 50;
/// The best candidate must beat every *different* recording by this margin.
pub const MATCH_MARGIN: i32 = 8;

/// 0..=100. Title equality (60) + artist overlap (35) + agreeing version (5),
/// minus a penalty when exactly one side is a Live/remix/instrumental/...
pub fn score(source_song: &str, source_singer: &str, song: &str, singer: &str) -> u8 {
    let source = title_parts(source_song);
    let target = title_parts(song);
    if source.base.is_empty() || target.base.is_empty() {
        return 0;
    }
    let mut total = if source.base == target.base {
        60
    } else {
        let (shorter, longer) = if source.base.chars().count() <= target.base.chars().count() {
            (&source.base, &target.base)
        } else {
            (&target.base, &source.base)
        };
        let ratio = shorter.chars().count() as f32 / longer.chars().count() as f32;
        let dice = similarity(&source.base, &target.base);
        if longer.contains(shorter.as_str()) && ratio >= 0.5 {
            42
        } else if dice >= 0.8 {
            45
        } else if dice >= 0.6 {
            30
        } else {
            0
        }
    };

    let source_artists = artists(source_singer);
    let target_artists = artists(singer);
    if source_artists.is_empty() || target_artists.is_empty() {
        // Unknown artist on either side: neither confirm nor reject.
        total += 10;
    } else if source_artists
        .iter()
        .any(|name| target_artists.contains(name))
    {
        total += 35;
    } else if source_artists.iter().any(|left| {
        target_artists.iter().any(|right| {
            let (short, long) = if left.chars().count() <= right.chars().count() {
                (left, right)
            } else {
                (right, left)
            };
            short.chars().count() >= 2 && long.contains(short.as_str())
        })
    }) {
        total += 25;
    }

    let missing = target
        .versions
        .iter()
        .filter(|version| !source.versions.contains(version))
        .count()
        + source
            .versions
            .iter()
            .filter(|version| !target.versions.contains(version))
            .count();
    if missing == 0 {
        if !source.versions.is_empty() {
            total += 5;
        }
    } else {
        total -= (missing as i32 * 25).min(45);
    }
    total.clamp(0, 100) as u8
}

/// Two releases of the same recording (single and album, re-uploads) share
/// this key; their tie must not block an otherwise certain match.
fn identity(song: &str, singer: &str) -> String {
    let parts = title_parts(song);
    format!(
        "{}\0{}\0{}",
        parts.base,
        parts.versions.join(","),
        artists(singer).join(",")
    )
}

/// Rank candidates and decide whether the best one can be imported without
/// asking. Candidates keep their search order among equal scores.
pub fn rank<T>(
    source_song: &str,
    source_singer: &str,
    candidates: Vec<T>,
    describe: impl Fn(&T) -> (&str, &str),
) -> (Decision, u8, Vec<Candidate<T>>) {
    let mut ranked = candidates
        .into_iter()
        .map(|item| {
            let (song, singer) = describe(&item);
            Candidate {
                score: score(source_song, source_singer, song, singer),
                identity: identity(song, singer),
                item,
            }
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.score.cmp(&left.score));
    // Keep the first (best-ranked) release of each recording.
    let mut seen = Vec::<String>::new();
    ranked.retain(|candidate| {
        if seen.contains(&candidate.identity) {
            false
        } else {
            seen.push(candidate.identity.clone());
            true
        }
    });
    let top = ranked.first().map_or(0, |candidate| candidate.score as i32);
    let runner_up = ranked.get(1).map_or(0, |candidate| candidate.score as i32);
    let decision = if top >= MATCH_THRESHOLD && top - runner_up >= MATCH_MARGIN {
        Decision::Matched
    } else if top >= REVIEW_THRESHOLD {
        Decision::Review
    } else {
        Decision::Missing
    };
    (decision, top as u8, ranked)
}

/// A shorter query for a second search when "song artists" found nothing
/// usable: the bare title plus the first artist.
pub fn fallback_query(song: &str, singer: &str) -> Option<String> {
    let base = fold(song)
        .split(['(', '（', '[', '【', '-'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if base.is_empty() {
        return None;
    }
    let artist = singer
        .split(['&', ',', '，', '、', '/', ';'])
        .next()
        .unwrap_or_default()
        .trim();
    let query = format!("{base} {artist}").trim().to_string();
    (query != format!("{song} {singer}").trim()).then_some(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decide(song: &str, singer: &str, candidates: &[(&str, &str)]) -> (Decision, Vec<String>) {
        let (decision, _, ranked) =
            rank(song, singer, candidates.to_vec(), |item| (item.0, item.1));
        (
            decision,
            ranked
                .iter()
                .map(|candidate| candidate.item.0.to_string())
                .collect(),
        )
    }

    #[test]
    fn folds_script_width_and_case() {
        assert_eq!(fold("陳奕迅"), "陈奕迅");
        assert_eq!(fold("後來"), "后来");
        assert_eq!(fold("ＬＩＶＥ"), "live");
        assert_eq!(artists("陳奕迅 feat. 王菲"), ["王菲", "陈奕迅"]);
    }

    #[test]
    fn traditional_and_simplified_names_match_automatically() {
        let (decision, ranked) = decide("後來", "劉若英", &[("后来", "刘若英"), ("后来", "群星")]);
        assert_eq!(decision, Decision::Matched);
        assert_eq!(ranked[0], "后来");
    }

    #[test]
    fn original_beats_live_and_instrumental_versions() {
        let (decision, ranked) = decide(
            "晴天",
            "周杰伦",
            &[
                ("晴天 (Live)", "周杰伦"),
                ("晴天", "周杰伦"),
                ("晴天 (伴奏)", "周杰伦"),
            ],
        );
        assert_eq!(decision, Decision::Matched);
        assert_eq!(ranked[0], "晴天");
    }

    #[test]
    fn a_live_source_prefers_the_live_recording() {
        let (decision, ranked) = decide(
            "Mirrors (Acoustic)",
            "Beth",
            &[("Mirrors", "Beth"), ("Mirrors (Acoustic)", "Beth")],
        );
        assert_eq!(decision, Decision::Matched);
        assert_eq!(ranked[0], "Mirrors (Acoustic)");
    }

    #[test]
    fn duplicate_releases_of_one_recording_do_not_force_review() {
        let (decision, ranked) = decide(
            "最初",
            "逆时针向",
            &[
                ("最初", "逆时针向"),
                ("最初", "逆时针向"),
                ("最初的梦想", "范玮琪"),
            ],
        );
        assert_eq!(decision, Decision::Matched);
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn same_title_by_another_artist_is_never_automatic() {
        let (decision, _) = decide("晴天", "周杰伦", &[("晴天", "何维健")]);
        assert_ne!(decision, Decision::Matched);
        let (decision, _) = decide("最初", "逆时针向", &[("最初", "另一位歌手")]);
        assert_ne!(decision, Decision::Matched);
    }

    #[test]
    fn only_a_different_version_needs_review() {
        let (decision, _) = decide("晴天", "周杰伦", &[("晴天 (Live)", "周杰伦")]);
        assert_eq!(decision, Decision::Review);
    }

    #[test]
    fn unrelated_results_are_missing() {
        let (decision, _) = decide("晴天", "周杰伦", &[("稻香", "周杰伦")]);
        assert_eq!(decision, Decision::Missing);
        let (decision, ranked) = decide("晴天", "周杰伦", &[]);
        assert_eq!(decision, Decision::Missing);
        assert!(ranked.is_empty());
    }

    #[test]
    fn version_words_inside_the_title_are_not_annotations() {
        let (decision, ranked) = decide(
            "Live Forever",
            "Oasis",
            &[("Live Forever (Live)", "Oasis"), ("Live Forever", "Oasis")],
        );
        assert_eq!(decision, Decision::Matched);
        assert_eq!(ranked[0], "Live Forever");
        assert!(score("Yellow - Live", "Coldplay", "Yellow (Live)", "Coldplay") >= 95);
    }

    #[test]
    fn suffix_annotations_and_punctuation_do_not_change_the_title() {
        assert!(score("Yellow - Remastered 2011", "Coldplay", "Yellow", "Coldplay") >= 90);
        assert!(
            score(
                "Love Story (Taylor’s Version)",
                "Taylor Swift",
                "Love Story (Taylor's Version)",
                "Taylor Swift"
            ) >= 90
        );
    }

    #[test]
    fn fallback_query_drops_annotations() {
        assert_eq!(
            fallback_query("晴天 (Live)", "周杰伦/五月天").as_deref(),
            Some("晴天 周杰伦")
        );
        assert_eq!(fallback_query("晴天", "周杰伦"), None);
    }
}
