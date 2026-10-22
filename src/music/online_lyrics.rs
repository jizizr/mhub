//! HyperLyrics-Enhanced 风格的多源在线歌词补全。
//!
//! 这里保留歌曲所属 provider 的原生歌词，并在缺少原文或翻译时，按歌曲元数据
//! 从网易云、QQ 音乐、酷狗、酷我检索候选。候选必须通过标题、歌手、专辑和时长
//! 的身份匹配，避免同名歌曲的歌词串台。

use super::provider::{CLIENT, MusicLyrics, MusicPlatform};
use crate::ApiResult;
use aes::Aes128;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use base64::{Engine as _, engine::general_purpose};
use encoding_rs::GB18030;
use flate2::read::ZlibDecoder;
use lazy_static::lazy_static;
use md5::compute as md5;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Read,
    sync::Mutex,
    time::{Duration, Instant},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const PASS_SCORE: i32 = 70;
const ALIGN_TOLERANCE_MS: i64 = 1_000;

#[derive(Clone, Debug)]
struct Candidate {
    source: Source,
    id: String,
    numeric_id: i64,
    title: String,
    artist: String,
    album: String,
    duration_ms: i64,
    access_key: String,
    content_type: i64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Source {
    Netease,
    Tencent,
    Kugou,
    Kuwo,
}

impl Source {
    fn order(self) -> usize {
        match self {
            Self::Netease => 0,
            Self::Tencent => 1,
            Self::Kugou => 2,
            Self::Kuwo => 3,
        }
    }
}

#[derive(Clone, Debug)]
struct OnlineLyrics {
    plain: String,
    translation: String,
}

#[derive(Clone)]
struct CachedLyrics {
    expires_at: Instant,
    value: Option<OnlineLyrics>,
}

lazy_static! {
    static ref CACHE: Mutex<HashMap<String, CachedLyrics>> = Mutex::new(HashMap::new());
}

pub(super) async fn complete(
    platform: MusicPlatform,
    track_id: &str,
    title: &str,
    artist: &str,
    album: &str,
    duration: Option<u32>,
    native: Option<MusicLyrics>,
) -> ApiResult<Option<MusicLyrics>> {
    let title = title.trim();
    if title.is_empty() {
        return Ok(native);
    }
    if native.as_ref().is_some_and(|lyrics| {
        !lyrics.plain.trim().is_empty() && !lyrics.translation.trim().is_empty()
    }) {
        return Ok(native);
    }
    let key = serde_json::to_string(&(platform.id(), track_id, title, artist, album, duration))?;
    let online = if let Some(value) = cached(&key) {
        value
    } else {
        let value = fetch_online(title, artist, album, duration).await?;
        store(key, value.clone());
        value
    };
    Ok(merge(native, online))
}

fn cached(key: &str) -> Option<Option<OnlineLyrics>> {
    let mut cache = CACHE.lock().ok()?;
    let item = cache.get(key)?.clone();
    if item.expires_at > Instant::now() {
        return Some(item.value);
    }
    cache.remove(key);
    None
}

fn store(key: String, value: Option<OnlineLyrics>) {
    if let Ok(mut cache) = CACHE.lock() {
        cache.retain(|_, item| item.expires_at > Instant::now());
        if cache.len() >= 512 {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, item)| item.expires_at)
                .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            key,
            CachedLyrics {
                expires_at: Instant::now() + Duration::from_secs(30 * 60),
                value,
            },
        );
    }
}

fn merge(native: Option<MusicLyrics>, online: Option<OnlineLyrics>) -> Option<MusicLyrics> {
    match (native, online) {
        (None, None) => None,
        (Some(value), None) => Some(value),
        (None, Some(value)) => Some(MusicLyrics {
            plain: value.plain,
            translation: value.translation,
        }),
        (Some(mut native), Some(online)) => {
            if native.plain.trim().is_empty() {
                native.plain = online.plain;
            }
            if native.translation.trim().is_empty() && !online.translation.trim().is_empty() {
                native.translation = align_translation(&native.plain, &online.translation);
            }
            Some(native)
        }
    }
}

async fn fetch_online(
    title: &str,
    artist: &str,
    album: &str,
    duration: Option<u32>,
) -> ApiResult<Option<OnlineLyrics>> {
    let duration_ms = duration
        .map(|value| value as i64 * 1_000)
        .unwrap_or_default();
    let query = format!("{title} {artist}").trim().to_string();
    let (netease, tencent, kugou, kuwo) = tokio::join!(
        netease_candidates(&query),
        tencent_candidates(&query),
        kugou_candidates(&query, duration_ms),
        kuwo_candidates(&query),
    );
    let mut candidates = Vec::new();
    candidates.extend(netease.unwrap_or_default());
    candidates.extend(tencent.unwrap_or_default());
    candidates.extend(kugou.unwrap_or_default());
    candidates.extend(kuwo.unwrap_or_default());
    candidates.sort_by(|left, right| {
        score(right, title, artist, album, duration_ms)
            .cmp(&score(left, title, artist, album, duration_ms))
            .then_with(|| left.source.order().cmp(&right.source.order()))
    });
    candidates.truncate(12);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(16);
    let mut fallback = None;
    for candidate in candidates {
        if score(&candidate, title, artist, album, duration_ms) < PASS_SCORE {
            continue;
        }
        let result = tokio::time::timeout_at(deadline, fetch_source_lyrics(&candidate)).await;
        let Ok(result) = result else {
            break;
        };
        let Some(lyrics) = result.unwrap_or(None) else {
            continue;
        };
        if lyrics.plain.trim().is_empty() {
            continue;
        }
        if !lyrics.translation.trim().is_empty() {
            return Ok(Some(lyrics));
        }
        // Keep the best original, but keep looking for a matching bilingual source.
        if fallback.is_none() {
            fallback = Some(lyrics);
        }
    }
    Ok(fallback)
}

fn score(candidate: &Candidate, title: &str, artist: &str, album: &str, duration_ms: i64) -> i32 {
    let mut score = 0;
    let wanted_title = normalize(title);
    let candidate_title = normalize(&candidate.title);
    if wanted_title.is_empty() || candidate_title.is_empty() {
        return 0;
    }
    if candidate_title == wanted_title {
        score += 50;
    } else if candidate_title.contains(&wanted_title) || wanted_title.contains(&candidate_title) {
        score += 35;
    }
    let wanted_artists = split_artists(artist);
    let candidate_artists = split_artists(&candidate.artist);
    let artist_matches = wanted_artists.iter().any(|wanted| {
        candidate_artists
            .iter()
            .any(|got| wanted == got || wanted.contains(got) || got.contains(wanted))
    });
    if !wanted_artists.is_empty() && !artist_matches {
        return 0;
    }
    if artist_matches {
        score += 30;
    }
    let wanted_album = normalize(album);
    let candidate_album = normalize(&candidate.album);
    if !wanted_album.is_empty()
        && !candidate_album.is_empty()
        && (wanted_album == candidate_album
            || wanted_album.contains(&candidate_album)
            || candidate_album.contains(&wanted_album))
    {
        score += 10;
    }
    if duration_ms > 0 && candidate.duration_ms > 0 {
        let delta = (duration_ms - candidate.duration_ms).abs();
        score += if delta <= 1_500 {
            15
        } else if delta <= 5_000 {
            10
        } else {
            -30
        };
    }
    score
}

fn split_artists(value: &str) -> Vec<String> {
    value
        .split(['&', ',', '，', '、', '/', '／'])
        .map(normalize)
        .filter(|value| !value.is_empty())
        .collect()
}

fn normalize(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut nesting = 0usize;
    for character in value.trim().chars() {
        match character {
            '(' | '[' | '{' | '（' | '［' | '｛' => nesting += 1,
            ')' | ']' | '}' | '）' | '］' | '｝' => nesting = nesting.saturating_sub(1),
            _ if nesting > 0 => {}
            character if !character.is_whitespace() => output.extend(character.to_lowercase()),
            _ => {}
        }
    }
    output
}

async fn fetch_source_lyrics(candidate: &Candidate) -> ApiResult<Option<OnlineLyrics>> {
    match candidate.source {
        Source::Netease => netease_lyrics(&candidate.id).await,
        Source::Tencent => tencent_lyrics(candidate).await,
        Source::Kugou => kugou_lyrics(candidate).await,
        Source::Kuwo => kuwo_lyrics(&candidate.id).await,
    }
}

async fn netease_candidates(title: &str) -> ApiResult<Vec<Candidate>> {
    let url = format!(
        "https://music.163.com/api/search/get/web?csrf_token=&s={}&type=1&offset=0&total=true&limit=5",
        urlencoding::encode(title)
    );
    let value = get_json(&url, &[]).await?;
    Ok(value["result"]["songs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|song| {
            Some(Candidate {
                source: Source::Netease,
                id: json_id(&song["id"]),
                numeric_id: song["id"].as_i64().unwrap_or_default(),
                title: song["name"].as_str()?.to_string(),
                artist: song["artists"]
                    .as_array()?
                    .iter()
                    .filter_map(|item| item["name"].as_str())
                    .collect::<Vec<_>>()
                    .join("/"),
                album: song["album"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                duration_ms: song["duration"].as_i64().unwrap_or_default(),
                access_key: String::new(),
                content_type: 0,
            })
        })
        .filter(|song| !song.id.is_empty())
        .collect())
}

async fn netease_lyrics(id: &str) -> ApiResult<Option<OnlineLyrics>> {
    let response = match netease_eapi_lyrics(id).await {
        Ok(response) => response,
        Err(_) => {
            get_json(
                &format!(
                    "https://music.163.com/api/song/lyric?id={id}&lv=-1&kv=-1&tv=-1&rv=-1&yv=-1"
                ),
                &[],
            )
            .await?
        }
    };
    let yrc = response["yrc"]["lyric"].as_str().unwrap_or_default();
    let plain = if yrc.trim().is_empty() {
        response["lrc"]["lyric"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    } else {
        yrc_to_lrc(yrc)
    };
    let translation = if !yrc.trim().is_empty() {
        response["ytlrc"]["lyric"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
    } else {
        None
    }
    .or_else(|| response["tlyric"]["lyric"].as_str())
    .unwrap_or_default()
    .to_string();
    if plain.trim().is_empty() || plain.contains("暂无歌词") {
        return Ok(None);
    }
    Ok(Some(OnlineLyrics {
        plain: plain.clone(),
        translation: align_translation(&plain, &translation),
    }))
}

async fn netease_eapi_lyrics(id: &str) -> ApiResult<Value> {
    let path = "/api/song/lyric/v1";
    let params = json!({
        "id": id,
        "lv": "-1",
        "tv": "-1",
        "rv": "-1",
        "yv": "-1"
    })
    .to_string();
    let digest = format!("{:x}", md5(format!("nobody{path}use{params}md5forencrypt")));
    let payload = format!("{path}-36cd479b6b5-{params}-36cd479b6b5-{digest}");
    let encrypted = aes_ecb_encrypt(payload.as_bytes(), b"e82ckenh8dichen8");
    let body = format!(
        "params={}",
        encrypted
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            .to_uppercase()
    );
    let bytes = CLIENT
        .post("https://interface.music.163.com/eapi/song/lyric/v1")
        .timeout(REQUEST_TIMEOUT)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("User-Agent", "Mozilla/5.0 NeteaseMusicDesktop/3.1.3.203419")
        .header("Referer", "https://music.163.com/")
        .body(body)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let decrypted = aes_ecb_decrypt(&bytes, b"e82ckenh8dichen8")
        .ok_or_else(|| crate::ApiError::Custom("网易云 EAPI 响应解密失败".to_string()))?;
    Ok(serde_json::from_slice(&decrypted)?)
}

async fn tencent_candidates(title: &str) -> ApiResult<Vec<Candidate>> {
    let body = json!({
        "comm": {
            "ct": 11,
            "cv": 1003006,
            "v": 1003006,
            "os_ver": "15",
            "phonetype": "24122RKC7C",
            "tmeAppID": "qqmusiclight",
            "nettype": "NETWORK_WIFI"
        },
        "req_0": {
            "method": "DoSearchForQQMusicLite",
            "module": "music.search.SearchCgiService",
            "param": {
                "search_id": "music-api",
                "remoteplace": "search.android.keyboard",
                "query": title,
                "search_type": 0,
                "num_per_page": 5,
                "page_num": 1,
                "highlight": 0,
                "nqc_flag": 0,
                "page_id": 1,
                "grp": 1
            }
        }
    });
    let value = post_json(
        "https://u.y.qq.com/cgi-bin/musicu.fcg",
        &body,
        &[("Referer", "https://y.qq.com/")],
    )
    .await?;
    Ok(value["req_0"]["data"]["body"]["item_song"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|song| {
            Some(Candidate {
                source: Source::Tencent,
                id: song["mid"].as_str()?.to_string(),
                numeric_id: song["id"].as_i64().unwrap_or_default(),
                title: song["title"].as_str()?.to_string(),
                artist: song["singer"]
                    .as_array()?
                    .iter()
                    .filter_map(|item| item["name"].as_str())
                    .collect::<Vec<_>>()
                    .join("/"),
                album: song["album"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                duration_ms: song["interval"].as_i64().unwrap_or_default() * 1_000,
                access_key: String::new(),
                content_type: 0,
            })
        })
        .filter(|song| !song.id.is_empty())
        .collect())
}

async fn tencent_lyrics(candidate: &Candidate) -> ApiResult<Option<OnlineLyrics>> {
    let body = json!({
        "comm": {
            "ct": 19,
            "cv": 2111,
            "v": 2111,
            "os_ver": "15",
            "phonetype": "24122RKC7C",
            "tmeAppID": "qqmusiclight",
            "nettype": "NETWORK_WIFI"
        },
        "req_0": {
            "method": "GetPlayLyricInfo",
            "module": "music.musichallSong.PlayLyricInfo",
            "param": {
                "songID": candidate.numeric_id,
                "songName": general_purpose::STANDARD.encode(candidate.title.as_bytes()),
                "albumName": general_purpose::STANDARD.encode(candidate.album.as_bytes()),
                "singerName": general_purpose::STANDARD.encode(candidate.artist.as_bytes()),
                "crypt": 1,
                "qrc": 1,
                "trans": 1,
                "roma": 1,
                "cv": 2111,
                "ct": 19,
                "lrc_t": 0,
                "qrc_t": 0,
                "roma_t": 0,
                "trans_t": 0,
                "type": 0,
                "interval": candidate.duration_ms / 1_000
            }
        }
    });
    let value = post_json(
        "https://u.y.qq.com/cgi-bin/musicu.fcg",
        &body,
        &[("Referer", "https://y.qq.com/")],
    )
    .await?;
    let data = &value["req_0"]["data"];
    let plain = decode_qrc(data["lyric"].as_str().unwrap_or_default());
    let translation = decode_qrc(data["trans"].as_str().unwrap_or_default());
    if plain.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(OnlineLyrics {
        plain: plain.clone(),
        translation: align_translation(&plain, &translation),
    }))
}

async fn kugou_candidates(title: &str, duration_ms: i64) -> ApiResult<Vec<Candidate>> {
    let params = [
        ("album_audio_id", "0".to_string()),
        ("appid", "1005".to_string()),
        ("clientver", "20759".to_string()),
        ("duration", (duration_ms / 1_000 * 1_000).to_string()),
        ("hash", String::new()),
        ("keyword", title.to_string()),
        ("lrctxt", "1".to_string()),
        ("man", "yes".to_string()),
        ("query_copyright", "1".to_string()),
    ];
    let value = get_json(
        &format!(
            "https://lyrics.kugou.com/v2/search?{}",
            signed_query(&params)
        ),
        &[("User-Agent", "Android-KuGou/20759")],
    )
    .await?;
    Ok(value["data"]["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .take(5)
        .filter_map(|song| {
            Some(Candidate {
                source: Source::Kugou,
                id: song["download_id"]
                    .as_str()
                    .or_else(|| song["id"].as_str())?
                    .to_string(),
                numeric_id: 0,
                title: song["song"].as_str()?.to_string(),
                artist: song["singer"].as_str().unwrap_or_default().to_string(),
                album: String::new(),
                // Kugou's lyric-search response already uses milliseconds.
                duration_ms: song["duration"].as_i64().unwrap_or_default(),
                access_key: song["accesskey"].as_str().unwrap_or_default().to_string(),
                content_type: song["contenttype"].as_i64().unwrap_or_default(),
            })
        })
        .filter(|song| !song.id.is_empty() && !song.access_key.is_empty())
        .collect())
}

async fn kugou_lyrics(candidate: &Candidate) -> ApiResult<Option<OnlineLyrics>> {
    let params = [
        ("accesskey", candidate.access_key.clone()),
        ("appid", "1005".to_string()),
        ("clientver", "20759".to_string()),
        ("contenttype", candidate.content_type.to_string()),
        ("download_id", candidate.id.clone()),
    ];
    let value = get_json(
        &format!(
            "https://lyrics.kugou.com/v2/download?{}",
            signed_query(&params)
        ),
        &[("User-Agent", "Android-KuGou/20759")],
    )
    .await?;
    let content = value["data"]["content"].as_str().unwrap_or_default();
    let bytes = general_purpose::STANDARD
        .decode(content)
        .unwrap_or_default();
    let raw = if bytes.starts_with(b"krc1") {
        decrypt_krc(&bytes)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let plain = if bytes.starts_with(b"krc1") {
        krc_to_lrc(&raw)
    } else {
        raw.clone()
    };
    if plain.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(OnlineLyrics {
        plain: plain.clone(),
        translation: kugou_translation(&raw, &plain),
    }))
}

async fn kuwo_candidates(title: &str) -> ApiResult<Vec<Candidate>> {
    let url = format!(
        "https://www.kuwo.cn/openapi/v1/www/search/searchMusicBykeyWord?key={}&pn=1&rn=5&httpsStatus=1",
        urlencoding::encode(title)
    );
    let value = get_json(&url, &[("Referer", "https://www.kuwo.cn/")]).await?;
    Ok(value["data"]["list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|song| {
            Some(Candidate {
                source: Source::Kuwo,
                id: song["rid"].as_i64()?.to_string(),
                numeric_id: 0,
                title: song["name"].as_str()?.to_string(),
                artist: song["artist"].as_str().unwrap_or_default().to_string(),
                album: song["album"].as_str().unwrap_or_default().to_string(),
                duration_ms: song["duration"].as_i64().unwrap_or_default() * 1_000,
                access_key: String::new(),
                content_type: 0,
            })
        })
        .filter(|song| !song.id.is_empty())
        .collect())
}

async fn kuwo_lyrics(id: &str) -> ApiResult<Option<OnlineLyrics>> {
    if let Ok(Some(raw)) = kuwo_lrcx(id).await {
        if let Some((plain, translation)) = kuwo_lrcx_to_lrc(&raw) {
            return Ok(Some(OnlineLyrics { plain, translation }));
        }
    }
    let url =
        format!("https://www.kuwo.cn/openapi/v1/www/lyric/getlyric?musicId={id}&httpsStatus=1");
    let value = get_json(&url, &[("Referer", "https://www.kuwo.cn/")]).await?;
    let plain = value["data"]["lrclist"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|line| {
            let time = line["time"].as_str()?.parse::<f64>().ok()?;
            Some(format!(
                "[{}]{}",
                format_lrc_time((time * 1_000.0) as i64),
                line["lineLyric"].as_str().unwrap_or_default()
            ))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if plain.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(OnlineLyrics {
        plain,
        translation: String::new(),
    }))
}

async fn kuwo_lrcx(id: &str) -> ApiResult<Option<String>> {
    let request = format!("user=12345,web,web,web&requester=localhost&req=1&rid=MUSIC_{id}&lrcx=1");
    let encoded = general_purpose::STANDARD.encode(xor_bytes(request.as_bytes(), b"yeelion"));
    let response = get_bytes(
        &format!("https://newlyric.kuwo.cn/newlyric.lrc?{encoded}"),
        &[
            ("User-Agent", "okhttp/3.10.0"),
            ("Referer", "https://www.kuwo.cn/"),
        ],
    )
    .await?;
    let prefix = b"tp=content";
    let separator = b"\r\n\r\n";
    if !response.starts_with(prefix) {
        return Ok(None);
    }
    let Some(offset) = response
        .windows(separator.len())
        .position(|window| window == separator)
        .map(|position| position + separator.len())
    else {
        return Ok(None);
    };
    let mut inflated = String::new();
    ZlibDecoder::new(&response[offset..]).read_to_string(&mut inflated)?;
    let decoded = general_purpose::STANDARD
        .decode(inflated.trim())
        .map_err(|error| crate::ApiError::Custom(format!("Kuwo LRCX Base64 解码失败: {error}")))?;
    let decoded_bytes = xor_bytes(&decoded, b"yeelion");
    let (decoded, _, had_errors) = GB18030.decode(&decoded_bytes);
    if had_errors || decoded.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(decoded.into_owned()))
}

fn kuwo_lrcx_to_lrc(raw: &str) -> Option<(String, String)> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Kind {
        Plain,
        Timed,
        Auxiliary,
    }
    let marker = regex::Regex::new(r"<(-?\d+),(-?\d+)>([^<]*)").ok()?;
    let timestamp = regex::Regex::new(r"^\[(\d{1,3}):(\d{2})(?:[.:](\d{1,3}))?\]").ok()?;
    let mut lines = Vec::new();
    for line in raw.lines() {
        let Some(captures) = timestamp.captures(line.trim()) else {
            continue;
        };
        let minutes = captures.get(1)?.as_str().parse::<i64>().ok()?;
        let seconds = captures.get(2)?.as_str().parse::<i64>().ok()?;
        let fraction = captures.get(3).map(|value| value.as_str()).unwrap_or("0");
        let fraction = match fraction.len() {
            0 => 0,
            1 => fraction.parse::<i64>().ok()? * 100,
            2 => fraction.parse::<i64>().ok()? * 10,
            _ => fraction[..3].parse::<i64>().ok()?,
        };
        let start = minutes * 60_000 + seconds * 1_000 + fraction;
        let payload = &line[line.find(']')? + 1..];
        let markers = marker.captures_iter(payload).collect::<Vec<_>>();
        let text = if markers.is_empty() {
            payload.trim().to_string()
        } else {
            markers
                .iter()
                .filter_map(|item| item.get(3).map(|value| value.as_str()))
                .collect::<String>()
                .trim()
                .to_string()
        };
        if text.is_empty() {
            continue;
        }
        let kind = if markers.is_empty() {
            Kind::Plain
        } else if markers.iter().all(|item| {
            item.get(1)
                .and_then(|value| value.as_str().parse::<i64>().ok())
                == Some(0)
                && item
                    .get(2)
                    .and_then(|value| value.as_str().parse::<i64>().ok())
                    == Some(0)
        }) {
            Kind::Auxiliary
        } else {
            Kind::Timed
        };
        lines.push((start, text, kind));
    }
    if lines.is_empty() {
        return None;
    }
    let mut originals = Vec::new();
    let mut translations = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let (start, text, kind) = &lines[index];
        if *kind == Kind::Auxiliary {
            if let Some((original_start, _, _)) = originals.last() {
                translations.push((*original_start, text.clone()));
            }
        } else if *kind == Kind::Plain
            && lines
                .get(index + 1)
                .is_some_and(|next| next.0 == *start && next.2 == Kind::Plain)
        {
            translations.push((*start, text.clone()));
            index += 1;
            originals.push((*start, lines[index].1.clone(), lines[index].2));
        } else {
            originals.push((*start, text.clone(), *kind));
        }
        index += 1;
    }
    let plain = originals
        .iter()
        .map(|(time, text, _)| format!("[{}]{}", format_lrc_time(*time), text))
        .collect::<Vec<_>>()
        .join("\n");
    let translation = translations
        .iter()
        .map(|(time, text)| format!("[{}]{}", format_lrc_time(*time), text))
        .collect::<Vec<_>>()
        .join("\n");
    (!plain.is_empty()).then_some((plain, translation))
}

fn xor_bytes(input: &[u8], key: &[u8]) -> Vec<u8> {
    input
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ key[index % key.len()])
        .collect()
}

async fn get_json(url: &str, headers: &[(&str, &str)]) -> ApiResult<Value> {
    let mut request = CLIENT.get(url).timeout(REQUEST_TIMEOUT);
    for (key, value) in headers {
        request = request.header(*key, *value);
    }
    let response = request.send().await?.error_for_status()?;
    Ok(response.json().await?)
}

async fn get_bytes(url: &str, headers: &[(&str, &str)]) -> ApiResult<Vec<u8>> {
    let mut request = CLIENT.get(url).timeout(REQUEST_TIMEOUT);
    for (key, value) in headers {
        request = request.header(*key, *value);
    }
    Ok(request
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}

async fn post_json(url: &str, body: &Value, headers: &[(&str, &str)]) -> ApiResult<Value> {
    let mut request = CLIENT
        .post(url)
        .timeout(REQUEST_TIMEOUT)
        .header("content-type", "application/json")
        .body(body.to_string());
    for (key, value) in headers {
        request = request.header(*key, *value);
    }
    let response = request.send().await?.error_for_status()?;
    Ok(response.json().await?)
}

fn json_id(value: &Value) -> String {
    value
        .as_i64()
        .map(|id| id.to_string())
        .or_else(|| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn signed_query(parameters: &[(&str, String)]) -> String {
    let mut sorted = parameters
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let joined = sorted
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<String>();
    let signature = format!(
        "{:x}",
        md5(format!(
            "OIlwieks28dk2k092lksi2UIkp{joined}OIlwieks28dk2k092lksi2UIkp"
        ))
    );
    sorted.push(("signature".to_string(), signature));
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    sorted
        .into_iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(&key),
                urlencoding::encode(&value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn decrypt_krc(input: &[u8]) -> String {
    const KEY: &[u8] = &[
        0x40, 0x47, 0x61, 0x77, 0x5e, 0x32, 0x74, 0x47, 0x51, 0x36, 0x31, 0x2d, 0xce, 0xd2, 0x6e,
        0x69,
    ];
    let decoded = input[4..]
        .iter()
        .enumerate()
        .map(|(index, value)| value ^ KEY[index % KEY.len()])
        .collect::<Vec<_>>();
    let mut decoder = ZlibDecoder::new(decoded.as_slice());
    let mut output = String::new();
    decoder.read_to_string(&mut output).ok();
    output
}

fn krc_to_lrc(raw: &str) -> String {
    raw.lines()
        .filter_map(|line| {
            let (head, body) = line.trim().split_once(']')?;
            let values = head.strip_prefix('[')?.split(',').collect::<Vec<_>>();
            let start = values.first()?.parse::<i64>().ok()?;
            let text = body
                .split('<')
                .skip(1)
                .filter_map(|item| item.split_once('>').map(|(_, text)| text))
                .collect::<String>();
            let text = if text.is_empty() {
                body.to_string()
            } else {
                text
            };
            (!text.trim().is_empty())
                .then(|| format!("[{}]{}", format_lrc_time(start), text.trim()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn kugou_translation(raw: &str, original: &str) -> String {
    let Some(encoded) = raw.lines().find_map(|line| {
        line.strip_prefix("[language:")
            .and_then(|value| value.strip_suffix(']'))
    }) else {
        return String::new();
    };
    let Ok(decoded) = general_purpose::STANDARD.decode(encoded) else {
        return String::new();
    };
    let Ok(value) = serde_json::from_slice::<Value>(&decoded) else {
        return String::new();
    };
    let Some(lines) = value["content"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["type"] == 1))
        .and_then(|item| item["lyricContent"].as_array())
    else {
        return String::new();
    };
    let times = parse_lrc(original);
    if lines.len() != times.len() {
        return String::new();
    }
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let text = line
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect::<String>();
            let &(time, _) = times.get(index)?;
            (!text.trim().is_empty()).then(|| format!("[{}]{}", format_lrc_time(time), text.trim()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn decode_qrc(value: &str) -> String {
    let value = value.trim();
    if value.starts_with('<') || value.starts_with('[') {
        return qrc_to_lrc(value);
    }
    let hex: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    if hex.is_empty() || hex.len() % 16 != 0 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return String::new();
    }
    let bytes = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect::<Vec<_>>();
    let Some(decrypted) = super::qrc_crypto::decrypt(&bytes) else {
        return String::new();
    };
    let mut output = String::new();
    if ZlibDecoder::new(decrypted.as_slice())
        .take(2 * 1024 * 1024)
        .read_to_string(&mut output)
        .is_err()
    {
        return String::new();
    }
    qrc_to_lrc(&output)
}

/// Flatten QRC word timing into line-synchronized LRC for the current API.
/// Extract the attribute before parsing XML: XML parsers normalize literal
/// newlines inside attributes, but QQ stores the entire lyric there.
fn qrc_to_lrc(value: &str) -> String {
    let xml = regex::Regex::new(r#"(?s)LyricContent\s*=\s*["'](.*?)["']\s*/?>"#).unwrap();
    let content = xml
        .captures(value)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .unwrap_or(value);
    let content = content
        .replace("&#13;", "")
        .replace("&#10;", "\n")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let words = regex::Regex::new(r"\(\d+,\d+\)").unwrap();
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (header, body) = line.strip_prefix('[')?.split_once(']')?;
            if let Some((start, duration)) = header.split_once(',') {
                let start = start.parse::<i64>().ok()?;
                duration.parse::<u64>().ok()?;
                let text = words.replace_all(body, "");
                (!text.trim().is_empty())
                    .then(|| format!("[{}]{}", format_lrc_time(start), text.trim()))
            } else {
                // Auxiliary lyrics may already be ordinary LRC.
                parse_lrc_time_value(header)?;
                Some(line.to_string())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn aes_ecb_encrypt(input: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let padding = 16 - input.len() % 16;
    let mut output = input.to_vec();
    output.extend(std::iter::repeat_n(padding as u8, padding));
    for block in output.chunks_exact_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    output
}

fn aes_ecb_decrypt(input: &[u8], key: &[u8; 16]) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() % 16 != 0 {
        return None;
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut output = input.to_vec();
    for block in output.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    let padding = *output.last()? as usize;
    if padding == 0 || padding > 16 || output.len() < padding {
        return None;
    }
    output.truncate(output.len() - padding);
    Some(output)
}

fn yrc_to_lrc(value: &str) -> String {
    value
        .lines()
        .filter_map(|line| {
            let (header, body) = line.trim().split_once(']')?;
            let values = header.strip_prefix('[')?.split(',').collect::<Vec<_>>();
            let start = values.first()?.parse::<i64>().ok()?;
            let mut text = String::new();
            let mut remaining = body;
            while let Some(marker_end) = remaining.find(')') {
                let after_marker = &remaining[marker_end + 1..];
                let next_marker = after_marker.find('(').unwrap_or(after_marker.len());
                text.push_str(&after_marker[..next_marker]);
                if next_marker == after_marker.len() {
                    break;
                }
                remaining = &after_marker[next_marker..];
            }
            if text.is_empty() {
                text.push_str(body);
            }
            (!text.trim().is_empty())
                .then(|| format!("[{}]{}", format_lrc_time(start), text.trim()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn align_translation(original: &str, translation: &str) -> String {
    if translation.trim().is_empty() {
        return String::new();
    }
    let original_lines = parse_lrc(original);
    let translations = parse_lrc(translation);
    let mut used = vec![false; translations.len()];
    original_lines
        .iter()
        .filter_map(|(time, _)| {
            let (index, (_, text)) = translations
                .iter()
                .enumerate()
                .filter(|(index, (candidate, text))| {
                    !used[*index]
                        && !text.trim().is_empty()
                        && (*candidate - *time).abs() <= ALIGN_TOLERANCE_MS
                })
                .min_by_key(|(_, (candidate, _))| (*candidate - *time).abs())?;
            used[index] = true;
            Some(format!("[{}]{}", format_lrc_time(*time), text.trim()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_lrc(value: &str) -> Vec<(i64, String)> {
    let mut rows = Vec::new();
    for line in value.lines() {
        let mut remaining = line.trim();
        let mut times = Vec::new();
        while let Some(after) = remaining.strip_prefix('[') {
            let Some((tag, rest)) = after.split_once(']') else {
                break;
            };
            let Some(time) = parse_lrc_time_value(tag) else {
                break;
            };
            times.push(time);
            remaining = rest;
        }
        let text = remaining.trim();
        if !text.is_empty() {
            rows.extend(times.into_iter().map(|time| (time, text.to_string())));
        }
    }
    rows.sort_by_key(|(time, _)| *time);
    rows
}

fn parse_lrc_time_value(value: &str) -> Option<i64> {
    if !value.is_ascii() {
        return None;
    }
    let (minutes, seconds) = value.split_once(':')?;
    let minutes = minutes.parse::<i64>().ok()?;
    let (seconds, fraction) = seconds
        .split_once('.')
        .or_else(|| seconds.split_once(':'))
        .unwrap_or((seconds, "0"));
    let seconds = seconds.parse::<i64>().ok()?;
    let fraction = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().ok()? * 100,
        2 => fraction.parse::<i64>().ok()? * 10,
        _ => fraction[..3].parse::<i64>().ok()?,
    };
    Some(minutes * 60_000 + seconds * 1_000 + fraction)
}

fn format_lrc_time(milliseconds: i64) -> String {
    let milliseconds = milliseconds.max(0);
    format!(
        "{:02}:{:02}.{:03}",
        milliseconds / 60_000,
        (milliseconds / 1_000) % 60,
        milliseconds % 1_000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_translation_by_timestamp() {
        let result = align_translation(
            "[00:01.00]原文\n[00:03.00]下一句",
            "[00:01.80]翻译\n[00:03.10]下一句翻译",
        );
        assert_eq!(result, "[00:01.000]翻译\n[00:03.000]下一句翻译");
    }

    #[test]
    fn parses_krc_to_lrc() {
        assert_eq!(
            krc_to_lrc("[1000,2000]<0,1000,0>你<1000,1000,0>好"),
            "[00:01.000]你好"
        );
    }

    #[test]
    fn parses_qrc_to_lrc() {
        assert_eq!(
            qrc_to_lrc("[1000,2000](1000,700)你(1700,300)好\n[3000,500]世界"),
            "[00:01.000]你好\n[00:03.000]世界"
        );
    }

    #[test]
    fn decodes_plain_qrc_without_crypto() {
        assert_eq!(
            decode_qrc("[1000,2000](1000,700)你(1700,300)好"),
            "[00:01.000]你好"
        );
    }

    #[test]
    fn decodes_upstream_custom_cipher_fixture() {
        // Produced by the unmodified upstream Kotlin TripleDesCustom at
        // 44775f5 with zlib-compressed QRC XML, not by our Rust implementation.
        let fixture = "ee7befb746f93831d42dcdf2e21f0d5d833c044042289012e0c11abf870c7dfb9c809065c5235ff1cf6f94cef240c69ae0b16cc9d426d2dc3a61034bd9502329408720784d10dcc362a80b009b71f77e";
        assert_eq!(decode_qrc(fixture), "[00:01.000]Hello");
    }

    #[test]
    fn qrc_xml_and_lrc_auxiliary_preserve_text() {
        let xml = "<QrcInfos><Lyric_1 LyricType=\"1\" LyricContent=\"[1000,2000]Hello (world)(1000,2000) &amp; you\n[3000,500]Bye(3000,500)\"/></QrcInfos>";
        assert_eq!(
            decode_qrc(xml),
            "[00:01.000]Hello (world) & you\n[00:03.000]Bye"
        );
        assert_eq!(decode_qrc("[00:01.000]你好"), "[00:01.000]你好");
        assert!(decode_qrc("aabb invalid encrypted data").is_empty());
    }

    #[test]
    fn pairs_kuwo_lrcx_auxiliary_lines_as_translation() {
        let (plain, translation) = kuwo_lrcx_to_lrc(
            "[00:01.000]<0,2000>Hello\n[00:01.000]<0,0>你好\n[00:03.000]<0,2000>World",
        )
        .expect("LRCX should parse");
        assert_eq!(plain, "[00:01.000]Hello\n[00:03.000]World");
        assert_eq!(translation, "[00:01.000]你好");
    }

    #[test]
    fn scores_exact_identity() {
        let candidate = Candidate {
            source: Source::Netease,
            id: "1".into(),
            numeric_id: 1,
            title: "红豆".into(),
            artist: "王菲".into(),
            album: "唱游".into(),
            duration_ms: 256_000,
            access_key: String::new(),
            content_type: 0,
        };
        assert!(score(&candidate, "红豆", "王菲", "唱游", 256_000) >= PASS_SCORE);
        assert!(score(&candidate, "红豆", "另一位歌手", "唱游", 256_000) < PASS_SCORE);
    }

    #[test]
    fn expands_repeated_lrc_timestamps_and_rejects_malformed_time() {
        assert_eq!(
            parse_lrc("  [00:03.00][00:01.00]Hello"),
            vec![(1000, "Hello".into()), (3000, "Hello".into())]
        );
        assert_eq!(parse_lrc_time_value("00:00.你你"), None);
    }

    #[test]
    fn preserves_native_bilingual_lyrics_and_aligns_missing_translation() {
        let make = |translation: &str| {
            Some(MusicLyrics {
                plain: "[00:01.000]Hello".into(),
                translation: translation.into(),
            })
        };
        let online = || {
            Some(OnlineLyrics {
                plain: "[00:01.200]Hello".into(),
                translation: "[00:01.200]你好".into(),
            })
        };
        assert_eq!(
            merge(make("[00:01.000]原有翻译"), online())
                .unwrap()
                .translation,
            "[00:01.000]原有翻译"
        );
        assert_eq!(
            merge(make(""), online()).unwrap().translation,
            "[00:01.000]你好"
        );
    }
}
