//! NetEase catalog search with every section, album and artist pages.

use super::{ensure_netease_ok, get_netease_json};
use crate::BotError;
use crate::music::TrackInfo;
use crate::music::catalog::{
    AlbumDetail, ArtistDetail, CatalogItem, CatalogKind, CatalogSearch, date_from_millis,
};
use crate::music::provider::{MusicPlatform, json_id_to_string};
use serde::Deserialize;

const PLATFORM: MusicPlatform = MusicPlatform::Netease;

#[derive(Deserialize)]
struct SearchResponse {
    code: Option<i64>,
    #[serde(default)]
    result: SearchResult,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SearchResult {
    songs: Vec<Song>,
    albums: Vec<Album>,
    artists: Vec<Artist>,
    playlists: Vec<Playlist>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Song {
    id: serde_json::Value,
    name: String,
    #[serde(alias = "artists")]
    ar: Vec<Artist>,
    #[serde(alias = "album")]
    al: Option<Album>,
    /// Milliseconds: `dt` in cloud search and v1 details, `duration` elsewhere.
    #[serde(alias = "duration")]
    dt: Option<u64>,
    no: Option<u32>,
    #[serde(alias = "disc")]
    cd: Option<serde_json::Value>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Album {
    id: serde_json::Value,
    name: String,
    #[serde(rename = "picUrl")]
    pic_url: Option<String>,
    artist: Option<Artist>,
    artists: Vec<Artist>,
    #[serde(rename = "publishTime")]
    publish_time: Option<i64>,
    company: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "subType")]
    sub_type: Option<String>,
    songs: Vec<Song>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Artist {
    id: serde_json::Value,
    name: String,
    #[serde(rename = "picUrl")]
    pic_url: Option<String>,
    #[serde(rename = "img1v1Url")]
    img1v1_url: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Playlist {
    id: serde_json::Value,
    name: String,
    #[serde(rename = "coverImgUrl")]
    cover_img_url: Option<String>,
    creator: Option<Creator>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Creator {
    nickname: String,
}

pub(in crate::music) async fn catalog_search(
    keyword: &str,
    limit: usize,
) -> Result<CatalogSearch, BotError> {
    let search = |kind: u32, limit: usize| async move {
        let url = format!(
            "https://music.163.com/api/cloudsearch/pc?s={}&type={kind}&offset=0&limit={limit}",
            urlencoding::encode(keyword)
        );
        let response: SearchResponse = get_netease_json(&url).await?;
        ensure_netease_ok(response.code, None)?;
        Ok::<_, BotError>(response.result)
    };
    // Songs carry the search; a failed side section only empties its tab.
    let (songs, albums, artists, playlists) = tokio::join!(
        search(1, limit),
        search(10, limit.min(12)),
        search(100, limit.min(8)),
        search(1000, limit.min(12)),
    );
    let mut found = CatalogSearch::new(PLATFORM);
    found.songs = songs?.songs.into_iter().map(song_item).collect();
    found.albums = albums
        .map(|result| result.albums.into_iter().map(album_item).collect())
        .unwrap_or_default();
    found.artists = artists
        .map(|result| result.artists.into_iter().map(artist_item).collect())
        .unwrap_or_default();
    found.playlists = playlists
        .map(|result| result.playlists.into_iter().map(playlist_item).collect())
        .unwrap_or_default();
    found.rank_top(keyword);
    Ok(found)
}

fn artists_text(artists: &[Artist]) -> String {
    let names = artists
        .iter()
        .map(|artist| artist.name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    if names.is_empty() {
        "未知歌手".to_string()
    } else {
        names.join(" / ")
    }
}

fn song_item(song: Song) -> CatalogItem {
    let album = song.al.unwrap_or_default();
    let mut item = CatalogItem::new(CatalogKind::Song, PLATFORM, json_id_to_string(song.id))
        .title(song.name)
        .subtitle(artists_text(&song.ar))
        .album(album.name)
        .cover(album.pic_url.unwrap_or_default());
    item.duration_ms = song.dt.filter(|ms| *ms > 0);
    item.track_number = song.no.filter(|no| *no > 0);
    item.disc_number = song.cd.as_ref().and_then(disc_number);
    item
}

/// Discs come as numbers or strings such as `"1"` and `"01"`.
fn disc_number(value: &serde_json::Value) -> Option<u32> {
    match value {
        serde_json::Value::Number(n) => n.as_u64().map(|n| n as u32),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|disc| *disc > 0)
}

fn album_artist(album: &Album) -> Option<&Artist> {
    album
        .artist
        .as_ref()
        .filter(|artist| !artist.name.trim().is_empty())
        .or_else(|| album.artists.first())
}

fn album_item(album: Album) -> CatalogItem {
    let id = json_id_to_string(album.id.clone());
    let artist = album_artist(&album)
        .map(|artist| artist.name.clone())
        .unwrap_or_default();
    CatalogItem::new(CatalogKind::Album, PLATFORM, id.clone())
        .title(album.name)
        .subtitle(artist)
        .cover(album.pic_url.unwrap_or_default())
        .release_date(date_from_millis(album.publish_time.unwrap_or_default()))
        .url(album_url(&id))
}

fn album_url(id: &str) -> String {
    format!("https://music.163.com/#/album?id={id}")
}

fn artist_item(artist: Artist) -> CatalogItem {
    let cover = artist
        .pic_url
        .or(artist.img1v1_url)
        // NetEase's placeholder avatar for artists without a photo.
        .filter(|url| !url.contains("5639395138885805"))
        .unwrap_or_default();
    CatalogItem::new(CatalogKind::Artist, PLATFORM, json_id_to_string(artist.id))
        .title(artist.name)
        .cover(cover)
}

fn playlist_item(playlist: Playlist) -> CatalogItem {
    let id = json_id_to_string(playlist.id);
    CatalogItem::new(CatalogKind::Playlist, PLATFORM, id.clone())
        .title(playlist.name)
        .subtitle(playlist.creator.map(|c| c.nickname).unwrap_or_default())
        .cover(playlist.cover_img_url.unwrap_or_default())
        .url(format!("https://music.163.com/#/playlist?id={id}"))
}

/// Unknown artists and albums carry id 0, which opens no page.
fn nonzero_id(value: serde_json::Value) -> String {
    let id = json_id_to_string(value);
    if id.trim_start_matches('0').is_empty() {
        String::new()
    } else {
        id
    }
}

fn numeric(id: &str) -> Result<(), BotError> {
    if super::is_netease_numeric_id(id) {
        Ok(())
    } else {
        Err(BotError::Custom("网易云 ID 无效".to_string()))
    }
}

#[derive(Deserialize)]
struct AlbumResponse {
    code: Option<i64>,
    album: Option<Album>,
    /// Some answers list the songs beside the album instead of inside it.
    #[serde(default)]
    songs: Vec<Song>,
}

pub(in crate::music) async fn album_detail(id: &str) -> Result<AlbumDetail, BotError> {
    numeric(id)?;
    // The older `/api/album/<id>` answers -462 (verification) to anonymous
    // clients; v1 lists the songs beside the album.
    let url = format!("https://music.163.com/api/v1/album/{id}");
    let response: AlbumResponse = get_netease_json(&url).await?;
    ensure_netease_ok(response.code, None)?;
    album_from_response(response)
}

fn album_from_response(response: AlbumResponse) -> Result<AlbumDetail, BotError> {
    let mut album = response
        .album
        .ok_or_else(|| BotError::Custom("没有找到网易云专辑".to_string()))?;
    let songs = if album.songs.is_empty() {
        response.songs
    } else {
        std::mem::take(&mut album.songs)
    };
    let id = json_id_to_string(album.id.clone());
    let cover = album.pic_url.clone().unwrap_or_default();
    let (artist, artist_id) = album_artist(&album)
        .map(|artist| (artist.name.clone(), nonzero_id(artist.id.clone())))
        .unwrap_or_default();
    let tracks = songs
        .into_iter()
        .map(song_item)
        .map(|mut track| {
            // Album songs name the album but often without its cover.
            if track.cover.is_empty() {
                track.cover = cover.clone();
            }
            if track.album.is_empty() {
                track.album = album.name.clone();
            }
            track
        })
        .collect();
    Ok(AlbumDetail {
        platform: PLATFORM.id(),
        url: album_url(&id),
        artist_id,
        id,
        title: album.name,
        artist,
        cover,
        release_date: date_from_millis(album.publish_time.unwrap_or_default()),
        genre: album.sub_type.unwrap_or_default(),
        copyright: album.company.unwrap_or_default(),
        tracks,
    })
}

#[derive(Deserialize)]
struct ArtistResponse {
    code: Option<i64>,
    artist: Option<Artist>,
    #[serde(default, rename = "hotSongs")]
    hot_songs: Vec<Song>,
}

#[derive(Deserialize)]
struct ArtistAlbumsResponse {
    code: Option<i64>,
    #[serde(default, rename = "hotAlbums")]
    hot_albums: Vec<Album>,
}

pub(in crate::music) async fn artist_detail(id: &str) -> Result<ArtistDetail, BotError> {
    numeric(id)?;
    let artist_url = format!("https://music.163.com/api/v1/artist/{id}");
    let albums_url = format!("https://music.163.com/api/artist/albums/{id}?limit=60&offset=0");
    let (artist, albums) = tokio::join!(
        get_netease_json::<ArtistResponse>(&artist_url),
        get_netease_json::<ArtistAlbumsResponse>(&albums_url),
    );
    let artist = artist?;
    ensure_netease_ok(artist.code, None)?;
    // The page still opens with top songs when the album list fails.
    let albums = albums
        .ok()
        .filter(|albums| matches!(albums.code, None | Some(200)))
        .map(|albums| albums.hot_albums)
        .unwrap_or_default();
    artist_from_parts(artist, albums)
}

fn artist_from_parts(
    response: ArtistResponse,
    albums: Vec<Album>,
) -> Result<ArtistDetail, BotError> {
    let artist = response
        .artist
        .ok_or_else(|| BotError::Custom("没有找到网易云艺人".to_string()))?;
    let artist = artist_item(artist);
    let (singles, albums): (Vec<_>, Vec<_>) = albums
        .into_iter()
        .partition(|album| matches!(album.kind.as_deref(), Some("Single" | "EP" | "EP/Single")));
    Ok(ArtistDetail {
        platform: PLATFORM.id(),
        id: artist.id,
        name: artist.title,
        cover: artist.cover,
        top_songs: response
            .hot_songs
            .into_iter()
            .take(20)
            .map(song_item)
            .collect(),
        albums: albums.into_iter().map(album_item).collect(),
        singles: singles.into_iter().map(album_item).collect(),
    })
}

#[derive(Deserialize)]
struct DetailResponse {
    code: Option<i64>,
    #[serde(default)]
    songs: Vec<Song>,
}

pub(in crate::music) async fn track_info(id: &str) -> Result<TrackInfo, BotError> {
    numeric(id)?;
    let url = format!(
        "https://music.163.com/api/song/detail?ids=[{}]",
        urlencoding::encode(id)
    );
    let response: DetailResponse = get_netease_json(&url).await?;
    ensure_netease_ok(response.code, None)?;
    let song = response
        .songs
        .into_iter()
        .next()
        .ok_or_else(|| BotError::Custom("没有找到歌曲详情".to_string()))?;
    Ok(track_info_from_song(id, song))
}

fn track_info_from_song(id: &str, song: Song) -> TrackInfo {
    let album = song.al.clone().unwrap_or_default();
    TrackInfo {
        platform: PLATFORM.id(),
        id: id.to_string(),
        singer: artists_text(&song.ar),
        artist_id: song
            .ar
            .first()
            .map(|artist| nonzero_id(artist.id.clone()))
            .unwrap_or_default(),
        song: song.name,
        album_id: nonzero_id(album.id),
        album: album.name,
        cover: album.pic_url.unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn album_reads_songs_inside_or_beside_the_album() {
        let inside: AlbumResponse = parse(serde_json::json!({"code": 200, "album": {
            "id": 18909, "name": "Partners 拍档", "picUrl": "https://p1/a.jpg",
            "publishTime": 1019750400000_i64, "company": "亚歆",
            "artist": {"id": 6452, "name": "周杰伦"},
            "songs": [{"id": 186064, "name": "刀马旦", "duration": 192000, "no": 1, "disc": "1",
                       "artists": [{"id": 6452, "name": "周杰伦"}], "album": {"id": 18909, "name": "Partners 拍档"}}]
        }}));
        let album = album_from_response(inside).unwrap();
        assert_eq!(album.artist_id, "6452");
        assert_eq!(album.release_date, "2002-04-26");
        assert_eq!(album.url, "https://music.163.com/#/album?id=18909");
        let track = &album.tracks[0];
        assert_eq!(track.cover, "https://p1/a.jpg");
        assert_eq!(
            (track.track_number, track.disc_number, track.duration_ms),
            (Some(1), Some(1), Some(192000))
        );

        let beside: AlbumResponse = parse(serde_json::json!({"code": 200,
            "album": {"id": 1, "name": "A", "artist": {"id": 0, "name": ""}, "artists": [{"id": 7, "name": "B"}]},
            "songs": [{"id": 2, "name": "S", "ar": [{"id": 7, "name": "B"}], "al": {"id": 1, "name": "A", "picUrl": "c"}, "dt": 1000, "cd": "01"}]}));
        let album = album_from_response(beside).unwrap();
        assert_eq!(
            (album.artist.as_str(), album.artist_id.as_str()),
            ("B", "7")
        );
        assert_eq!(album.tracks[0].disc_number, Some(1));
        assert_eq!(album.tracks[0].cover, "c");
    }

    #[test]
    fn artist_splits_singles_from_albums() {
        let response: ArtistResponse = parse(serde_json::json!({"code": 200,
            "artist": {"id": 6452, "name": "周杰伦", "picUrl": "https://p2/jay.jpg"},
            "hotSongs": [{"id": 1, "name": "晴天", "ar": [{"id": 6452, "name": "周杰伦"}], "al": {"id": 3, "name": "叶惠美", "picUrl": "c"}}]}));
        let albums: Vec<Album> = parse(serde_json::json!([
            {"id": 403900711, "name": "太阳之子", "type": "专辑", "publishTime": 1774409488576_i64},
            {"id": 410149064, "name": "恒星不忘", "type": "Single"}
        ]));
        let artist = artist_from_parts(response, albums).unwrap();
        assert_eq!(artist.cover, "https://p2/jay.jpg");
        assert_eq!(artist.top_songs[0].album, "叶惠美");
        assert_eq!(artist.albums.len(), 1);
        assert_eq!(artist.singles[0].title, "恒星不忘");
    }

    #[test]
    fn track_info_links_the_album_and_first_artist() {
        let song: Song = parse(serde_json::json!({"id": 186001, "name": "七里香",
            "artists": [{"id": 6452, "name": "周杰伦"}],
            "album": {"id": 18903, "name": "七里香", "picUrl": "https://p2/qlx.jpg"}}));
        let info = track_info_from_song("186001", song);
        assert_eq!(
            (info.album_id.as_str(), info.artist_id.as_str()),
            ("18903", "6452")
        );
        assert_eq!(info.cover, "https://p2/qlx.jpg");

        let anonymous: Song = parse(serde_json::json!({"id": 1, "name": "x",
            "artists": [{"id": 0, "name": "RyaVocal"}], "album": {"id": 0, "name": ""}}));
        let info = track_info_from_song("1", anonymous);
        assert!(info.album_id.is_empty() && info.artist_id.is_empty());
    }

    #[test]
    fn artist_placeholder_avatar_is_dropped() {
        let artist: Artist = parse(serde_json::json!({"id": 1, "name": "x",
            "img1v1Url": "https://p1.music.126.net/6y-UleORITEDbvrOLV0Q8A==/5639395138885805.jpg"}));
        assert_eq!(artist_item(artist).cover, "");
    }
}
