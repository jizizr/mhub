use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// A music platform MHub can search and download from. The serialized form is
/// the stable `id()`, which is also the `platform` column of `music_cache`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Platform {
    #[serde(rename = "applemusic")]
    AppleMusic,
    #[serde(rename = "bilibili")]
    Bilibili,
    #[serde(rename = "kugou")]
    Kugou,
    #[serde(rename = "netease")]
    Netease,
    #[serde(rename = "soda")]
    Soda,
    #[serde(rename = "tencent")]
    #[default]
    Tencent,
    #[serde(rename = "youtubemusic")]
    YouTubeMusic,
}

impl Platform {
    pub const ALL: [Platform; 7] = [
        Platform::AppleMusic,
        Platform::Bilibili,
        Platform::Kugou,
        Platform::Netease,
        Platform::Soda,
        Platform::Tencent,
        Platform::YouTubeMusic,
    ];

    /// Platforms taken offline. They still parse, so stored settings, caches
    /// and playlists keep deserializing, but MHub no longer lists, searches or
    /// plays them. Soda: since 2026-11 its full tracks need the official
    /// client's native request signature, so only previews were reachable.
    /// Kugou: anonymous play info answers 需要付费 for licensed songs (10 of
    /// 10 hits on 2026-11-08) and its search API mostly returns nothing; an
    /// account means the Kugou Concept login and device verification flow.
    pub fn is_offline(self) -> bool {
        matches!(self, Platform::Soda | Platform::Kugou)
    }

    /// `ALL` without the offline platforms: what MHub offers.
    pub fn online() -> impl Iterator<Item = Platform> {
        Self::ALL
            .into_iter()
            .filter(|platform| !platform.is_offline())
    }

    pub fn id(self) -> &'static str {
        match self {
            Platform::AppleMusic => "applemusic",
            Platform::Bilibili => "bilibili",
            Platform::Kugou => "kugou",
            Platform::Netease => "netease",
            Platform::Soda => "soda",
            Platform::Tencent => "tencent",
            Platform::YouTubeMusic => "youtubemusic",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Platform::AppleMusic => "Apple Music",
            Platform::Bilibili => "Bilibili",
            Platform::Kugou => "酷狗音乐",
            Platform::Netease => "网易云音乐",
            Platform::Soda => "汽水音乐",
            Platform::Tencent => "QQ音乐",
            Platform::YouTubeMusic => "YouTube Music",
        }
    }

    /// Every name `from_alias` accepts, `id()` first.
    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Platform::AppleMusic => &[
                "applemusic",
                "am",
                "apple",
                "apple-music",
                "apple_music",
                "苹果音乐",
            ],
            Platform::Bilibili => &["bilibili", "b", "bili", "哔哩哔哩", "b站"],
            Platform::Kugou => &["kugou", "kg", "酷狗", "酷狗音乐"],
            Platform::Netease => &["netease", "163", "wyy", "网易", "网易云", "网易云音乐"],
            Platform::Soda => &["soda", "qs", "qishui", "汽水", "汽水音乐"],
            Platform::Tencent => &[
                "tencent",
                "qq",
                "qqmusic",
                "tx",
                "qq音乐",
                "腾讯",
                "腾讯音乐",
            ],
            Platform::YouTubeMusic => &[
                "youtubemusic",
                "yt",
                "ytm",
                "youtube",
                "youtube-music",
                "youtube_music",
            ],
        }
    }

    /// Whether the platform can serve lyrics at all: Bilibili and Kugou have
    /// no lyrics source, so offering them a lyrics button would always end in
    /// an empty answer. Keep in sync with MHub's provider implementations.
    pub fn supports_lyrics(self) -> bool {
        !matches!(self, Platform::Bilibili | Platform::Kugou)
    }

    /// Whether playlist/album links can be opened. Keep in sync with MHub's
    /// provider implementations.
    pub fn supports_collections(self) -> bool {
        matches!(self, Platform::AppleMusic | Platform::Netease)
    }

    pub fn from_alias(alias: &str) -> Option<Self> {
        let alias = alias.trim().to_lowercase();
        Self::ALL
            .into_iter()
            .find(|platform| platform.aliases().contains(&alias.as_str()))
    }

    /// One-letter code for size-limited payloads such as Telegram callbacks.
    pub fn callback_code(self) -> &'static str {
        match self {
            Platform::AppleMusic => "a",
            Platform::Bilibili => "b",
            Platform::Kugou => "k",
            Platform::Netease => "n",
            Platform::Soda => "s",
            Platform::Tencent => "t",
            Platform::YouTubeMusic => "y",
        }
    }

    pub fn from_callback_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|platform| platform.callback_code() == code)
    }

    pub fn from_stored_default(value: &str) -> Self {
        Self::from_alias(value).unwrap_or_default()
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

impl FromStr for Platform {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_alias(value).ok_or_else(|| format!("暂不支持音乐平台：{value}"))
    }
}

#[cfg(test)]
mod tests {
    use super::Platform;

    #[test]
    fn soda_is_offline_but_still_parses() {
        assert!(Platform::Soda.is_offline() && Platform::Kugou.is_offline());
        assert_eq!(Platform::from_alias("汽水"), Some(Platform::Soda));
        assert_eq!(Platform::from_alias("酷狗"), Some(Platform::Kugou));
        assert!(Platform::online().all(|platform| !platform.is_offline()));
        assert_eq!(Platform::online().count(), Platform::ALL.len() - 2);
    }

    #[test]
    fn ids_callback_codes_and_serde_round_trip() {
        for platform in Platform::ALL {
            assert_eq!(Platform::from_alias(platform.id()), Some(platform));
            assert_eq!(platform.aliases()[0], platform.id());
            assert_eq!(
                Platform::from_callback_code(platform.callback_code()),
                Some(platform)
            );
            let json = serde_json::to_string(&platform).unwrap();
            assert_eq!(json, format!("\"{}\"", platform.id()));
            assert_eq!(serde_json::from_str::<Platform>(&json).unwrap(), platform);
        }
    }

    #[test]
    fn aliases_are_case_insensitive_and_unique() {
        assert_eq!(Platform::from_alias(" QQ "), Some(Platform::Tencent));
        assert_eq!(Platform::from_alias("B站"), Some(Platform::Bilibili));
        assert_eq!(Platform::from_alias("YTM"), Some(Platform::YouTubeMusic));
        assert_eq!(Platform::from_alias("spotify"), None);
        let mut seen = std::collections::HashSet::new();
        for platform in Platform::ALL {
            for alias in platform.aliases() {
                assert!(seen.insert(*alias), "duplicate alias {alias}");
            }
        }
    }

    #[test]
    fn unknown_stored_default_falls_back_to_tencent() {
        assert_eq!(Platform::from_stored_default(""), Platform::Tencent);
        assert_eq!(
            Platform::from_stored_default("applemusic"),
            Platform::AppleMusic
        );
    }
}
