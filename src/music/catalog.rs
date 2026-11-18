//! Catalog browsing like Apple Music's search page: songs, albums, artists
//! and playlists, plus album and artist pages. Only platforms whose own
//! catalog has a section list it; the others keep the plain song search.

use super::provider::MusicPlatform;
use serde::Serialize;

/// A search section, in the order clients show them as tabs.
pub const TOP: &str = "top";
pub const ARTISTS: &str = "artists";
pub const ALBUMS: &str = "albums";
pub const SONGS: &str = "songs";
pub const PLAYLISTS: &str = "playlists";

/// The sections a platform's catalog search fills. A platform without album
/// pages lists only `songs`, which is the plain song search.
pub fn sections(platform: MusicPlatform) -> &'static [&'static str] {
    match platform {
        MusicPlatform::AppleMusic | MusicPlatform::Netease => {
            &[TOP, ARTISTS, ALBUMS, SONGS, PLAYLISTS]
        }
        // QQ Music's singer search is closed to anonymous clients.
        MusicPlatform::Tencent => &[TOP, ALBUMS, SONGS, PLAYLISTS],
        _ => &[SONGS],
    }
}

pub fn has_albums(platform: MusicPlatform) -> bool {
    sections(platform).contains(&ALBUMS)
}

pub fn has_artists(platform: MusicPlatform) -> bool {
    sections(platform).contains(&ARTISTS)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogKind {
    Song,
    Album,
    Artist,
    Playlist,
}

/// One catalog entry of any kind. Every section uses this one shape, so a
/// client decodes a single type and picks the row style by `type`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CatalogItem {
    #[serde(rename = "type")]
    pub kind: CatalogKind,
    pub platform: &'static str,
    pub id: String,
    /// Song name, album title, artist name or playlist name.
    pub title: String,
    /// Song: its artists. Album: its artist. Playlist: its curator.
    pub subtitle: String,
    pub cover: String,
    /// Songs: the album name.
    pub album: String,
    /// Albums: `YYYY-MM-DD` or `YYYY` when the platform knows it.
    pub release_date: String,
    /// Albums and playlists: the link `collections/resolve` accepts, which
    /// is also what a saved playlist keeps as its source.
    pub url: String,
    /// Album tracks: position on the disc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl CatalogItem {
    pub fn new(kind: CatalogKind, platform: MusicPlatform, id: impl Into<String>) -> Self {
        Self {
            kind,
            platform: platform.id(),
            id: id.into(),
            title: String::new(),
            subtitle: String::new(),
            cover: String::new(),
            album: String::new(),
            release_date: String::new(),
            url: String::new(),
            track_number: None,
            disc_number: None,
            duration_ms: None,
        }
    }

    pub fn title(mut self, value: impl Into<String>) -> Self {
        self.title = value.into();
        self
    }

    pub fn subtitle(mut self, value: impl Into<String>) -> Self {
        self.subtitle = value.into();
        self
    }

    pub fn cover(mut self, value: impl Into<String>) -> Self {
        self.cover = value.into();
        self
    }

    pub fn album(mut self, value: impl Into<String>) -> Self {
        self.album = value.into();
        self
    }

    pub fn release_date(mut self, value: impl Into<String>) -> Self {
        self.release_date = value.into();
        self
    }

    pub fn url(mut self, value: impl Into<String>) -> Self {
        self.url = value.into();
        self
    }

    fn usable(&self) -> bool {
        !self.id.trim().is_empty() && !self.title.trim().is_empty()
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CatalogSearch {
    pub platform: &'static str,
    /// The sections this platform fills, in tab order.
    pub sections: Vec<&'static str>,
    /// Best results: a mix of kinds, most relevant first.
    pub top: Vec<CatalogItem>,
    pub artists: Vec<CatalogItem>,
    pub albums: Vec<CatalogItem>,
    pub songs: Vec<CatalogItem>,
    pub playlists: Vec<CatalogItem>,
    /// Songs only, because the catalog failed: answered but never cached.
    #[serde(skip)]
    pub partial: bool,
}

impl CatalogSearch {
    pub fn new(platform: MusicPlatform) -> Self {
        Self {
            platform: platform.id(),
            sections: sections(platform).to_vec(),
            ..Self::default()
        }
    }

    /// Drops entries without an id or title and repeats within a section.
    pub fn tidy(mut self) -> Self {
        for list in [
            &mut self.top,
            &mut self.artists,
            &mut self.albums,
            &mut self.songs,
            &mut self.playlists,
        ] {
            let mut seen = std::collections::HashSet::new();
            list.retain(|item| item.usable() && seen.insert((item.kind, item.id.clone())));
        }
        self
    }

    /// Best results for a platform that has no ranking of its own: an artist
    /// or album named exactly like the query, then the leading songs, then
    /// the first album and playlist.
    pub fn rank_top(&mut self, query: &str) {
        let wanted = normalized(query);
        let mut top = Vec::new();
        if let Some(artist) = self.artists.iter().find(|a| normalized(&a.title) == wanted) {
            top.push(artist.clone());
        }
        if let Some(album) = self.albums.iter().find(|a| normalized(&a.title) == wanted) {
            top.push(album.clone());
        }
        top.extend(self.songs.iter().take(4).cloned());
        top.extend(self.albums.first().cloned());
        top.extend(self.playlists.first().cloned());
        let mut seen = std::collections::HashSet::new();
        top.retain(|item| seen.insert((item.kind, item.id.clone())));
        top.truncate(8);
        self.top = top;
    }
}

fn normalized(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Clone, Debug, Serialize)]
pub struct AlbumDetail {
    pub platform: &'static str,
    pub id: String,
    pub title: String,
    pub artist: String,
    /// Empty when the platform has no artist page.
    pub artist_id: String,
    pub cover: String,
    pub release_date: String,
    pub genre: String,
    pub copyright: String,
    /// The link `collections/resolve` accepts, kept as a saved playlist's source.
    pub url: String,
    pub tracks: Vec<CatalogItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ArtistDetail {
    pub platform: &'static str,
    pub id: String,
    pub name: String,
    pub cover: String,
    pub top_songs: Vec<CatalogItem>,
    pub albums: Vec<CatalogItem>,
    /// Singles and EPs, when the platform lists them apart from albums.
    pub singles: Vec<CatalogItem>,
}

/// Answers kept for a while, so repeated searches and pages cost the
/// platform (and the client's quota) nothing, like the song search cache.
pub struct TtlCache<V> {
    entries: std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, V)>>,
    ttl: std::time::Duration,
    capacity: usize,
}

impl<V: Clone> TtlCache<V> {
    pub fn new(ttl: std::time::Duration, capacity: usize) -> Self {
        Self {
            entries: Default::default(),
            ttl,
            capacity: capacity.max(1),
        }
    }

    pub fn get(&self, key: &str) -> Option<V> {
        let entries = self.entries.lock().unwrap();
        entries
            .get(key)
            .filter(|(stored, _)| stored.elapsed() < self.ttl)
            .map(|(_, value)| value.clone())
    }

    pub fn put(&self, key: String, value: V) {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= self.capacity {
            let ttl = self.ttl;
            entries.retain(|_, (stored, _)| stored.elapsed() < ttl);
            // Still full of live answers: drop the oldest.
            if entries.len() >= self.capacity
                && let Some(oldest) = entries
                    .iter()
                    .min_by_key(|(_, (stored, _))| *stored)
                    .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(key, (std::time::Instant::now(), value));
    }
}

/// `YYYY-MM-DD` for a millisecond timestamp in China time, which is how
/// NetEase stores release days (local midnight); empty for zero or junk.
pub fn date_from_millis(millis: i64) -> String {
    if millis <= 0 {
        return String::new();
    }
    let china = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|time| time.with_timezone(&china).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// An id a path segment may carry: platform ids are short and plain.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, title: &str) -> CatalogItem {
        CatalogItem::new(CatalogKind::Song, MusicPlatform::Netease, id).title(title)
    }

    #[test]
    fn top_puts_an_exact_artist_and_album_before_songs() {
        let mut search = CatalogSearch::new(MusicPlatform::Netease);
        search.artists = vec![
            CatalogItem::new(CatalogKind::Artist, MusicPlatform::Netease, "1").title("周杰伦 Jay"),
            CatalogItem::new(CatalogKind::Artist, MusicPlatform::Netease, "2").title("周杰伦"),
        ];
        search.albums =
            vec![CatalogItem::new(CatalogKind::Album, MusicPlatform::Netease, "9").title("叶惠美")];
        search.songs = (1..=6).map(|n| song(&n.to_string(), "晴天")).collect();
        search.rank_top(" 周杰伦 ");
        let ids: Vec<_> = search
            .top
            .iter()
            .map(|item| (item.kind, item.id.as_str()))
            .collect();
        assert_eq!(
            ids,
            vec![
                (CatalogKind::Artist, "2"),
                (CatalogKind::Song, "1"),
                (CatalogKind::Song, "2"),
                (CatalogKind::Song, "3"),
                (CatalogKind::Song, "4"),
                (CatalogKind::Album, "9"),
            ]
        );
    }

    #[test]
    fn tidy_drops_untitled_and_repeated_entries() {
        let mut search = CatalogSearch::new(MusicPlatform::Netease);
        search.songs = vec![
            song("1", "a"),
            song("1", "a"),
            song("", "b"),
            song("2", " "),
        ];
        let search = search.tidy();
        assert_eq!(search.songs.len(), 1);
    }

    #[test]
    fn songs_only_platforms_have_no_album_pages() {
        assert!(has_albums(MusicPlatform::AppleMusic));
        assert!(has_artists(MusicPlatform::Netease));
        assert!(has_albums(MusicPlatform::Tencent));
        assert!(!has_artists(MusicPlatform::Tencent));
        assert_eq!(sections(MusicPlatform::Bilibili), &[SONGS]);
    }

    #[test]
    fn cache_answers_until_expiry_and_stays_bounded() {
        let cache = TtlCache::new(std::time::Duration::from_secs(60), 2);
        cache.put("a".into(), 1);
        cache.put("b".into(), 2);
        cache.put("c".into(), 3);
        assert_eq!(cache.get("a"), None);
        assert_eq!((cache.get("b"), cache.get("c")), (Some(2), Some(3)));
        let expired = TtlCache::new(std::time::Duration::ZERO, 2);
        expired.put("a".into(), 1);
        assert_eq!(expired.get("a"), None);
    }

    #[test]
    fn dates_from_millis() {
        assert_eq!(date_from_millis(1019750400000), "2002-04-26");
        assert_eq!(date_from_millis(0), "");
    }

    #[test]
    fn serializes_one_shape_with_a_type_tag() {
        let item = CatalogItem::new(CatalogKind::Album, MusicPlatform::AppleMusic, "1").title("x");
        let json = serde_json::to_value(item).unwrap();
        assert_eq!(json["type"], "album");
        assert_eq!(json["platform"], "applemusic");
        assert!(json.get("track_number").is_none());
    }
}
