use super::provider::{
    MusicCollection, MusicLyrics, MusicPlatform, MusicProvider, MusicSearchItem, MusicTrack,
    VkeysResponse, append_vkeys_auth, get_json_with_headers, json_id_to_string,
    parse_vkeys_duration, parse_vkeys_kbps, vkeys_enabled, vkeys_track_format, vkeys_url,
};
use crate::BotError;
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;

pub static NETEASE_PROVIDER: NeteaseProvider = NeteaseProvider;

pub struct NeteaseProvider;

#[async_trait]
impl MusicProvider for NeteaseProvider {
    async fn search(&self, keyword: &str, limit: usize) -> Result<Vec<MusicSearchItem>, BotError> {
        match search_netease_native(keyword, limit).await {
            Ok(songs) if !songs.is_empty() => Ok(songs),
            // Without vkeys, surface the native result (empty list or real error) untouched.
            native_result if !vkeys_enabled() => native_result,
            native_result => match search_netease_by_vkeys(keyword, limit).await {
                Ok(songs) => Ok(vkeys_netease_to_items(songs, limit)),
                Err(_) => native_result,
            },
        }
    }

    async fn resolve(
        &self,
        keyword: &str,
        selected_id: Option<&str>,
    ) -> Result<MusicTrack, BotError> {
        resolve_with_quality(keyword, selected_id, "lossless").await
    }

    async fn collection(
        &self,
        keyword: &str,
        limit: usize,
    ) -> Result<Option<MusicCollection>, BotError> {
        let Some(collection) = parse_netease_collection_id(keyword) else {
            return Ok(None);
        };
        get_netease_collection(collection, limit).await.map(Some)
    }

    async fn lyrics(&self, track_id: &str) -> Result<Option<MusicLyrics>, BotError> {
        let id = parse_netease_id(track_id).unwrap_or_else(|| track_id.trim().to_string());
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            return Ok(None);
        }
        match get_native_netease_lyrics(&id).await {
            Ok(Some(lyrics)) => Ok(Some(lyrics)),
            Ok(None) | Err(_) => get_vkeys_netease_lyrics(&id).await,
        }
    }
}

pub(super) async fn resolve_with_quality(
    keyword: &str,
    selected_id: Option<&str>,
    quality: &str,
) -> Result<MusicTrack, BotError> {
    let id = resolve_netease_id(keyword, selected_id).await?;
    if should_prefer_vkeys_netease_download(quality)
        && netease_cookie().is_none()
        && vkeys_enabled()
    {
        match get_vkeys_netease_track(&id, quality).await {
            Ok(track) => return Ok(track),
            Err(vkeys_error) => match resolve_netease_native(&id, quality).await {
                Ok(track) => return Ok(track),
                Err(_) => return Err(vkeys_error),
            },
        }
    }
    match resolve_netease_native(&id, quality).await {
        Ok(track) => Ok(track),
        Err(native_error) if vkeys_enabled() => match get_vkeys_netease_track(&id, quality).await {
            Ok(track) => Ok(track),
            Err(_) => Err(native_error),
        },
        Err(native_error) => Err(native_error),
    }
}

async fn resolve_netease_id(keyword: &str, selected_id: Option<&str>) -> Result<String, BotError> {
    if let Some(id) = selected_id {
        return Ok(id.to_string());
    }
    if let Some(id) = parse_netease_id(keyword) {
        return Ok(id);
    }
    NETEASE_PROVIDER
        .search(keyword, 1)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| BotError::Custom("没有找到可下载的音乐".to_string()))
        .map(|item| item.id)
}

async fn resolve_netease_native(id: &str, quality: &str) -> Result<MusicTrack, BotError> {
    let (song, source) =
        tokio::try_join!(get_netease_song(id), get_netease_audio_source(id, quality))?;
    let singer = netease_artists(&song);
    let album = song.album.unwrap_or_default();
    Ok(MusicTrack {
        id: id.to_string(),
        platform: MusicPlatform::Netease,
        song: song.name,
        singer,
        album: album.name,
        cover: album.pic_url.unwrap_or_default(),
        link: format!("https://music.163.com/#/song?id={id}"),
        url: source.url,
        headers: HashMap::new(),
        max_chunk_size: None,
        replay_gain_centi_db: source.replay_gain_centi_db,
        served_quality: None,
        duration: song
            .duration
            .or(source.duration_ms)
            .filter(|duration| *duration > 0)
            .map(|duration| ((duration + 500) / 1000) as u32),
        bitrate: source.bitrate,
        // The CDN labels FLAC as audio/mpeg; the API's `type` is authoritative.
        format: Some(source.format),
    })
}

async fn search_netease_native(
    keyword: &str,
    limit: usize,
) -> Result<Vec<MusicSearchItem>, BotError> {
    let url = format!(
        "https://music.163.com/api/search/get/web?csrf_token=&s={}&type=1&offset=0&total=true&limit={limit}",
        urlencoding::encode(keyword)
    );
    let response: NeteaseSearchResponse = get_netease_json(&url).await?;
    Ok(response
        .result
        .and_then(|result| result.songs)
        .unwrap_or_default()
        .into_iter()
        .map(netease_song_to_item)
        .filter(|item| !item.id.is_empty())
        .collect())
}

async fn get_native_netease_lyrics(id: &str) -> Result<Option<MusicLyrics>, BotError> {
    Ok(netease_lyrics_from_response(get_netease_lyrics(id).await?))
}

fn netease_lyrics_from_response(response: NeteaseLyricResponse) -> Option<MusicLyrics> {
    let text = |part: Option<NeteaseLyricPart>| part.map(|lyric| lyric.lyric).unwrap_or_default();
    let plain = text(response.lrc);
    let translation = text(response.tlyric);
    let words = response
        .yrc
        .map(|lyric| super::word_lyrics::parse_yrc(&lyric.lyric))
        .unwrap_or_default();
    // `yromalrc` is timed to the word-by-word lines, `romalrc` to the LRC.
    let line_roman = text(response.romalrc);
    let word_roman = text(response.yromalrc);
    let roman = if !words.is_empty() && !word_roman.trim().is_empty() {
        word_roman
    } else {
        line_roman
    };
    if plain.trim().is_empty() && translation.trim().is_empty() && words.is_empty() {
        return None;
    }
    let credits = [
        ("歌词贡献者", response.lyric_user),
        ("翻译贡献者", response.trans_user),
    ]
    .into_iter()
    .filter_map(|(role, user)| {
        let name = user?.nickname?;
        let name = name.trim();
        (!name.is_empty()).then(|| format!("{role}：{name}"))
    })
    .collect();
    Some(MusicLyrics {
        plain,
        translation,
        words,
        roman,
        credits,
    })
}

#[derive(Deserialize)]
struct NeteaseSearchResponse {
    result: Option<NeteaseSearchResult>,
}

#[derive(Deserialize)]
struct NeteaseSearchResult {
    songs: Option<Vec<NeteaseSong>>,
}

#[derive(Deserialize)]
struct NeteaseDetailResponse {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    songs: Vec<NeteaseSong>,
}

#[derive(Deserialize)]
struct NeteaseUrlResponse {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    data: Vec<NeteaseUrlData>,
}

#[derive(Deserialize)]
struct NeteaseUrlData {
    url: Option<String>,
    #[serde(default)]
    br: Option<u64>,
    #[serde(default, rename = "type")]
    file_type: Option<String>,
    /// The tier actually served, e.g. `lossless` when `hires` has no Hi-Res master.
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    time: Option<u64>,
    /// ReplayGain-style track gain (dB to reach -18 LUFS); measured against
    /// ffmpeg ebur128 on several songs it agrees within 0.1 dB.
    #[serde(default)]
    gain: Option<f64>,
    #[serde(default, rename = "freeTrialInfo")]
    free_trial_info: Option<NeteaseFreeTrialInfo>,
}

#[derive(Debug, PartialEq)]
struct NeteaseAudioSource {
    url: String,
    format: String,
    bitrate: Option<u32>,
    level: Option<String>,
    duration_ms: Option<u64>,
    replay_gain_centi_db: Option<i32>,
}

#[derive(Deserialize)]
struct NeteaseFreeTrialInfo {
    #[allow(dead_code)]
    start: Option<i64>,
    #[allow(dead_code)]
    end: Option<i64>,
}

#[derive(Deserialize)]
struct NeteasePlaylistResponse {
    playlist: Option<NeteasePlaylist>,
}

#[derive(Deserialize)]
struct NeteasePlaylist {
    id: serde_json::Value,
    name: String,
    #[serde(default)]
    tracks: Vec<NeteaseSong>,
    #[serde(default, rename = "trackIds")]
    track_ids: Vec<NeteaseTrackId>,
    #[serde(default, rename = "trackCount")]
    track_count: Option<usize>,
}

#[derive(Deserialize)]
struct NeteaseTrackId {
    id: serde_json::Value,
}

#[derive(Deserialize)]
struct NeteaseAlbumResponse {
    album: Option<NeteaseAlbumInfo>,
    #[serde(default)]
    songs: Vec<NeteaseSong>,
}

#[derive(Deserialize)]
struct NeteaseAlbumInfo {
    id: serde_json::Value,
    name: String,
    artist: Option<NeteaseArtist>,
}

#[derive(Deserialize)]
struct NeteaseLyricResponse {
    lrc: Option<NeteaseLyricPart>,
    tlyric: Option<NeteaseLyricPart>,
    yrc: Option<NeteaseLyricPart>,
    #[serde(default)]
    romalrc: Option<NeteaseLyricPart>,
    #[serde(default)]
    yromalrc: Option<NeteaseLyricPart>,
    #[serde(default, rename = "lyricUser")]
    lyric_user: Option<NeteaseLyricUser>,
    #[serde(default, rename = "transUser")]
    trans_user: Option<NeteaseLyricUser>,
}

#[derive(Deserialize)]
struct NeteaseLyricUser {
    #[serde(default)]
    nickname: Option<String>,
}

#[derive(Deserialize)]
struct NeteaseLyricPart {
    #[serde(default)]
    lyric: String,
}

#[derive(Clone, Deserialize)]
struct NeteaseSong {
    id: serde_json::Value,
    name: String,
    #[serde(default, alias = "ar")]
    artists: Vec<NeteaseArtist>,
    #[serde(default, rename = "duration")]
    duration: Option<u64>,
    #[serde(alias = "al")]
    album: Option<NeteaseAlbum>,
}

#[derive(Clone, Deserialize)]
struct NeteaseArtist {
    name: String,
}

#[derive(Clone, Default, Deserialize)]
struct NeteaseAlbum {
    #[serde(default)]
    name: String,
    #[serde(rename = "picUrl")]
    pic_url: Option<String>,
}

#[derive(Clone, Default, Deserialize)]
struct VkeysNeteaseTrack {
    #[serde(default)]
    id: serde_json::Value,
    #[serde(default)]
    song: String,
    #[serde(default)]
    singer: String,
    #[serde(default)]
    album: String,
    #[serde(default)]
    cover: String,
    #[serde(default)]
    interval: String,
    #[serde(default)]
    link: String,
    #[serde(default)]
    quality: String,
    #[serde(default)]
    size: String,
    #[serde(default)]
    kbps: String,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum VkeysNeteaseData {
    List(Vec<VkeysNeteaseTrack>),
    Track(Box<VkeysNeteaseTrack>),
}

#[derive(Deserialize)]
struct VkeysNeteaseLyricsData {
    #[serde(default)]
    lrc: String,
    #[serde(default)]
    tlyric: String,
}

async fn search_netease_by_vkeys(
    keyword: &str,
    limit: usize,
) -> Result<Vec<VkeysNeteaseTrack>, BotError> {
    if !vkeys_enabled() {
        return Err(BotError::Custom("vkeys 未启用".to_string()));
    }
    let mut url = vkeys_url("/v2/music/netease")?;
    append_vkeys_auth(&mut url);
    url.query_pairs_mut()
        .append_pair("word", keyword)
        .append_pair("page", "1")
        .append_pair("num", &limit.clamp(1, 50).to_string());
    let response: VkeysResponse<VkeysNeteaseData> = get_vkeys_json(url.as_str()).await?;
    response.ensure_success("vkeys 网易云搜索")?;
    match response.data {
        Some(VkeysNeteaseData::List(list)) => Ok(list),
        Some(VkeysNeteaseData::Track(track)) => Ok(vec![*track]),
        None => Ok(Vec::new()),
    }
}

async fn get_vkeys_netease_track(id: &str, quality: &str) -> Result<MusicTrack, BotError> {
    let mut url = vkeys_url("/v2/music/netease")?;
    append_vkeys_auth(&mut url);
    url.query_pairs_mut()
        .append_pair("id", id)
        .append_pair("quality", &netease_vkeys_quality(quality).to_string());
    let response: VkeysResponse<VkeysNeteaseTrack> = get_vkeys_json(url.as_str()).await?;
    response.ensure_success("vkeys 网易云下载")?;
    let track = response
        .data
        .ok_or_else(|| BotError::Custom("vkeys 网易云没有返回歌曲数据".to_string()))?;
    let download_url = track
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| BotError::Custom("vkeys 网易云没有返回可下载直链".to_string()))?
        .to_string();
    Ok(MusicTrack {
        id: json_id_to_string(track.id).if_empty(|| id.to_string()),
        platform: MusicPlatform::Netease,
        song: track.song.if_empty(|| "未知歌曲".to_string()),
        singer: track.singer.if_empty(|| "未知歌手".to_string()),
        album: track.album,
        cover: track.cover,
        link: track
            .link
            .if_empty(|| format!("https://music.163.com/#/song?id={id}")),
        url: download_url.clone(),
        headers: HashMap::new(),
        max_chunk_size: None,
        replay_gain_centi_db: None,
        served_quality: None,
        duration: parse_vkeys_duration(&track.interval),
        // MusicTrack.bitrate 按 bps 消费，vkeys 报的是 kbps
        bitrate: parse_vkeys_kbps(&track.kbps).map(|kbps| kbps.saturating_mul(1000)),
        format: vkeys_track_format(&download_url, &track.quality, &track.size),
    })
}

async fn get_vkeys_netease_lyrics(id: &str) -> Result<Option<MusicLyrics>, BotError> {
    if !vkeys_enabled() {
        return Ok(None);
    }
    let mut url = vkeys_url("/v2/music/netease/lyric")?;
    append_vkeys_auth(&mut url);
    url.query_pairs_mut().append_pair("id", id);
    let response: VkeysResponse<VkeysNeteaseLyricsData> = get_vkeys_json(url.as_str()).await?;
    response.ensure_success("vkeys 网易云歌词")?;
    let Some(data) = response.data else {
        return Ok(None);
    };
    if data.lrc.trim().is_empty() && data.tlyric.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(MusicLyrics {
        plain: data.lrc,
        translation: data.tlyric,
        ..Default::default()
    }))
}

pub(super) async fn track_info(id: &str) -> Result<super::TrackInfo, BotError> {
    if !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(BotError::Custom("网易云歌曲 ID 无效".to_string()));
    }
    let song = get_netease_song(id).await?;
    let singer = netease_artists(&song);
    let album = song.album.unwrap_or_default();
    Ok(super::TrackInfo {
        platform: MusicPlatform::Netease.id(),
        id: id.to_string(),
        song: song.name,
        singer,
        album: album.name,
        cover: album.pic_url.unwrap_or_default(),
    })
}

async fn get_netease_song(id: &str) -> Result<NeteaseSong, BotError> {
    let url = format!(
        "https://music.163.com/api/song/detail?ids=[{}]",
        urlencoding::encode(id)
    );
    let response: NeteaseDetailResponse = get_netease_json(&url).await?;
    ensure_netease_ok(response.code, response.msg.as_deref())?;
    response
        .songs
        .into_iter()
        .next()
        .ok_or_else(|| BotError::Custom("没有找到歌曲详情".to_string()))
}

async fn get_netease_audio_source(id: &str, quality: &str) -> Result<NeteaseAudioSource, BotError> {
    // `url/v1` selects by tier and reports what it actually served; the old
    // bitrate endpoint stays as a fallback when the EAPI gateway misbehaves.
    let response = match get_netease_url_v1(id, quality).await {
        Ok(response) => response,
        Err(_) => get_netease_url_legacy(id, quality).await?,
    };
    ensure_netease_ok(response.code, response.msg.as_deref())?;
    if let Some(source) = response
        .data
        .into_iter()
        .find_map(netease_full_audio_source)
    {
        return Ok(source);
    }
    let hint = if netease_cookie().is_none() {
        "未配置网易云登录 Cookie，会员歌曲无法获取"
    } else {
        match super::netease_session::logged_in().await {
            Some(false) => "网易云登录已失效，请更新 Cookie",
            Some(true) => "账号会员权益不足或歌曲已下架",
            None => "可能是账号会员权益不足或登录已失效",
        }
    };
    Err(BotError::Custom(format!(
        "网易云只返回了试听链接或没有可播放链接（{hint}）"
    )))
}

async fn get_netease_url_v1(id: &str, quality: &str) -> Result<NeteaseUrlResponse, BotError> {
    let ids = serde_json::to_string(&[id])?;
    let params = serde_json::json!({
        "ids": ids,
        "level": netease_native_level(quality),
        "encodeType": "flac",
    });
    netease_eapi("/api/song/enhance/player/url/v1", &params).await
}

async fn get_netease_url_legacy(id: &str, quality: &str) -> Result<NeteaseUrlResponse, BotError> {
    let url = format!(
        "https://music.163.com/api/song/enhance/player/url?id={}&ids=[{}]&br={}",
        urlencoding::encode(id),
        urlencoding::encode(id),
        netease_native_bitrate(quality)
    );
    get_netease_json(&url).await
}

const NETEASE_EAPI_KEY: &[u8; 16] = b"e82ckenh8dichen8";

pub(super) async fn netease_eapi<T: serde::de::DeserializeOwned>(
    path: &str,
    params: &serde_json::Value,
) -> Result<T, BotError> {
    netease_eapi_with_cookies(path, params)
        .await
        .map(|(body, _)| body)
}

/// Like `netease_eapi`, also returning the response's `Set-Cookie` values.
pub(super) async fn netease_eapi_with_cookies<T: serde::de::DeserializeOwned>(
    path: &str,
    params: &serde_json::Value,
) -> Result<(T, Vec<String>), BotError> {
    let api_path = path
        .strip_prefix("/api")
        .ok_or_else(|| BotError::Custom(format!("无效的网易云 EAPI 路径 {path}")))?;
    let mut request = super::provider::CLIENT
        .post(format!("https://interface.music.163.com/eapi{api_path}"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("User-Agent", "Mozilla/5.0 NeteaseMusicDesktop/3.1.3.203419")
        .header("Referer", "https://music.163.com/")
        .body(netease_eapi_body(path, &params.to_string()));
    if let Some(cookie) = netease_cookie() {
        request = request.header("Cookie", netease_eapi_cookie(&cookie));
    }
    let resp = request.send().await?;
    if !resp.status().is_success() {
        return Err(BotError::Custom(format!(
            "网易云 EAPI 请求失败：HTTP {}",
            resp.status()
        )));
    }
    let set_cookies = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect();
    let bytes = resp.bytes().await?;
    // Without `e_r` the gateway answers in plain JSON; with it, AES-ECB.
    if let Ok(value) = serde_json::from_slice(&bytes) {
        return Ok((value, set_cookies));
    }
    let decrypted = super::online_lyrics::aes_ecb_decrypt(&bytes, NETEASE_EAPI_KEY)
        .ok_or_else(|| BotError::Custom("网易云 EAPI 响应解密失败".to_string()))?;
    Ok((serde_json::from_slice(&decrypted)?, set_cookies))
}

fn netease_eapi_body(path: &str, params: &str) -> String {
    let digest = format!(
        "{:x}",
        md5::compute(format!("nobody{path}use{params}md5forencrypt"))
    );
    let payload = format!("{path}-36cd479b6b5-{params}-36cd479b6b5-{digest}");
    let encrypted = super::online_lyrics::aes_ecb_encrypt(payload.as_bytes(), NETEASE_EAPI_KEY);
    let hex = encrypted
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    format!("params={hex}")
}

/// The EAPI gateway grants member tiers to desktop-client sessions.
fn netease_eapi_cookie(cookie: &str) -> String {
    let mut cookie = netease_cookie_with_device(cookie);
    for (key, value) in [("os", "pc"), ("appver", "3.1.3.203419")] {
        if !cookie_has_key(&cookie, key) {
            let separator = if cookie.trim_end().ends_with(';') {
                " "
            } else {
                "; "
            };
            cookie = format!("{cookie}{separator}{key}={value}");
        }
    }
    cookie
}

fn netease_full_audio_source(item: NeteaseUrlData) -> Option<NeteaseAudioSource> {
    if item.free_trial_info.is_some() {
        return None;
    }
    let url = item.url.map(|url| url.trim().to_string())?;
    if url.is_empty() {
        return None;
    }
    let format = item
        .file_type
        .as_deref()
        .map(|kind| kind.trim().to_ascii_lowercase())
        .filter(|kind| !kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphanumeric()))
        .or_else(|| {
            url.split('?')
                .next()
                .and_then(|path| path.rsplit_once('.'))
                .map(|(_, ext)| ext.to_ascii_lowercase())
                .filter(|ext| matches!(ext.as_str(), "mp3" | "flac" | "m4a"))
        })
        .unwrap_or_else(|| "mp3".to_string());
    Some(NeteaseAudioSource {
        url,
        format,
        bitrate: item
            .br
            .filter(|br| *br > 0)
            .map(|br| br.min(u32::MAX as u64) as u32),
        level: item.level.filter(|level| !level.trim().is_empty()),
        duration_ms: item.time.filter(|time| *time > 0),
        // 0 means "not measured"; anything beyond ±30 dB is not a real gain.
        replay_gain_centi_db: item
            .gain
            .filter(|gain| gain.is_finite() && *gain != 0.0 && gain.abs() <= 30.0)
            .map(|gain| (gain * 100.0).round() as i32),
    })
}

fn ensure_netease_ok(code: Option<i64>, msg: Option<&str>) -> Result<(), BotError> {
    match code {
        Some(code) if code != 200 => Err(BotError::Custom(format!(
            "网易云 API 错误 {code}: {}",
            msg.unwrap_or_default()
        ))),
        _ => Ok(()),
    }
}

async fn get_netease_collection(
    collection: NeteaseCollectionId,
    limit: usize,
) -> Result<MusicCollection, BotError> {
    match collection.kind {
        NeteaseCollectionKind::Playlist => get_netease_playlist(&collection.id, limit).await,
        NeteaseCollectionKind::Album => get_netease_album_collection(&collection.id, limit).await,
    }
}

async fn get_netease_playlist(id: &str, limit: usize) -> Result<MusicCollection, BotError> {
    let url = format!(
        "https://music.163.com/api/v6/playlist/detail?id={}",
        urlencoding::encode(id)
    );
    let response: NeteasePlaylistResponse = get_netease_json(&url).await?;
    let playlist = response
        .playlist
        .ok_or_else(|| BotError::Custom("没有找到网易云歌单".to_string()))?;
    let max_items = limit.max(1);
    let allow_incomplete = super::incomplete_collection_allowed();
    validate_playlist_completeness(&playlist, max_items, allow_incomplete)?;
    let playlist_id = json_id_to_string(playlist.id);
    let track_ids = playlist
        .track_ids
        .into_iter()
        .take(max_items)
        .map(|track| {
            let id = json_id_to_string(track.id);
            if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_digit()) {
                return Err(BotError::Custom(
                    "网易云歌单包含无效歌曲 ID，已停止导入以避免漏歌".to_string(),
                ));
            }
            Ok(id)
        })
        .collect::<Result<Vec<_>, BotError>>()?;
    let songs = if track_ids.is_empty() {
        playlist.tracks.into_iter().take(max_items).collect()
    } else {
        get_netease_songs(&track_ids, allow_incomplete).await?
    };
    let items = songs
        .into_iter()
        .map(netease_song_to_item)
        .filter(|item| !item.id.trim().is_empty())
        .collect();
    Ok(MusicCollection {
        platform: MusicPlatform::Netease,
        id: playlist_id,
        title: playlist.name,
        items,
    })
}

/// NetEase may count songs it no longer lists (removed or hidden), so a
/// shortfall is reported as incomplete; the user can still import the rest.
fn validate_playlist_completeness(
    playlist: &NeteasePlaylist,
    limit: usize,
    allow_incomplete: bool,
) -> Result<(), BotError> {
    let available = if playlist.track_ids.is_empty() {
        playlist.tracks.len()
    } else {
        playlist.track_ids.len()
    };
    let expected = playlist.track_count.unwrap_or(available).min(limit);
    if available < expected {
        if allow_incomplete {
            tracing::info!(
                "importing NetEase playlist with {available} of {expected} track ids by request"
            );
            return Ok(());
        }
        return Err(BotError::IncompleteCollection(format!(
            "网易云歌单应有 {expected} 首，但接口只返回 {available} 首，缺少的 {} 首可能已下架或不可见",
            expected - available
        )));
    }
    Ok(())
}

async fn get_netease_songs(
    ids: &[String],
    allow_incomplete: bool,
) -> Result<Vec<NeteaseSong>, BotError> {
    let mut songs_by_id = HashMap::new();
    for chunk in ids.chunks(200) {
        let encoded_ids = format!("[{}]", chunk.join(","));
        let url = format!(
            "https://music.163.com/api/song/detail?ids={}",
            urlencoding::encode(&encoded_ids)
        );
        let response: NeteaseDetailResponse = get_netease_json(&url).await?;
        ensure_netease_ok(response.code, response.msg.as_deref())?;
        for song in response.songs {
            let song_id = json_id_to_string(song.id.clone());
            if !song_id.is_empty() {
                songs_by_id.insert(song_id, song);
            }
        }
    }
    let missing = ids
        .iter()
        .filter(|id| !songs_by_id.contains_key(*id))
        .count();
    if missing > 0 && !allow_incomplete {
        return Err(BotError::IncompleteCollection(format!(
            "网易云歌单有 {missing} 首歌曲详情未返回，可能已下架或不可见"
        )));
    }
    Ok(songs_in_playlist_order(ids, &songs_by_id))
}

fn songs_in_playlist_order(
    ids: &[String],
    songs_by_id: &HashMap<String, NeteaseSong>,
) -> Vec<NeteaseSong> {
    ids.iter()
        .filter_map(|id| songs_by_id.get(id).cloned())
        .collect()
}

async fn get_netease_album_collection(id: &str, limit: usize) -> Result<MusicCollection, BotError> {
    let url = format!(
        "https://music.163.com/api/album/{}",
        urlencoding::encode(id)
    );
    let response: NeteaseAlbumResponse = get_netease_json(&url).await?;
    let album = response
        .album
        .ok_or_else(|| BotError::Custom("没有找到网易云专辑".to_string()))?;
    let artist = album
        .artist
        .as_ref()
        .map(|artist| artist.name.trim())
        .filter(|name| !name.is_empty());
    let title = artist
        .map(|artist| format!("{} - {}", album.name, artist))
        .unwrap_or(album.name);
    let items = response
        .songs
        .into_iter()
        .take(limit.max(1))
        .map(netease_song_to_item)
        .filter(|item| !item.id.trim().is_empty())
        .collect();
    Ok(MusicCollection {
        platform: MusicPlatform::Netease,
        id: json_id_to_string(album.id),
        title,
        items,
    })
}

async fn get_netease_lyrics(id: &str) -> Result<NeteaseLyricResponse, BotError> {
    let url = netease_lyrics_url(id);
    get_netease_json(&url).await
}

fn netease_lyrics_url(id: &str) -> String {
    format!(
        "https://music.163.com/api/song/lyric?id={}&lv=-1&kv=-1&tv=-1&rv=-1&yv=-1",
        urlencoding::encode(id)
    )
}

async fn get_netease_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, BotError> {
    get_json_with_headers(url, &netease_headers()).await
}

async fn get_vkeys_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, BotError> {
    get_json_with_headers(url, &HashMap::new()).await
}

/// vkeys levels: 2 standard 128k, 4 极高 320k, 5 SQ lossless, 6 Hi-Res.
fn netease_vkeys_quality(quality: &str) -> i64 {
    match quality.trim().to_ascii_lowercase().as_str() {
        "standard" | "std" | "128" => 2,
        "lossless" | "flac" | "无损" => 5,
        "hires" | "hi-res" | "master" => 6,
        _ => 4,
    }
}

/// Hi-Res falls back to `lossless` server-side when a song has no master.
/// `jyeffect`/`sky` are processed surround renditions, not masters, so no
/// listening tier maps onto them.
fn netease_native_level(quality: &str) -> &'static str {
    match quality.trim().to_ascii_lowercase().as_str() {
        "standard" | "std" | "128" => "standard",
        "lossless" | "flac" | "无损" => "lossless",
        "hires" | "hi-res" | "master" => "hires",
        _ => "exhigh",
    }
}

fn netease_native_bitrate(quality: &str) -> i64 {
    match quality.trim().to_ascii_lowercase().as_str() {
        "standard" | "std" | "128" => 128_000,
        "lossless" | "flac" | "无损" => 999_000,
        "hires" | "hi-res" | "master" => 1_999_000,
        _ => 320_000,
    }
}

fn should_prefer_vkeys_netease_download(quality: &str) -> bool {
    matches!(
        quality.trim().to_ascii_lowercase().as_str(),
        "lossless" | "flac" | "无损" | "hires" | "hi-res" | "master"
    )
}

trait IfEmpty {
    fn if_empty<F>(self, fallback: F) -> String
    where
        F: FnOnce() -> String;
}

impl IfEmpty for String {
    fn if_empty<F>(self, fallback: F) -> String
    where
        F: FnOnce() -> String,
    {
        if self.trim().is_empty() {
            fallback()
        } else {
            self
        }
    }
}

fn netease_headers() -> HashMap<String, String> {
    let mut headers = HashMap::from([
        ("Referer".to_string(), "https://music.163.com/".to_string()),
        (
            "User-Agent".to_string(),
            "Mozilla/5.0 AppleWebKit/537.36 Chrome/125 Safari/537.36".to_string(),
        ),
    ]);
    if let Some(cookie) = netease_cookie() {
        headers.insert("Cookie".to_string(), netease_cookie_with_device(&cookie));
    }
    headers
}

fn netease_cookie() -> Option<String> {
    super::netease_session::current_cookie()
}

fn netease_cookie_with_device(cookie: &str) -> String {
    let cookie = cookie.trim();
    if cookie_has_key(cookie, "NMTID") {
        return cookie.to_string();
    }

    let separator = if cookie.ends_with(';') { " " } else { "; " };
    // 与上游一致：每次请求随机生成，反作弊只校验存在性与格式（见上游 e1bddf6）
    format!("{cookie}{separator}NMTID={}", random_netease_nmtid())
}

fn cookie_has_key(cookie: &str, key: &str) -> bool {
    cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .any(|(name, _)| name.trim().eq_ignore_ascii_case(key))
}

fn random_netease_nmtid() -> String {
    let mut value = uuid::Uuid::new_v4().simple().to_string();
    value.truncate(30);
    format!("00{value}")
}

fn netease_song_to_item(song: NeteaseSong) -> MusicSearchItem {
    let singer = netease_artists(&song);
    let cover = song
        .album
        .as_ref()
        .and_then(|album| album.pic_url.clone())
        .unwrap_or_default();
    MusicSearchItem {
        platform: MusicPlatform::Netease,
        id: json_id_to_string(song.id),
        song: song.name,
        singer,
        cover,
    }
}

fn vkeys_netease_to_item(song: VkeysNeteaseTrack) -> MusicSearchItem {
    MusicSearchItem {
        platform: MusicPlatform::Netease,
        id: json_id_to_string(song.id),
        song: song.song.if_empty(|| "未知歌曲".to_string()),
        singer: song.singer.if_empty(|| "未知歌手".to_string()),
        cover: song.cover,
    }
}

fn vkeys_netease_to_items(songs: Vec<VkeysNeteaseTrack>, limit: usize) -> Vec<MusicSearchItem> {
    songs
        .into_iter()
        .take(limit)
        .map(vkeys_netease_to_item)
        .filter(|item| !item.id.is_empty())
        .collect()
}

fn netease_artists(song: &NeteaseSong) -> String {
    let artists = song
        .artists
        .iter()
        .map(|artist| artist.name.as_str())
        .filter(|name| !name.trim().is_empty())
        .collect::<Vec<_>>();
    if artists.is_empty() {
        "未知歌手".to_string()
    } else {
        artists.join(" / ")
    }
}

fn parse_netease_id(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Some(trimmed.to_string());
    }

    let url = url::Url::parse(trimmed).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if !host.contains("163.com") {
        return None;
    }
    netease_url_path_and_query(&url)
        .1
        .and_then(|query| query_id(&query))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NeteaseCollectionKind {
    Playlist,
    Album,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NeteaseCollectionId {
    kind: NeteaseCollectionKind,
    id: String,
}

fn parse_netease_collection_id(text: &str) -> Option<NeteaseCollectionId> {
    let trimmed = text.trim();
    if let Some(id) = trimmed.strip_prefix("album:")
        && is_netease_numeric_id(id)
    {
        return Some(NeteaseCollectionId {
            kind: NeteaseCollectionKind::Album,
            id: id.trim().to_string(),
        });
    }
    if let Some(id) = trimmed.strip_prefix("playlist:")
        && is_netease_numeric_id(id)
    {
        return Some(NeteaseCollectionId {
            kind: NeteaseCollectionKind::Playlist,
            id: id.trim().to_string(),
        });
    }
    let url = url::Url::parse(trimmed).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if !host.contains("163.com") {
        return None;
    }
    let (path, query) = netease_url_path_and_query(&url);
    let id = query.and_then(|query| query_id(&query))?;
    if path.contains("album") {
        return Some(NeteaseCollectionId {
            kind: NeteaseCollectionKind::Album,
            id,
        });
    }
    if path.contains("playlist") {
        return Some(NeteaseCollectionId {
            kind: NeteaseCollectionKind::Playlist,
            id,
        });
    }
    None
}

fn netease_url_path_and_query(url: &url::Url) -> (String, Option<String>) {
    if let Some(fragment) = url
        .fragment()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let fragment = fragment.trim_start_matches('/');
        let (path, query) = fragment.split_once('?').unwrap_or((fragment, ""));
        return (
            path.to_ascii_lowercase(),
            (!query.trim().is_empty()).then(|| query.to_string()),
        );
    }
    (
        url.path().to_ascii_lowercase(),
        url.query().map(ToString::to_string),
    )
}

fn query_id(query: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "id")
        .map(|(_, value)| value.to_string())
        .filter(|id| is_netease_numeric_id(id))
}

fn is_netease_numeric_id(value: &str) -> bool {
    !value.trim().is_empty() && value.trim().chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_incomplete_playlist_ids_instead_of_importing_only_preview_tracks() {
        let playlist: NeteasePlaylist = serde_json::from_value(serde_json::json!({
            "id": 1, "name": "Fixture", "trackCount": 25,
            "tracks": [], "trackIds": (1..=10).map(|id| serde_json::json!({"id": id})).collect::<Vec<_>>()
        })).unwrap();
        assert!(matches!(
            validate_playlist_completeness(&playlist, 5001, false),
            Err(BotError::IncompleteCollection(_))
        ));
        assert!(validate_playlist_completeness(&playlist, 10, false).is_ok());
        // The user confirmed importing the songs NetEase still lists.
        assert!(validate_playlist_completeness(&playlist, 5001, true).is_ok());
    }

    #[test]
    fn playlist_detail_uses_all_ids_beyond_the_old_200_track_cap() {
        let playlist: NeteasePlaylist = serde_json::from_value(serde_json::json!({
            "id": 1, "name": "Fixture", "trackCount": 275,
            "tracks": [], "trackIds": (1..=275).map(|id| serde_json::json!({"id": id})).collect::<Vec<_>>()
        })).unwrap();
        assert!(validate_playlist_completeness(&playlist, 5001, false).is_ok());
        assert_eq!(playlist.track_ids.len(), 275);
    }

    #[tokio::test]
    #[ignore = "requires live public NetEase API"]
    async fn live_playlist_import_uses_complete_track_ids_in_source_order() {
        let response: NeteasePlaylistResponse =
            get_netease_json("https://music.163.com/api/v6/playlist/detail?id=7261574758")
                .await
                .unwrap();
        let playlist = response.playlist.unwrap();
        let expected = playlist
            .track_ids
            .into_iter()
            .map(|track| json_id_to_string(track.id))
            .collect::<Vec<_>>();
        let collection = get_netease_playlist("7261574758", super::super::MAX_COLLECTION_ITEMS + 1)
            .await
            .unwrap();
        assert_eq!(
            collection.items.len(),
            playlist.track_count.unwrap_or(expected.len())
        );
        assert_eq!(
            collection
                .items
                .iter()
                .map(|item| &item.id)
                .collect::<Vec<_>>(),
            expected.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn parses_netease_collection_urls() {
        assert_eq!(
            parse_netease_collection_id("https://music.163.com/#/playlist?id=123456"),
            Some(NeteaseCollectionId {
                kind: NeteaseCollectionKind::Playlist,
                id: "123456".to_string(),
            })
        );
        assert_eq!(
            parse_netease_collection_id("https://music.163.com/#/album?id=654321"),
            Some(NeteaseCollectionId {
                kind: NeteaseCollectionKind::Album,
                id: "654321".to_string(),
            })
        );
        assert_eq!(
            parse_netease_collection_id("https://music.163.com/#/song?id=449818741"),
            None
        );
    }

    #[test]
    fn maps_vkeys_netease_quality_from_global_quality() {
        assert_eq!(netease_vkeys_quality("standard"), 2);
        assert_eq!(netease_vkeys_quality("high"), 4);
        assert_eq!(netease_vkeys_quality("lossless"), 5);
        assert_eq!(netease_vkeys_quality("hires"), 6);
        assert_eq!(netease_native_bitrate("standard"), 128_000);
        assert_eq!(netease_native_bitrate("high"), 320_000);
        assert_eq!(netease_native_bitrate("lossless"), 999_000);
    }

    #[test]
    fn parses_vkeys_netease_media_metadata() {
        assert_eq!(parse_vkeys_duration("4分29秒"), Some(269));
        assert_eq!(parse_vkeys_duration("183"), Some(183));
        assert_eq!(parse_vkeys_kbps("3002kbps"), Some(3002));
    }

    #[test]
    fn lyrics_keep_romanization_matching_the_lines_shown_and_contributors() {
        let response = |yrc: &str| -> NeteaseLyricResponse {
            serde_json::from_value(serde_json::json!({
                "lrc": {"lyric": "[00:21.961]拦路雨"},
                "romalrc": {"lyric": "[00:21.961]lan lou yv"},
                "yrc": {"lyric": yrc},
                "yromalrc": {"lyric": "[00:22.180]lan lou yv"},
                "lyricUser": {"nickname": " 贡献者甲 "},
                "transUser": {"nickname": ""}
            }))
            .unwrap()
        };
        let worded = netease_lyrics_from_response(response(
            "[22180,1000](22180,500,0)拦(22680,500,0)路\n[23180,1000](23180,500,0)雨(23680,500,0)偏",
        ))
        .unwrap();
        assert!(!worded.words.is_empty());
        assert_eq!(worded.roman, "[00:22.180]lan lou yv");
        assert_eq!(worded.credits, vec!["歌词贡献者：贡献者甲"]);
        let lined = netease_lyrics_from_response(response("")).unwrap();
        assert_eq!(lined.roman, "[00:21.961]lan lou yv");
    }

    #[test]
    fn netease_lyrics_url_requests_all_source_tracks() {
        let url = netease_lyrics_url("449818741");
        assert!(url.contains("lv=-1"));
        assert!(url.contains("kv=-1"));
        assert!(url.contains("tv=-1"));
        assert!(url.contains("rv=-1"));
        assert!(url.contains("yv=-1"));
    }

    #[test]
    fn injects_random_nmtid_into_netease_cookie() {
        let cookie = netease_cookie_with_device("MUSIC_U=abc; os=android");
        assert!(cookie.contains("MUSIC_U=abc"));
        assert!(cookie.contains("os=android"));

        let nmtid = cookie
            .split(';')
            .map(str::trim)
            .find_map(|part| part.strip_prefix("NMTID="))
            .expect("NMTID should be injected");
        assert_eq!(nmtid.len(), 32);
        assert!(nmtid.starts_with("00"));
        assert!(nmtid.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn preserves_existing_netease_nmtid_cookie() {
        let cookie = "MUSIC_U=abc; NMTID=00existingdeviceid";
        assert_eq!(netease_cookie_with_device(cookie), cookie);
        assert!(cookie_has_key("music_u=abc; nmtid=value", "NMTID"));
    }

    #[test]
    fn only_lossless_netease_prefers_vkeys_without_cookie() {
        assert!(!should_prefer_vkeys_netease_download("standard"));
        assert!(!should_prefer_vkeys_netease_download("high"));
        assert!(should_prefer_vkeys_netease_download("lossless"));
        assert!(should_prefer_vkeys_netease_download("hires"));
    }

    #[test]
    fn rejects_netease_free_trial_url() {
        let trial: NeteaseUrlData = serde_json::from_str(
            r#"{"url":"https://example.test/trial.mp3","freeTrialInfo":{"start":0,"end":30000}}"#,
        )
        .unwrap();
        assert_eq!(netease_full_audio_source(trial), None);

        let full: NeteaseUrlData =
            serde_json::from_str(r#"{"url":"https://example.test/full.mp3"}"#).unwrap();
        assert_eq!(
            netease_full_audio_source(full).map(|source| source.url),
            Some("https://example.test/full.mp3".to_string())
        );
    }

    #[test]
    fn keeps_the_format_and_tier_the_api_actually_served() {
        // Real `url/v1` shape: a FLAC on an `.flac`-less URL, Hi-Res downgraded.
        let item: NeteaseUrlData = serde_json::from_value(serde_json::json!({
            "url": "http://m801.music.126.net/x/y/abc", "br": 1567785, "size": 50737963,
            "type": "FLAC", "encodeType": "mp3", "level": "lossless", "time": 258902,
            "freeTrialInfo": null
        }))
        .unwrap();
        let source = netease_full_audio_source(item).unwrap();
        assert_eq!(source.format, "flac");
        assert_eq!(source.bitrate, Some(1_567_785));
        assert_eq!(source.level.as_deref(), Some("lossless"));
        assert_eq!(source.duration_ms, Some(258_902));
        assert_eq!(source.replay_gain_centi_db, None);
        let with_gain: NeteaseUrlData = serde_json::from_value(serde_json::json!({
            "url": "https://example.test/a.flac", "type": "flac", "gain": -7.291
        }))
        .unwrap();
        assert_eq!(
            netease_full_audio_source(with_gain)
                .unwrap()
                .replay_gain_centi_db,
            Some(-729)
        );

        let untyped: NeteaseUrlData =
            serde_json::from_str(r#"{"url":"https://example.test/a.flac?x=1"}"#).unwrap();
        assert_eq!(netease_full_audio_source(untyped).unwrap().format, "flac");
        let empty: NeteaseUrlData = serde_json::from_str(r#"{"url":"  "}"#).unwrap();
        assert_eq!(netease_full_audio_source(empty), None);
    }

    #[test]
    fn maps_listening_tiers_to_url_v1_levels() {
        assert_eq!(netease_native_level("standard"), "standard");
        assert_eq!(netease_native_level("high"), "exhigh");
        assert_eq!(netease_native_level("lossless"), "lossless");
        assert_eq!(netease_native_level("hires"), "hires");
    }

    #[test]
    fn eapi_body_is_uppercase_hex_of_whole_blocks() {
        let body = netease_eapi_body("/api/song/enhance/player/url/v1", r#"{"ids":"[\"1\"]"}"#);
        let hex = body.strip_prefix("params=").unwrap();
        assert_eq!(hex.len() % 32, 0);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
        );
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect::<Vec<_>>();
        let plain = super::super::online_lyrics::aes_ecb_decrypt(&bytes, NETEASE_EAPI_KEY).unwrap();
        assert!(
            String::from_utf8(plain)
                .unwrap()
                .starts_with("/api/song/enhance/player/url/v1-36cd479b6b5-")
        );
    }

    #[test]
    fn eapi_cookie_adds_desktop_client_markers_once() {
        let cookie = netease_eapi_cookie("MUSIC_U=abc; os=ios");
        assert!(cookie.contains("os=ios"));
        assert!(!cookie.contains("os=pc"));
        assert!(cookie.contains("appver=3.1.3.203419"));
        assert!(cookie.contains("NMTID="));
    }

    #[test]
    fn preserves_playlist_order_and_duplicate_tracks() {
        let ids = vec!["1".to_string(), "1".to_string(), "2".to_string()];
        let mut songs = HashMap::new();
        for (id, name) in [("1", "first"), ("2", "second")] {
            let song: NeteaseSong = serde_json::from_value(serde_json::json!({
                "id": id,
                "name": name,
                "ar": [],
                "al": {"name": "album"}
            }))
            .unwrap();
            songs.insert(id.to_string(), song);
        }
        let result = songs_in_playlist_order(&ids, &songs);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].name, "first");
        assert_eq!(result[1].name, "first");
        assert_eq!(result[2].name, "second");
    }
}
