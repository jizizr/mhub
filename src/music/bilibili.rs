use super::provider::{CLIENT, MusicPlatform, MusicProvider, MusicSearchItem, MusicTrack};
use crate::BotError;
use async_trait::async_trait;
use lazy_static::lazy_static;
use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;

lazy_static! {
    static ref BILIBILI_BV_RE: Regex = Regex::new(r"(?i)(BV[a-zA-Z0-9]{10})").unwrap();
    static ref BILIBILI_TAG_RE: Regex = Regex::new(r"<[^>]+>").unwrap();
    /// Anonymous device cookie (`buvid3`/`buvid4`), fetched once and reused.
    static ref BUVID_COOKIE: tokio::sync::Mutex<String> = tokio::sync::Mutex::new(String::new());
}

/// A full desktop browser User-Agent. Bilibili's risk control answers 412 to
/// a bare `Mozilla/5.0`; without the buvid device cookie it still rejects
/// search some of the time and, from the origin, every view request.
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const REFERER: &str = "https://www.bilibili.com/";
/// The upos CDN answers plain GET and HEAD with HTTP 567 and serves only
/// bounded Range requests (MusicBot-Go `05df3b4` uses the same 4 MiB cap).
const CDN_MAX_CHUNK: u64 = 4 * 1024 * 1024;

fn browser_headers() -> HashMap<String, String> {
    HashMap::from([
        ("User-Agent".to_string(), BROWSER_USER_AGENT.to_string()),
        ("Referer".to_string(), REFERER.to_string()),
    ])
}

async fn get_bilibili_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, BotError> {
    let cookie = buvid_cookie(false).await;
    match get_with_cookie(url, &cookie).await {
        // A device cookie the risk control has started rejecting: mint a
        // fresh one once.
        Err(BilibiliError::PreconditionFailed) => {
            let fresh = buvid_cookie(true).await;
            match get_with_cookie(url, &fresh).await {
                Err(BilibiliError::PreconditionFailed) => Err(BotError::Custom(
                    "Bilibili 风控拒绝了请求（HTTP 412）".to_string(),
                )),
                result => result.map_err(BilibiliError::into_bot),
            }
        }
        result => result.map_err(BilibiliError::into_bot),
    }
}

enum BilibiliError {
    PreconditionFailed,
    Other(BotError),
}

impl BilibiliError {
    fn into_bot(self) -> BotError {
        match self {
            Self::PreconditionFailed => {
                BotError::Custom("Bilibili 风控拒绝了请求（HTTP 412）".to_string())
            }
            Self::Other(err) => err,
        }
    }
}

async fn get_with_cookie<T: serde::de::DeserializeOwned>(
    url: &str,
    cookie: &str,
) -> Result<T, BilibiliError> {
    let mut request = CLIENT
        .get(url)
        .header("User-Agent", BROWSER_USER_AGENT)
        .header("Referer", REFERER);
    if !cookie.is_empty() {
        request = request.header("Cookie", cookie);
    }
    let resp = request
        .send()
        .await
        .map_err(|err| BilibiliError::Other(err.into()))?;
    let status = resp.status();
    if status == reqwest::StatusCode::PRECONDITION_FAILED {
        return Err(BilibiliError::PreconditionFailed);
    }
    if !status.is_success() {
        return Err(BilibiliError::Other(BotError::Custom(format!(
            "Bilibili 接口请求失败：HTTP {status}"
        ))));
    }
    resp.json()
        .await
        .map_err(|err| BilibiliError::Other(err.into()))
}

/// The cached buvid cookie, minting one from the fingerprint endpoint when
/// empty or when `refresh` is set. An empty string means minting failed and
/// the request goes out without it.
async fn buvid_cookie(refresh: bool) -> String {
    let mut cached = BUVID_COOKIE.lock().await;
    if refresh || cached.is_empty() {
        match fetch_buvid_cookie().await {
            Some(cookie) => *cached = cookie,
            None => tracing::warn!("Bilibili buvid cookie could not be fetched"),
        }
    }
    cached.clone()
}

async fn fetch_buvid_cookie() -> Option<String> {
    let resp = CLIENT
        .get("https://api.bilibili.com/x/frontend/finger/spi")
        .header("User-Agent", BROWSER_USER_AGENT)
        .header("Referer", REFERER)
        .send()
        .await
        .ok()?;
    let spi: BilibiliSpiResponse = resp.json().await.ok()?;
    let data = spi.data.filter(|_| spi.code == 0)?;
    buvid_cookie_value(&data.b_3, &data.b_4)
}

fn buvid_cookie_value(buvid3: &str, buvid4: &str) -> Option<String> {
    let buvid3 = buvid3.trim();
    if buvid3.is_empty() {
        return None;
    }
    let buvid4 = buvid4.trim();
    Some(if buvid4.is_empty() {
        format!("buvid3={buvid3}")
    } else {
        format!("buvid3={buvid3}; buvid4={}", urlencoding::encode(buvid4))
    })
}

#[derive(Deserialize)]
struct BilibiliSpiResponse {
    code: i64,
    data: Option<BilibiliSpiData>,
}

#[derive(Deserialize)]
struct BilibiliSpiData {
    #[serde(default)]
    b_3: String,
    #[serde(default)]
    b_4: String,
}

pub static BILIBILI_PROVIDER: BilibiliProvider = BilibiliProvider;

pub struct BilibiliProvider;

#[async_trait]
impl MusicProvider for BilibiliProvider {
    async fn search(&self, keyword: &str, limit: usize) -> Result<Vec<MusicSearchItem>, BotError> {
        let url = format!(
            "https://api.bilibili.com/x/web-interface/search/type?search_type=video&keyword={}&page=1",
            urlencoding::encode(keyword)
        );
        let response: BilibiliSearchResponse = get_bilibili_json(&url).await?;
        ensure_bilibili_ok(response.code, &response.message)?;
        Ok(response
            .data
            .map(|data| data.result)
            .unwrap_or_default()
            .into_iter()
            .take(limit)
            .filter_map(|item| {
                let bvid = item.bvid?;
                Some(MusicSearchItem {
                    platform: MusicPlatform::Bilibili,
                    id: bvid,
                    song: strip_html_tags(&item.title),
                    singer: item.author.unwrap_or_else(|| "未知 UP".to_string()),
                    cover: item
                        .pic
                        .as_deref()
                        .map(normalize_bilibili_image)
                        .unwrap_or_default(),
                })
            })
            .collect())
    }

    async fn resolve(
        &self,
        keyword: &str,
        selected_id: Option<&str>,
    ) -> Result<MusicTrack, BotError> {
        resolve_with_quality(keyword, selected_id, "high").await
    }
}

pub(super) async fn resolve_with_quality(
    keyword: &str,
    selected_id: Option<&str>,
    quality: &str,
) -> Result<MusicTrack, BotError> {
    let bvid = match selected_id {
        Some(id) => id.to_string(),
        None => match parse_bilibili_id(keyword) {
            Some(id) => id,
            None => {
                BILIBILI_PROVIDER
                    .search(keyword, 1)
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| BotError::Custom("没有找到可下载的音乐".to_string()))?
                    .id
            }
        },
    };
    let view = get_video_view(&bvid).await?;
    let audio = get_video_audio(&view.bvid, view.cid, quality).await?;
    Ok(MusicTrack {
        id: view.bvid.clone(),
        platform: MusicPlatform::Bilibili,
        song: view.title,
        singer: view
            .owner
            .map(|owner| owner.name)
            .unwrap_or_else(|| "未知 UP".to_string()),
        album: String::new(),
        cover: normalize_bilibili_image(&view.pic),
        link: format!("https://www.bilibili.com/video/{}", view.bvid),
        url: audio.url,
        headers: browser_headers(),
        max_chunk_size: Some(CDN_MAX_CHUNK),
        replay_gain_centi_db: None,
        served_quality: None,
        duration: view
            .duration
            .filter(|duration| *duration > 0)
            .map(|duration| duration as u32),
        bitrate: audio.bandwidth,
        format: Some("m4a".to_string()),
    })
}

#[derive(Deserialize)]
struct BilibiliSearchResponse {
    code: i64,
    #[serde(default)]
    message: String,
    data: Option<BilibiliSearchData>,
}

#[derive(Deserialize)]
struct BilibiliSearchData {
    #[serde(default)]
    result: Vec<BilibiliSearchItem>,
}

#[derive(Deserialize)]
struct BilibiliSearchItem {
    bvid: Option<String>,
    title: String,
    author: Option<String>,
    pic: Option<String>,
}

#[derive(Deserialize)]
struct BilibiliViewResponse {
    code: i64,
    #[serde(default)]
    message: String,
    data: Option<BilibiliViewData>,
}

#[derive(Deserialize)]
struct BilibiliViewData {
    bvid: String,
    cid: i64,
    title: String,
    pic: String,
    duration: Option<i64>,
    owner: Option<BilibiliOwner>,
}

#[derive(Deserialize)]
struct BilibiliOwner {
    name: String,
}

#[derive(Deserialize)]
struct BilibiliPlayResponse {
    code: i64,
    #[serde(default)]
    message: String,
    data: Option<BilibiliPlayData>,
}

#[derive(Deserialize)]
struct BilibiliPlayData {
    dash: Option<BilibiliDash>,
}

#[derive(Deserialize)]
struct BilibiliDash {
    #[serde(default)]
    audio: Vec<BilibiliDashAudio>,
}

#[derive(Deserialize)]
struct BilibiliDashAudio {
    #[serde(rename = "baseUrl")]
    base_url: Option<String>,
    #[serde(default, rename = "backupUrl")]
    backup_url: Vec<String>,
    bandwidth: Option<i64>,
}

async fn get_video_view(bvid: &str) -> Result<BilibiliViewData, BotError> {
    let url = format!(
        "https://api.bilibili.com/x/web-interface/view?bvid={}",
        urlencoding::encode(bvid)
    );
    let response: BilibiliViewResponse = get_bilibili_json(&url).await?;
    ensure_bilibili_ok(response.code, &response.message)?;
    response
        .data
        .ok_or_else(|| BotError::Custom("没有找到 Bilibili 视频信息".to_string()))
}

struct VideoAudio {
    url: String,
    bandwidth: Option<u32>,
}

/// The bitrate the Standard tier aims for, as on the other platforms.
const STANDARD_TARGET_BPS: i64 = 128_000;

async fn get_video_audio(bvid: &str, cid: i64, quality: &str) -> Result<VideoAudio, BotError> {
    let url = format!(
        "https://api.bilibili.com/x/player/playurl?bvid={}&cid={cid}&qn=16&fnval=16",
        urlencoding::encode(bvid)
    );
    let response: BilibiliPlayResponse = get_bilibili_json(&url).await?;
    ensure_bilibili_ok(response.code, &response.message)?;
    response
        .data
        .and_then(|data| data.dash)
        .and_then(|dash| {
            // AAC only: the Dolby (E-AC-3) and member FLAC tracks are not in
            // `dash.audio` and most browsers cannot play E-AC-3.
            select_audio(dash.audio, quality)
        })
        .and_then(|audio| {
            let bandwidth = audio
                .bandwidth
                .and_then(|bandwidth| u32::try_from(bandwidth).ok())
                .filter(|bandwidth| *bandwidth > 0);
            pick_cdn_url(audio.base_url.into_iter().chain(audio.backup_url))
                .map(|url| VideoAudio { url, bandwidth })
        })
        .ok_or_else(|| BotError::Custom("没有拿到 Bilibili 音频链接".to_string()))
}

/// Standard takes the AAC track nearest 128 kbps (id 30232, ~132k); every
/// other tier takes the best AAC (30280, ~192k). Without a member login the
/// API offers no FLAC, so Lossless and Hi-Res get that best AAC too.
fn select_audio(audio: Vec<BilibiliDashAudio>, quality: &str) -> Option<BilibiliDashAudio> {
    let standard = matches!(
        quality.trim().to_ascii_lowercase().as_str(),
        "standard" | "std" | "128"
    );
    let bandwidth = |audio: &BilibiliDashAudio| audio.bandwidth.unwrap_or_default();
    if standard {
        audio.into_iter().min_by_key(|audio| {
            (
                (bandwidth(audio) - STANDARD_TARGET_BPS).abs(),
                std::cmp::Reverse(bandwidth(audio)),
            )
        })
    } else {
        audio.into_iter().max_by_key(bandwidth)
    }
}

/// Prefers Bilibili's own upos mirrors over Akamai, which MusicBot-Go blocks
/// for unreliable transfers; Akamai is still used when it is the only choice.
fn pick_cdn_url(urls: impl Iterator<Item = String>) -> Option<String> {
    let urls: Vec<String> = urls
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
        .collect();
    urls.iter()
        .find(|url| !is_akamai(url))
        .or_else(|| urls.first())
        .cloned()
}

fn is_akamai(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host.contains("akamai"))
}

fn ensure_bilibili_ok(code: i64, message: &str) -> Result<(), BotError> {
    if code == 0 {
        Ok(())
    } else {
        Err(BotError::Custom(format!(
            "Bilibili API 错误 {code}: {message}"
        )))
    }
}

fn parse_bilibili_id(text: &str) -> Option<String> {
    BILIBILI_BV_RE
        .captures(text.trim())
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
}

fn strip_html_tags(text: &str) -> String {
    BILIBILI_TAG_RE.replace_all(text, "").into_owned()
}

/// Square JPEG at 960 px: covers are shown cropped square, and a JPEG can be
/// embedded in the M4A (MP4 `covr` takes JPEG or PNG, not WebP).
const COVER_TRANSFORM: &str = "@960w_960h_1c.jpg";

/// Points cover art at `s1.hdslb.com`. The `i0`–`i2.hdslb.com` image hosts
/// enforce hotlink protection on some edge nodes (seen from mainland China:
/// HTTP 403 for any foreign Referer, while `no-referrer` loads), whereas the
/// static host serves the same `/bfs/` paths without a Referer check, and its
/// resize suffix also replaces multi-megabyte originals in result lists.
fn normalize_bilibili_image(url: &str) -> String {
    let url = url.trim();
    let absolute = if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_string()
    };
    let Ok(mut parsed) = url::Url::parse(&absolute) else {
        return absolute;
    };
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let is_image_host = host.ends_with(".hdslb.com") || host.ends_with(".biliimg.com");
    if !is_image_host || !parsed.path().starts_with("/bfs/") || parsed.path().contains('@') {
        return absolute;
    }
    let path = format!("{}{COVER_TRANSFORM}", parsed.path());
    parsed.set_path(&path);
    if parsed.set_host(Some("s1.hdslb.com")).is_err() || parsed.set_scheme("https").is_err() {
        return absolute;
    }
    parsed.set_query(None);
    parsed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_upos_mirrors_over_akamai() {
        let urls = [
            "https://upos-hz-mirrorakam.akamaized.net/a.m4s".to_string(),
            " ".to_string(),
            "https://upos-sz-mirrorcosov.bilivideo.com/a.m4s".to_string(),
        ];
        assert_eq!(
            pick_cdn_url(urls.into_iter()).as_deref(),
            Some("https://upos-sz-mirrorcosov.bilivideo.com/a.m4s")
        );
        let only_akamai = ["https://upos-hz-mirrorakam.akamaized.net/a.m4s".to_string()];
        assert_eq!(
            pick_cdn_url(only_akamai.into_iter()).as_deref(),
            Some("https://upos-hz-mirrorakam.akamaized.net/a.m4s")
        );
        assert_eq!(pick_cdn_url(std::iter::empty()), None);
    }

    #[test]
    fn builds_buvid_cookie_with_encoded_buvid4() {
        assert_eq!(
            buvid_cookie_value("ABC-123infoc", "x/y=z").as_deref(),
            Some("buvid3=ABC-123infoc; buvid4=x%2Fy%3Dz")
        );
        assert_eq!(
            buvid_cookie_value("ABC", " ").as_deref(),
            Some("buvid3=ABC")
        );
        assert_eq!(buvid_cookie_value(" ", "x"), None);
    }

    #[test]
    fn serves_covers_from_the_static_host_as_square_jpeg() {
        assert_eq!(
            normalize_bilibili_image("//i0.hdslb.com/bfs/archive/abc.jpg"),
            "https://s1.hdslb.com/bfs/archive/abc.jpg@960w_960h_1c.jpg"
        );
        assert_eq!(
            normalize_bilibili_image("http://i2.hdslb.com/bfs/archive/abc.png"),
            "https://s1.hdslb.com/bfs/archive/abc.png@960w_960h_1c.jpg"
        );
        assert_eq!(
            normalize_bilibili_image("https://archive.biliimg.com/bfs/archive/abc.jpg"),
            "https://s1.hdslb.com/bfs/archive/abc.jpg@960w_960h_1c.jpg"
        );
        // Already transformed, or not a Bilibili image path: left alone.
        let resized = "https://i0.hdslb.com/bfs/archive/abc.jpg@100w.webp";
        assert_eq!(normalize_bilibili_image(resized), resized);
        let other = "https://example.com/bfs/a.jpg";
        assert_eq!(normalize_bilibili_image(other), other);
        assert_eq!(normalize_bilibili_image(""), "");
    }

    #[test]
    fn standard_takes_the_track_nearest_128k_and_others_the_best() {
        let tracks = || {
            [(30216, 46_565), (30232, 102_031), (30280, 184_052)]
                .into_iter()
                .map(|(id, bandwidth)| BilibiliDashAudio {
                    base_url: Some(format!("https://upos-sz-mirrorcos.bilivideo.com/{id}.m4s")),
                    backup_url: Vec::new(),
                    bandwidth: Some(bandwidth),
                })
                .collect::<Vec<_>>()
        };
        let pick = |quality| select_audio(tracks(), quality).unwrap().bandwidth;
        assert_eq!(pick("standard"), Some(102_031));
        for quality in ["high", "lossless", "hires"] {
            assert_eq!(pick(quality), Some(184_052), "{quality}");
        }
    }

    #[test]
    fn api_requests_send_a_full_browser_identity() {
        let headers = browser_headers();
        assert!(headers["User-Agent"].contains("Chrome/"));
        assert_eq!(headers["Referer"], "https://www.bilibili.com/");
    }

    #[tokio::test]
    #[ignore = "requires live Bilibili access"]
    async fn live_search_resolve_and_download() {
        let items = BILIBILI_PROVIDER.search("晴天 周杰伦", 3).await.unwrap();
        let first = items.first().expect("search result");
        assert!(first.cover.starts_with("https://s1.hdslb.com/bfs/"));
        let track = BILIBILI_PROVIDER
            .resolve(&first.song, Some(&first.id))
            .await
            .unwrap();
        assert_eq!(track.max_chunk_size, Some(CDN_MAX_CHUNK));
        let media = super::super::provider::download_track_media(&track)
            .await
            .unwrap();
        assert!(media.audio.len() > 1024 * 1024);
    }
}
