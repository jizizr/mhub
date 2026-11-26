//! Apple Music catalog search with every section, album and artist pages.

use super::{
    APPLE_MUSIC_API, AppleMusicArtwork, DEFAULT_ARTWORK_SIZE, apple_collection_next_url,
    apple_get_json, apple_language, apple_storefront, ensure_apple_enabled, format_artwork_url,
};
use crate::BotError;
use crate::music::TrackInfo;
use crate::music::catalog::{AlbumDetail, ArtistDetail, CatalogItem, CatalogKind, CatalogSearch};
use crate::music::provider::MusicPlatform;
use serde::Deserialize;
use url::Url;

/// Covers in lists; the album page and songs use the full size.
const LIST_ARTWORK_SIZE: usize = 600;
const MAX_ALBUM_TRACKS: usize = 500;

#[derive(Deserialize)]
struct SearchResponse {
    results: Option<SearchResults>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SearchResults {
    #[serde(rename = "topResults")]
    top_results: List,
    artists: List,
    albums: List,
    songs: List,
    playlists: List,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct List {
    data: Vec<Resource>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    data: Vec<Resource>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Resource {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    attributes: Attributes,
    relationships: Relationships,
    views: Views,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Attributes {
    name: String,
    #[serde(rename = "artistName")]
    artist_name: String,
    #[serde(rename = "albumName")]
    album_name: String,
    #[serde(rename = "curatorName")]
    curator_name: String,
    #[serde(rename = "releaseDate")]
    release_date: String,
    #[serde(rename = "genreNames")]
    genre_names: Vec<String>,
    copyright: String,
    #[serde(rename = "trackNumber")]
    track_number: Option<u32>,
    #[serde(rename = "discNumber")]
    disc_number: Option<u32>,
    #[serde(rename = "durationInMillis")]
    duration_in_millis: Option<u64>,
    artwork: Option<AppleMusicArtwork>,
    url: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Relationships {
    tracks: List,
    artists: List,
    albums: List,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Views {
    #[serde(rename = "top-songs")]
    top_songs: List,
    #[serde(rename = "full-albums")]
    full_albums: List,
    singles: List,
}

fn catalog_url(path: &str, query: &[(&str, &str)]) -> String {
    let mut url = Url::parse(&format!(
        "{APPLE_MUSIC_API}/v1/catalog/{}/{path}",
        apple_storefront()
    ))
    .unwrap();
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
        pairs.append_pair("l", &apple_language());
    }
    url.to_string()
}

pub(in crate::music) async fn catalog_search(
    keyword: &str,
    limit: usize,
) -> Result<CatalogSearch, BotError> {
    ensure_apple_enabled()?;
    let limit = limit.clamp(1, 25).to_string();
    let url = catalog_url(
        "search",
        &[
            ("term", keyword),
            ("types", "songs,albums,artists,playlists"),
            ("with", "topResults"),
            ("limit", &limit),
        ],
    );
    let response: SearchResponse = apple_get_json(&url, true).await?;
    Ok(search_from_response(response))
}

fn search_from_response(response: SearchResponse) -> CatalogSearch {
    let results = response.results.unwrap_or_default();
    let mut search = CatalogSearch::new(MusicPlatform::AppleMusic);
    search.top = items(results.top_results.data);
    search.artists = items(results.artists.data);
    search.albums = items(results.albums.data);
    search.songs = items(results.songs.data);
    search.playlists = items(results.playlists.data);
    // A catalog without a ranking still gets a best-results tab.
    if search.top.is_empty() {
        search.top = search.songs.iter().take(4).cloned().collect();
    }
    search
}

fn items(resources: Vec<Resource>) -> Vec<CatalogItem> {
    resources.into_iter().filter_map(item).collect()
}

/// Songs, albums, artists and playlists; other kinds (videos, stations,
/// curators) have no page here and are left out.
fn item(resource: Resource) -> Option<CatalogItem> {
    let Resource {
        id,
        kind,
        attributes,
        ..
    } = resource;
    let platform = MusicPlatform::AppleMusic;
    let item = match kind.as_str() {
        "songs" => CatalogItem::new(CatalogKind::Song, platform, id)
            .title(attributes.name)
            .subtitle(attributes.artist_name)
            .album(attributes.album_name)
            .cover(format_artwork_url(
                attributes.artwork.as_ref(),
                DEFAULT_ARTWORK_SIZE,
            )),
        "albums" => {
            let url = collection_url(&attributes.url, &format!("album:{id}"));
            CatalogItem::new(CatalogKind::Album, platform, id)
                .title(attributes.name)
                .subtitle(attributes.artist_name)
                .release_date(attributes.release_date)
                .cover(format_artwork_url(
                    attributes.artwork.as_ref(),
                    LIST_ARTWORK_SIZE,
                ))
                .url(url)
        }
        "artists" => CatalogItem::new(CatalogKind::Artist, platform, id)
            .title(attributes.name)
            .cover(format_artwork_url(
                attributes.artwork.as_ref(),
                LIST_ARTWORK_SIZE,
            )),
        "playlists" => {
            let url = collection_url(&attributes.url, &id);
            CatalogItem::new(CatalogKind::Playlist, platform, id)
                .title(attributes.name)
                .subtitle(attributes.curator_name)
                .cover(format_artwork_url(
                    attributes.artwork.as_ref(),
                    LIST_ARTWORK_SIZE,
                ))
                .url(url)
        }
        _ => return None,
    };
    let mut item = item;
    if item.kind == CatalogKind::Song {
        item.duration_ms = attributes.duration_in_millis;
        item.track_number = attributes.track_number;
        item.disc_number = attributes.disc_number;
    }
    Some(item)
}

/// The catalog's public link when it is one; otherwise the id form.
fn collection_url(public: &str, fallback: &str) -> String {
    if public.starts_with("https://music.apple.com/") {
        public.to_string()
    } else {
        fallback.to_string()
    }
}

pub(in crate::music) async fn album_detail(id: &str) -> Result<AlbumDetail, BotError> {
    ensure_apple_enabled()?;
    if !super::is_apple_numeric_id(id) {
        return Err(BotError::Custom("Apple Music 专辑 ID 无效".to_string()));
    }
    let url = catalog_url(&format!("albums/{id}"), &[("include", "tracks,artists")]);
    let response: Response = apple_get_json(&url, true).await?;
    let album = response
        .data
        .into_iter()
        .next()
        .ok_or_else(|| BotError::Custom("没有找到 Apple Music 专辑".to_string()))?;
    let Relationships {
        mut tracks,
        artists,
        ..
    } = album.relationships;
    // Box sets list their tracks over several pages.
    let mut pages = 0;
    while let Some(next) = tracks.next.take().filter(|next| !next.trim().is_empty()) {
        if tracks.data.len() >= MAX_ALBUM_TRACKS || pages >= 5 {
            break;
        }
        pages += 1;
        let page: List = apple_get_json(&apple_collection_next_url(&next)?, true).await?;
        tracks.data.extend(page.data);
        tracks.next = page.next;
    }
    Ok(album_from_parts(
        album.id,
        album.attributes,
        tracks.data,
        artists.data,
    ))
}

fn album_from_parts(
    id: String,
    attributes: Attributes,
    tracks: Vec<Resource>,
    artists: Vec<Resource>,
) -> AlbumDetail {
    let cover = format_artwork_url(attributes.artwork.as_ref(), DEFAULT_ARTWORK_SIZE);
    let mut tracks: Vec<CatalogItem> = tracks
        .into_iter()
        .filter(|track| track.kind == "songs")
        .filter_map(item)
        .collect();
    for track in &mut tracks {
        if track.cover.is_empty() {
            track.cover = cover.clone();
        }
        if track.album.is_empty() {
            track.album = attributes.name.clone();
        }
    }
    tracks.truncate(MAX_ALBUM_TRACKS);
    AlbumDetail {
        platform: MusicPlatform::AppleMusic.id(),
        url: collection_url(&attributes.url, &format!("album:{id}")),
        id,
        title: attributes.name,
        artist: attributes.artist_name,
        artist_id: artists
            .into_iter()
            .map(|artist| artist.id)
            .find(|id| super::is_apple_numeric_id(id))
            .unwrap_or_default(),
        cover,
        release_date: attributes.release_date,
        // "Music" is the store's root genre, never a useful label.
        genre: attributes
            .genre_names
            .into_iter()
            .find(|genre| !matches!(genre.as_str(), "Music" | "音乐" | "音樂"))
            .unwrap_or_default(),
        copyright: attributes.copyright,
        tracks,
    }
}

pub(in crate::music) async fn artist_detail(id: &str) -> Result<ArtistDetail, BotError> {
    ensure_apple_enabled()?;
    if !super::is_apple_numeric_id(id) {
        return Err(BotError::Custom("Apple Music 艺人 ID 无效".to_string()));
    }
    let url = catalog_url(
        &format!("artists/{id}"),
        &[("views", "top-songs,full-albums,singles")],
    );
    let response: Response = apple_get_json(&url, true).await?;
    let artist = response
        .data
        .into_iter()
        .next()
        .ok_or_else(|| BotError::Custom("没有找到 Apple Music 艺人".to_string()))?;
    let Views {
        top_songs,
        full_albums,
        singles,
    } = artist.views;
    // A view's first page holds ten; the view endpoint takes a larger limit.
    let (top_songs, full_albums, singles) = tokio::join!(
        whole_view(&artist.id, "top-songs", top_songs, 20),
        whole_view(&artist.id, "full-albums", full_albums, 50),
        whole_view(&artist.id, "singles", singles, 30),
    );
    Ok(ArtistDetail {
        platform: MusicPlatform::AppleMusic.id(),
        id: artist.id,
        name: artist.attributes.name,
        cover: format_artwork_url(artist.attributes.artwork.as_ref(), DEFAULT_ARTWORK_SIZE),
        top_songs: items(top_songs),
        albums: items(full_albums),
        singles: items(singles),
    })
}

/// The view's data, refetched with `limit` when the first page has more.
/// A failed refetch keeps the first page.
async fn whole_view(artist_id: &str, view: &str, first: List, limit: usize) -> Vec<Resource> {
    if first
        .next
        .as_deref()
        .is_none_or(|next| next.trim().is_empty())
    {
        return first.data;
    }
    let url = catalog_url(
        &format!("artists/{artist_id}/view/{view}"),
        &[("limit", &limit.to_string())],
    );
    match apple_get_json::<List>(&url, true).await {
        Ok(page) if page.data.len() > first.data.len() => page.data,
        _ => first.data,
    }
}

/// A song's catalog details with the album and artist pages it links to.
pub(in crate::music) async fn track_info(id: &str) -> Result<TrackInfo, BotError> {
    ensure_apple_enabled()?;
    let response: Response = apple_get_json(&super::apple_song_url(id), true).await?;
    let song = response
        .data
        .into_iter()
        .next()
        .ok_or_else(|| BotError::Custom("没有找到 Apple Music 歌曲详情".to_string()))?;
    Ok(track_info_from_song(song))
}

/// [`track_info`] as listed in another storefront, with the song's length.
pub(in crate::music) async fn track_info_in(
    storefront: &str,
    id: &str,
) -> Result<(TrackInfo, Option<u32>), BotError> {
    ensure_apple_enabled()?;
    let response: Response =
        apple_get_json(&super::apple_song_url_in(storefront, id), true).await?;
    let song = response
        .data
        .into_iter()
        .next()
        .ok_or_else(|| BotError::Custom("没有找到 Apple Music 歌曲详情".to_string()))?;
    let duration = song
        .attributes
        .duration_in_millis
        .map(|millis| ((millis + 500) / 1000) as u32);
    Ok((track_info_from_song(song), duration))
}

fn track_info_from_song(song: Resource) -> TrackInfo {
    let (album_id, artist_id) = links_from_song(&song);
    let attributes = song.attributes;
    TrackInfo {
        platform: MusicPlatform::AppleMusic.id(),
        id: song.id,
        cover: format_artwork_url(attributes.artwork.as_ref(), DEFAULT_ARTWORK_SIZE),
        song: attributes.name,
        singer: attributes.artist_name,
        album: attributes.album_name,
        album_id,
        artist_id,
    }
}

fn links_from_song(song: &Resource) -> (String, String) {
    let first = |list: &List| {
        list.data
            .iter()
            .map(|resource| resource.id.clone())
            .find(|id| super::is_apple_numeric_id(id))
            .unwrap_or_default()
    };
    let album = first(&song.relationships.albums);
    // Older catalog answers may omit relationships; the song's link names
    // its album (`/album/<name>/<id>?i=<song>`).
    let album = if album.is_empty() {
        album_id_from_song_url(&song.attributes.url)
    } else {
        album
    };
    (album, first(&song.relationships.artists))
}

fn album_id_from_song_url(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| {
            let mut segments = url.path_segments()?.collect::<Vec<_>>();
            let album = segments.iter().position(|segment| *segment == "album")?;
            segments.drain(..=album);
            segments.last().map(|id| id.to_string())
        })
        .filter(|id| super::is_apple_numeric_id(id))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    fn artwork() -> serde_json::Value {
        serde_json::json!({"url": "https://is1-ssl.mzstatic.com/x/{w}x{h}bb.jpg"})
    }

    #[test]
    fn maps_every_section_and_keeps_the_catalog_ranking() {
        let response: SearchResponse = parse(serde_json::json!({"results": {
            "topResults": {"data": [
                {"id": "300117743", "type": "artists", "attributes": {"name": "周杰伦", "artwork": artwork()}},
                {"id": "1", "type": "music-videos", "attributes": {"name": "MV"}},
                {"id": "536115195", "type": "songs", "attributes": {"name": "七里香", "artistName": "周杰伦", "albumName": "七里香", "artwork": artwork(), "durationInMillis": 297200}}
            ]},
            "albums": {"data": [{"id": "535739206", "type": "albums", "attributes": {
                "name": "范特西", "artistName": "周杰伦", "releaseDate": "2001-09-14", "artwork": artwork(),
                "url": "https://music.apple.com/cn/album/x/535739206"}}]},
            "playlists": {"data": [{"id": "pl.d467", "type": "playlists", "attributes": {
                "name": "周杰伦代表作", "curatorName": "Apple Music 国语流行", "artwork": artwork()}}]}
        }}));
        let search = search_from_response(response);
        let top: Vec<_> = search.top.iter().map(|item| item.kind).collect();
        assert_eq!(top, vec![CatalogKind::Artist, CatalogKind::Song]);
        assert_eq!(search.top[1].album, "七里香");
        assert_eq!(search.top[1].duration_ms, Some(297200));
        assert!(search.top[1].cover.contains("1200x1200"));
        let album = &search.albums[0];
        assert_eq!(album.subtitle, "周杰伦");
        assert_eq!(album.release_date, "2001-09-14");
        assert_eq!(album.url, "https://music.apple.com/cn/album/x/535739206");
        assert!(album.cover.contains("600x600"));
        assert_eq!(search.playlists[0].subtitle, "Apple Music 国语流行");
        assert_eq!(search.playlists[0].url, "pl.d467");
        assert!(search.songs.is_empty());
    }

    #[test]
    fn album_tracks_carry_numbers_and_the_album_artist_id() {
        let attributes: Attributes = parse(serde_json::json!({
            "name": "范特西", "artistName": "周杰伦", "releaseDate": "2001-09-14",
            "genreNames": ["音乐", "国语流行"], "copyright": "℗ 2001 JVR", "artwork": artwork()
        }));
        let tracks: Vec<Resource> = parse(serde_json::json!([
            {"id": "1", "type": "songs", "attributes": {"name": "爱在西元前", "artistName": "周杰伦",
                "trackNumber": 1, "discNumber": 1, "durationInMillis": 234253}},
            {"id": "2", "type": "music-videos", "attributes": {"name": "MV"}}
        ]));
        let artists: Vec<Resource> =
            parse(serde_json::json!([{"id": "300117743", "type": "artists"}]));
        let album = album_from_parts("535739206".to_string(), attributes, tracks, artists);
        assert_eq!(album.artist_id, "300117743");
        assert_eq!(album.genre, "国语流行");
        assert_eq!(album.url, "album:535739206");
        assert_eq!(album.tracks.len(), 1);
        let track = &album.tracks[0];
        assert_eq!((track.track_number, track.disc_number), (Some(1), Some(1)));
        assert_eq!(track.album, "范特西");
        assert_eq!(track.cover, album.cover);
    }

    #[test]
    fn song_links_fall_back_to_the_album_in_its_url() {
        let song: Resource = parse(
            serde_json::json!({"id": "536115195", "type": "songs", "attributes": {
            "name": "七里香", "url": "https://music.apple.com/cn/album/%E4%B8%83/536114662?i=536115195"},
            "relationships": {"artists": {"data": [{"id": "300117743", "type": "artists"}]}}}),
        );
        let info = track_info_from_song(song);
        assert_eq!(
            (
                info.album_id.as_str(),
                info.artist_id.as_str(),
                info.song.as_str()
            ),
            ("536114662", "300117743", "七里香")
        );
    }
}
