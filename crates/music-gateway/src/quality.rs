use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// A listening tier. Providers map it to their own levels and fall back to the
/// nearest tier a song actually has; `as_str()` is the `quality` column of
/// `music_cache`, so it must never change for an existing variant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Quality {
    #[serde(rename = "standard")]
    Standard,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "lossless")]
    Lossless,
    #[serde(rename = "hires")]
    HiRes,
}

impl Quality {
    pub const ALL: [Quality; 4] = [
        Quality::Standard,
        Quality::High,
        Quality::Lossless,
        Quality::HiRes,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Standard => "standard",
            Quality::High => "high",
            Quality::Lossless => "lossless",
            Quality::HiRes => "hires",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Quality::Standard => "标准",
            Quality::High => "高品质",
            Quality::Lossless => "无损",
            Quality::HiRes => "Hi-Res",
        }
    }

    /// Accepts the canonical names plus the spellings providers historically
    /// understood (`flac`, `无损`, `hi-res`, `128`, …).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "standard" | "std" | "128" => Some(Quality::Standard),
            "high" | "exhigh" | "320" => Some(Quality::High),
            "lossless" | "flac" | "无损" => Some(Quality::Lossless),
            "hires" | "hi-res" | "master" => Some(Quality::HiRes),
            _ => None,
        }
    }
}

impl fmt::Display for Quality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Quality {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value).ok_or_else(|| format!("不支持的音质：{value}"))
    }
}

#[cfg(test)]
mod tests {
    use super::Quality;

    #[test]
    fn canonical_names_round_trip_through_parse_and_serde() {
        for quality in Quality::ALL {
            assert_eq!(Quality::parse(quality.as_str()), Some(quality));
            let json = serde_json::to_string(&quality).unwrap();
            assert_eq!(json, format!("\"{}\"", quality.as_str()));
        }
        assert_eq!(Quality::parse(" FLAC "), Some(Quality::Lossless));
        assert_eq!(Quality::parse("Hi-Res"), Some(Quality::HiRes));
        assert_eq!(Quality::parse("best"), None);
    }
}
