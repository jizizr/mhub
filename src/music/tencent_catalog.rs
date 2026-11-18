//! QQ Music catalog search (songs, albums, playlists), album pages and
//! album/playlist collections. Its singer search is closed to anonymous
//! clients, so QQ Music has no artist section or page.

use super::{MUSICU_ENDPOINT, QQSinger, build_track_cover_url, post_json, singers_to_string};
use crate::BotError;
use crate::music::catalog::{AlbumDetail, CatalogItem, CatalogKind, CatalogSearch};
use crate::music::provider::MusicProvider;
use crate::music::provider::{MusicCollection, MusicPlatform, MusicSearchItem, first_non_empty};
use serde::Deserialize;
use serde_json::json;

const PLATFORM: MusicPlatform = MusicPlatform::Tencent;
const PLAYLIST_PAGE: usize = 500;

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Song {
    id: i64,
    mid: String,
    name: String,
    title: String,
    singer: Vec<QQSinger>,
    album: SongAlbum,
    /// Seconds.
    interval: Option<u64>,
    index_album: Option<u32>,
    index_cd: Option<u32>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct SongAlbum {
    mid: String,
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AlbumHit {
    #[serde(rename = "albumMID")]
    album_mid: String,
    #[serde(rename = "albumName")]
    album_name: String,
    #[serde(rename = "singerName")]
    singer_name: String,
    #[serde(rename = "publicTime")]
    public_time: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct PlaylistHit {
    dissid: String,
    dissname: String,
    imgurl: String,
    creator: Creator,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Creator {
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Hits<T> {
    list: Vec<T>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SearchBody {
    song: Hits<Song>,
    album: Hits<AlbumHit>,
    songlist: Hits<PlaylistHit>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SearchData {
    body: SearchBody,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Part<T> {
    code: i64,
    data: T,
}

#[derive(Deserialize)]
struct SearchResponse {
    code: i64,
    #[serde(default)]
    song: Option<Part<SearchData>>,
    #[serde(default)]
    album: Option<Part<SearchData>>,
    #[serde(default)]
    playlist: Option<Part<SearchData>>,
}

fn search_request(keyword: &str, kind: u32, count: usize) -> serde_json::Value {
    json!({
        "method": "DoSearchForQQMusicDesktop",
        "module": "music.search.SearchCgiService",
        "param": {
            "grp": 1,
            "num_per_page": count,
            "page_num": 1,
            "query": keyword,
            "search_type": kind
        }
    })
}

pub(in crate::music) async fn catalog_search(
    keyword: &str,
    limit: usize,
) -> Result<CatalogSearch, BotError> {
    // One round trip carries all three searches.
    let payload = json!({
        "comm": { "ct": "19", "cv": "1859", "uin": "0" },
        "song": search_request(keyword, 0, limit),
        "album": search_request(keyword, 2, limit.min(12)),
        "playlist": search_request(keyword, 3, limit.min(12)),
    });
    let body = post_json(MUSICU_ENDPOINT, &payload, false).await?;
    let response: SearchResponse = serde_json::from_slice(&body)?;
    let mut found = search_from_response(response);
    // The song part alone sometimes answers 2001; the song search with its
    // own fallbacks fills it then, and the other sections stay.
    if found.songs.is_empty() {
        found.songs = super::TENCENT_PROVIDER
            .search(keyword, limit)
            .await?
            .into_iter()
            .map(|item| {
                CatalogItem::new(CatalogKind::Song, PLATFORM, item.id)
                    .title(item.song)
                    .subtitle(item.singer)
                    .cover(item.cover)
            })
            .collect();
    }
    found.rank_top(keyword);
    Ok(found)
}

/// Every section that answered; a failed one stays empty.
fn search_from_response(response: SearchResponse) -> CatalogSearch {
    let usable = |part: Option<Part<SearchData>>| {
        part.filter(|part| response.code == 0 && part.code == 0)
            .map(|part| part.data.body)
    };
    let mut found = CatalogSearch::new(PLATFORM);
    if let Some(songs) = usable(response.song) {
        found.songs = songs.song.list.into_iter().map(song_item).collect();
    }
    if let Some(albums) = usable(response.album) {
        found.albums = albums.album.list.into_iter().map(album_hit).collect();
    }
    if let Some(playlists) = usable(response.playlist) {
        found.playlists = playlists
            .songlist
            .list
            .into_iter()
            .map(playlist_hit)
            .collect();
    }
    found
}

fn song_item(song: Song) -> CatalogItem {
    let id = first_non_empty([Some(song.mid.as_str())])
        .map(ToOwned::to_owned)
        .or_else(|| (song.id > 0).then(|| song.id.to_string()))
        .unwrap_or_default();
    let title = first_non_empty([Some(song.title.as_str()), Some(song.name.as_str())])
        .unwrap_or_default()
        .to_string();
    let mut item = CatalogItem::new(CatalogKind::Song, PLATFORM, id)
        .title(title)
        .subtitle(singers_to_string(&song.singer))
        .album(song.album.name)
        .cover(build_track_cover_url(&song.album.mid));
    item.duration_ms = song.interval.filter(|s| *s > 0).map(|s| s * 1000);
    item.track_number = song.index_album.filter(|n| *n > 0);
    // `index_cd` counts from zero.
    item.disc_number = song.index_cd.map(|cd| cd + 1);
    item
}

fn album_url(mid: &str) -> String {
    format!("https://y.qq.com/n/ryqq/albumDetail/{mid}")
}

fn album_hit(album: AlbumHit) -> CatalogItem {
    CatalogItem::new(CatalogKind::Album, PLATFORM, album.album_mid.clone())
        .title(album.album_name)
        .subtitle(album.singer_name)
        .cover(album_cover(&album.album_mid, 500))
        .release_date(album.public_time)
        .url(album_url(&album.album_mid))
}

fn album_cover(mid: &str, size: u32) -> String {
    if mid.trim().is_empty() {
        String::new()
    } else {
        format!("https://y.gtimg.cn/music/photo_new/T002R{size}x{size}M000{mid}.jpg")
    }
}

fn playlist_hit(playlist: PlaylistHit) -> CatalogItem {
    CatalogItem::new(CatalogKind::Playlist, PLATFORM, playlist.dissid.clone())
        .title(playlist.dissname)
        .subtitle(playlist.creator.name)
        .cover(https(&playlist.imgurl))
        .url(format!(
            "https://y.qq.com/n/ryqq/playlist/{}",
            playlist.dissid
        ))
}

fn https(url: &str) -> String {
    match url.strip_prefix("http://") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    }
}

fn valid_mid(mid: &str) -> bool {
    !mid.is_empty() && mid.len() <= 32 && mid.chars().all(|c| c.is_ascii_alphanumeric())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AlbumInfoData {
    #[serde(rename = "basicInfo")]
    basic_info: AlbumBasicInfo,
    singer: AlbumSingers,
    company: AlbumCompany,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AlbumBasicInfo {
    #[serde(rename = "albumMid")]
    album_mid: String,
    #[serde(rename = "albumName")]
    album_name: String,
    #[serde(rename = "publishDate")]
    publish_date: String,
    #[serde(rename = "genre")]
    genre: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AlbumSingers {
    #[serde(rename = "singerList")]
    singer_list: Vec<AlbumSinger>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AlbumSinger {
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AlbumCompany {
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AlbumSongsData {
    #[serde(rename = "songList")]
    song_list: Vec<AlbumSongEntry>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AlbumSongEntry {
    #[serde(rename = "songInfo")]
    song_info: Song,
}

#[derive(Deserialize)]
struct AlbumResponse {
    code: i64,
    #[serde(default)]
    album: Option<Part<AlbumInfoData>>,
    #[serde(default)]
    songs: Option<Part<AlbumSongsData>>,
}

pub(in crate::music) async fn album_detail(mid: &str) -> Result<AlbumDetail, BotError> {
    if !valid_mid(mid) {
        return Err(BotError::Custom("QQ音乐专辑 ID 无效".to_string()));
    }
    let payload = json!({
        "comm": { "ct": 24, "cv": 0 },
        "album": {
            "module": "music.musichallAlbum.AlbumInfoServer",
            "method": "GetAlbumDetail",
            "param": { "albumMid": mid }
        },
        "songs": {
            "module": "music.musichallAlbum.AlbumSongList",
            "method": "GetAlbumSongList",
            "param": { "albumMid": mid, "begin": 0, "num": 500, "order": 2 }
        }
    });
    let body = post_json(MUSICU_ENDPOINT, &payload, false).await?;
    album_from_response(serde_json::from_slice(&body)?, mid)
}

fn album_from_response(response: AlbumResponse, mid: &str) -> Result<AlbumDetail, BotError> {
    let info = response
        .album
        .filter(|part| response.code == 0 && part.code == 0)
        .map(|part| part.data)
        .filter(|data| !data.basic_info.album_name.trim().is_empty())
        .ok_or_else(|| BotError::Custom("没有找到QQ音乐专辑".to_string()))?;
    let songs = response
        .songs
        .filter(|part| part.code == 0)
        .map(|part| part.data.song_list)
        .unwrap_or_default();
    let mid = if info.basic_info.album_mid.trim().is_empty() {
        mid.to_string()
    } else {
        info.basic_info.album_mid.clone()
    };
    let artist = info
        .singer
        .singer_list
        .iter()
        .map(|singer| singer.name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(" / ");
    Ok(AlbumDetail {
        platform: PLATFORM.id(),
        url: album_url(&mid),
        cover: album_cover(&mid, 800),
        id: mid,
        title: info.basic_info.album_name,
        artist,
        artist_id: String::new(),
        release_date: info.basic_info.publish_date,
        genre: info.basic_info.genre,
        copyright: info.company.name,
        tracks: songs
            .into_iter()
            .map(|entry| song_item(entry.song_info))
            .collect(),
    })
}

/// A QQ Music album or playlist named by `album:<mid>`, `playlist:<id>` or
/// its y.qq.com link.
#[derive(Debug, Eq, PartialEq)]
enum CollectionId {
    Album(String),
    Playlist(String),
}

fn parse_collection_id(text: &str) -> Option<CollectionId> {
    let text = text.trim();
    if let Some(mid) = text.strip_prefix("album:") {
        return valid_mid(mid.trim()).then(|| CollectionId::Album(mid.trim().to_string()));
    }
    if let Some(id) = text.strip_prefix("playlist:") {
        let id = id.trim();
        return (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
            .then(|| CollectionId::Playlist(id.to_string()));
    }
    let url = url::Url::parse(text).ok()?;
    if !url.host_str()?.to_ascii_lowercase().ends_with("qq.com") {
        return None;
    }
    let query = |key: &str| {
        url.query_pairs()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let after = |name: &str| {
        segments
            .iter()
            .position(|segment| segment.eq_ignore_ascii_case(name))
            .and_then(|index| segments.get(index + 1))
            .map(|value| value.trim_end_matches(".html").to_string())
    };
    if let Some(mid) = after("albumDetail")
        .or_else(|| after("album"))
        .or_else(|| query("albummid"))
        .or_else(|| query("albumMid"))
        .filter(|mid| valid_mid(mid))
    {
        return Some(CollectionId::Album(mid));
    }
    let playlist = after("playlist")
        .or_else(|| {
            segments
                .last()
                .filter(|page| page.starts_with("taoge"))
                .and_then(|_| query("id"))
        })
        .or_else(|| query("disstid"))?;
    playlist
        .chars()
        .all(|c| c.is_ascii_digit())
        .then_some(CollectionId::Playlist(playlist))
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct DissData {
    dirinfo: DissInfo,
    songlist: Vec<Song>,
    total_song_num: usize,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct DissInfo {
    title: String,
}

#[derive(Deserialize)]
struct DissResponse {
    code: i64,
    #[serde(default)]
    diss: Option<Part<DissData>>,
}

pub(in crate::music) async fn collection(
    keyword: &str,
    limit: usize,
) -> Result<Option<MusicCollection>, BotError> {
    let Some(id) = parse_collection_id(keyword) else {
        return Ok(None);
    };
    let limit = limit.max(1);
    match id {
        CollectionId::Album(mid) => {
            let album = album_detail(&mid).await?;
            let title = if album.artist.is_empty() {
                album.title
            } else {
                format!("{} - {}", album.title, album.artist)
            };
            Ok(Some(MusicCollection {
                platform: PLATFORM,
                id: album.id,
                title,
                items: album
                    .tracks
                    .into_iter()
                    .take(limit)
                    .map(search_item)
                    .collect(),
            }))
        }
        CollectionId::Playlist(id) => playlist(&id, limit).await.map(Some),
    }
}

async fn playlist(id: &str, limit: usize) -> Result<MusicCollection, BotError> {
    let disstid: u64 = id
        .parse()
        .map_err(|_| BotError::Custom("QQ音乐歌单 ID 无效".to_string()))?;
    let mut title = String::new();
    let mut items = Vec::new();
    while items.len() < limit {
        let count = PLAYLIST_PAGE.min(limit - items.len());
        let payload = json!({
            "comm": { "ct": 24, "cv": 0 },
            "diss": {
                "module": "music.srfDissInfo.aiDissInfo",
                "method": "uniform_get_Dissinfo",
                "param": {
                    "disstid": disstid, "userinfo": 1, "tag": 1, "orderlist": 1,
                    "song_begin": items.len(), "song_num": count,
                    "onlysonglist": 0, "enc_host_uin": ""
                }
            }
        });
        let body = post_json(MUSICU_ENDPOINT, &payload, false).await?;
        let response: DissResponse = serde_json::from_slice(&body)?;
        let data = response
            .diss
            .filter(|part| response.code == 0 && part.code == 0)
            .map(|part| part.data)
            .ok_or_else(|| BotError::Custom("没有找到QQ音乐歌单，可能是私密歌单".to_string()))?;
        if title.is_empty() {
            title = data.dirinfo.title;
        }
        let page = data.songlist.len();
        items.extend(
            data.songlist
                .into_iter()
                .map(|song| search_item(song_item(song))),
        );
        if page == 0 || items.len() >= data.total_song_num {
            break;
        }
    }
    items.retain(|item: &MusicSearchItem| !item.id.is_empty());
    items.truncate(limit);
    Ok(MusicCollection {
        platform: PLATFORM,
        id: id.to_string(),
        title: if title.trim().is_empty() {
            "QQ音乐歌单".to_string()
        } else {
            title
        },
        items,
    })
}

fn search_item(item: CatalogItem) -> MusicSearchItem {
    MusicSearchItem {
        platform: PLATFORM,
        id: item.id,
        song: item.title,
        singer: item.subtitle,
        cover: item.cover,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn search_maps_songs_albums_and_playlists() {
        let response: SearchResponse = parse(serde_json::json!({"code": 0,
            "song": {"code": 0, "data": {"body": {"song": {"list": [{"id": 102065756, "mid": "004Z8Ihr0JIu5s",
                "name": "七里香", "title": "七里香", "interval": 299, "index_album": 2, "index_cd": 0,
                "singer": [{"name": "周杰伦"}], "album": {"mid": "003DFRzD192KKD", "name": "七里香"}}]}}}},
            "album": {"code": 0, "data": {"body": {"album": {"list": [{"albumMID": "003DFRzD192KKD",
                "albumName": "七里香", "singerName": "周杰伦", "publicTime": "2004-08-03"}]}}}},
            "playlist": {"code": 2001, "data": {}}
        }));
        let mut found = search_from_response(response);
        found.rank_top("七里香");
        let song = &found.songs[0];
        assert_eq!(song.id, "004Z8Ihr0JIu5s");
        assert_eq!(
            song.cover,
            "https://y.gtimg.cn/music/photo_new/T002M000003DFRzD192KKD.jpg"
        );
        assert_eq!(
            (song.duration_ms, song.track_number, song.disc_number),
            (Some(299_000), Some(2), Some(1))
        );
        assert_eq!(
            found.albums[0].url,
            "https://y.qq.com/n/ryqq/albumDetail/003DFRzD192KKD"
        );
        assert!(found.playlists.is_empty());
        // The album named exactly like the query leads the best results.
        assert_eq!(found.top[0].kind, CatalogKind::Album);
    }

    #[test]
    fn a_failed_section_stays_empty() {
        let response: SearchResponse = parse(
            serde_json::json!({"code": 0, "song": {"code": 2001, "data": {}},
            "album": {"code": 0, "data": {"body": {"album": {"list": [{"albumMID": "1", "albumName": "A"}]}}}}}),
        );
        let found = search_from_response(response);
        assert!(found.songs.is_empty());
        assert_eq!(found.albums.len(), 1);
    }

    #[test]
    fn album_detail_reads_info_and_songs() {
        let response: AlbumResponse = parse(serde_json::json!({"code": 0,
            "album": {"code": 0, "data": {"basicInfo": {"albumMid": "0041WVfh2vtlJE", "albumName": "太阳之子",
                "publishDate": "2026-03-25"}, "singer": {"singerList": [{"name": "周杰伦"}]},
                "company": {"name": "杰威尔"}}},
            "songs": {"code": 0, "data": {"songList": [{"songInfo": {"mid": "0001bLnu2ULqYf", "name": "太阳之子",
                "singer": [{"name": "周杰伦"}], "album": {"mid": "0041WVfh2vtlJE", "name": "太阳之子"},
                "interval": 297, "index_album": 1, "index_cd": 0}}]}}}));
        let album = album_from_response(response, "0041WVfh2vtlJE").unwrap();
        assert_eq!(album.artist, "周杰伦");
        assert_eq!(album.copyright, "杰威尔");
        assert!(album.artist_id.is_empty());
        assert_eq!(album.tracks[0].album, "太阳之子");
        assert!(album.cover.contains("T002R800x800M0000041WVfh2vtlJE"));
    }

    #[test]
    fn parses_album_and_playlist_links() {
        assert_eq!(
            parse_collection_id("album:003DFRzD192KKD"),
            Some(CollectionId::Album("003DFRzD192KKD".into()))
        );
        assert_eq!(
            parse_collection_id("playlist:7039749142"),
            Some(CollectionId::Playlist("7039749142".into()))
        );
        assert_eq!(
            parse_collection_id("https://y.qq.com/n/ryqq/albumDetail/0041WVfh2vtlJE"),
            Some(CollectionId::Album("0041WVfh2vtlJE".into()))
        );
        assert_eq!(
            parse_collection_id("https://y.qq.com/n/ryqq/playlist/7039749142"),
            Some(CollectionId::Playlist("7039749142".into()))
        );
        assert_eq!(
            parse_collection_id(
                "https://i.y.qq.com/n2/m/share/details/taoge.html?platform=11&id=7039749142"
            ),
            Some(CollectionId::Playlist("7039749142".into()))
        );
        assert_eq!(
            parse_collection_id("https://y.qq.com/n/ryqq/songDetail/0039MnYb0qxYhV"),
            None
        );
        assert_eq!(parse_collection_id("七里香"), None);
    }
}
