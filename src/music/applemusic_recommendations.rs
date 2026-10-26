//! Recommendation metadata. Media, credentials and listening history never
//! travel through these responses. AutoPlay creates Apple's own ephemeral
//! AutoPlay station for the account, exactly as the web player does; nothing
//! is added to the library or played on the account.
use super::*;
use crate::music::RecommendationContinuation;
use futures::{StreamExt, stream};
use rand::seq::SliceRandom;
use std::collections::HashSet;

const HOME_TTL: Duration = Duration::from_secs(300);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const STALE_HOME_TTL: Duration = Duration::from_secs(1800);
const RELATED_TTL: Duration = Duration::from_secs(1800);
const MAX_RELATED_CACHE: usize = 64;

struct PersonalRecommendationCache {
    fetched_at: Instant,
    attempted_at: Instant,
    home: RecommendationHome,
}
struct RelatedRecommendationCache {
    fetched_at: Instant,
    tracks: Vec<MusicSearchItem>,
}
lazy_static! {
    static ref HOME_CACHE: RwLock<Option<PersonalRecommendationCache>> = RwLock::new(None);
    static ref HOME_FETCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
    static ref RELATED_CACHE: RwLock<HashMap<String, RelatedRecommendationCache>> =
        RwLock::new(HashMap::new());
    static ref RELATED_FETCH_PERMITS: tokio::sync::Semaphore = tokio::sync::Semaphore::new(4);
}

pub async fn personal_recommendations(
    limit: usize,
    refresh: bool,
    exclude: &[String],
) -> Result<Vec<MusicSearchItem>, BotError> {
    Ok(personal_recommendation_home(limit, refresh, exclude)
        .await?
        .tracks)
}

pub async fn personal_recommendation_home(
    limit: usize,
    refresh: bool,
    exclude: &[String],
) -> Result<RecommendationHome, BotError> {
    let home = recommendation_pool(refresh).await?;
    Ok(sample_home(home, limit.clamp(1, 50), exclude))
}

fn cached_home(max_age: Duration) -> Option<RecommendationHome> {
    HOME_CACHE.read().ok().and_then(|cache| {
        cache
            .as_ref()
            .filter(|cache| home_cache_usable(cache, max_age))
            .map(|cache| cache.home.clone())
    })
}

fn home_cache_usable(cache: &PersonalRecommendationCache, max_age: Duration) -> bool {
    cache.fetched_at.elapsed() < max_age
        || (cache.fetched_at.elapsed() < STALE_HOME_TTL
            && cache.attempted_at.elapsed() < REFRESH_COOLDOWN)
}

async fn recommendation_pool(refresh: bool) -> Result<RecommendationHome, BotError> {
    ensure_apple_enabled()?;
    let ttl = if refresh { REFRESH_COOLDOWN } else { HOME_TTL };
    if let Some(home) = cached_home(ttl) {
        return Ok(home);
    }
    // Concurrent refreshes share one upstream fetch. A refresh resamples the
    // pool immediately but can only refetch Apple once every thirty seconds.
    let _guard = HOME_FETCH_LOCK.lock().await;
    if let Some(home) = cached_home(ttl) {
        return Ok(home);
    }
    if let Ok(mut cache) = HOME_CACHE.write()
        && let Some(cache) = cache.as_mut()
    {
        cache.attempted_at = Instant::now();
    }
    match fetch_recommendation_pool().await {
        Ok(home) => {
            if !home.tracks.is_empty()
                && let Ok(mut cache) = HOME_CACHE.write()
            {
                *cache = Some(PersonalRecommendationCache {
                    fetched_at: Instant::now(),
                    attempted_at: Instant::now(),
                    home: home.clone(),
                });
            }
            Ok(home)
        }
        Err(error) => cached_home(STALE_HOME_TTL).ok_or(error),
    }
}

fn is_shared_catalog_content(content: &AppleMusicRecommendationContent) -> bool {
    match content.resource_type.as_deref() {
        Some("playlists") => {
            content.id.starts_with("pl.")
                && !content.id.starts_with("pl.u-")
                && content.id.len() <= 120
                && content
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
        }
        Some("albums" | "songs") => valid_catalog_id(&content.id),
        _ => false,
    }
}

async fn fetch_recommendation_pool() -> Result<RecommendationHome, BotError> {
    let media_user_token =
        normalize_media_user_token(SETTINGS.music.applemusic.media_user_token.trim());
    if media_user_token.is_empty() {
        return Err(BotError::Custom(
            "Apple Music 推荐需要配置 music.applemusic.media_user_token".into(),
        ));
    }
    let mut url = Url::parse(&format!("{APPLE_MUSIC_API}/v1/me/recommendations"))?;
    url.query_pairs_mut()
        .append_pair("include", "contents")
        .append_pair("l", &apple_language());
    let response: AppleMusicRecommendationsResponse = tokio::time::timeout(
        Duration::from_secs(8),
        apple_get_json_with_media_user_token(url.as_str(), &media_user_token, true),
    )
    .await
    .map_err(|_| BotError::Custom("Apple Music 推荐请求超时".into()))??;

    let mut playlists = Vec::new();
    let mut tags = Vec::new();
    let mut collections = Vec::new();
    let mut song_ids = Vec::new();
    let mut seen = HashSet::new();
    for (index, recommendation) in response.data.into_iter().take(30).enumerate() {
        let Some(contents) = recommendation.relationships.and_then(|r| r.contents) else {
            continue;
        };
        let mut tag_playlist_ids = Vec::new();
        for content in contents.data.into_iter().take(40) {
            if !is_shared_catalog_content(&content) {
                continue;
            }
            let kind = match content.resource_type.as_deref() {
                Some("songs") => {
                    song_ids.push(content.id);
                    continue;
                }
                Some("albums") => AppleCollectionKind::Album,
                _ => AppleCollectionKind::Playlist,
            };
            tag_playlist_ids.push(content.id.clone());
            if playlists.len() < 48 && seen.insert(content.id.clone()) {
                // Never copy personalized shelf titles/reasons from /me. They
                // can include account names even when the collection is public.
                let group_id = format!("selection-{}", index + 1);
                playlists.push(recommendation_playlist(&group_id, &content));
                collections.push(AppleCollectionId {
                    kind,
                    id: content.id,
                });
            }
        }
        if !tag_playlist_ids.is_empty() {
            tags.push(RecommendationTag {
                id: format!("selection-{}", index + 1),
                label: if index == 0 {
                    "为你精选".into()
                } else {
                    format!("精选推荐 {}", index + 1)
                },
                subtitle: String::new(),
                playlist_ids: tag_playlist_ids,
            });
        }
    }
    // Cache a broad pool, never a caller's first `limit` songs. Resolve a
    // changing selection of shelves in parallel with bounded latency.
    collections.shuffle(&mut rand::rng());
    let batches = stream::iter(
        collections
            .into_iter()
            .take(8)
            .map(|collection| async move {
                tokio::time::timeout(
                    Duration::from_secs(10),
                    get_apple_collection(collection, 50),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .map(|collection| collection.items)
                .unwrap_or_default()
            }),
    )
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    let mut songs = batches.into_iter().flatten().collect::<Vec<_>>();
    let direct = stream::iter(song_ids.into_iter().take(12).map(|id| async move {
        tokio::time::timeout(Duration::from_secs(5), get_apple_song(&id))
            .await
            .ok()
            .and_then(Result::ok)
            .map(song_to_item)
    }))
    .buffer_unordered(6)
    .collect::<Vec<_>>()
    .await;
    songs.extend(direct.into_iter().flatten());
    if songs.is_empty() && !playlists.is_empty() {
        // A transient collection failure must not poison the pool for the next
        // five minutes or masquerade as an exhausted recommendation catalog.
        return Err(BotError::Custom(
            "Apple Music 推荐歌曲暂时不可用，请稍后重试".into(),
        ));
    }
    Ok(RecommendationHome {
        source: "applemusic-personal".into(),
        tags,
        playlists,
        tracks: songs,
    })
}

fn sample_home(
    mut home: RecommendationHome,
    limit: usize,
    exclude: &[String],
) -> RecommendationHome {
    home.tracks = sample_tracks(home.tracks, limit, exclude);
    // Keep the standard personal mixes easy to find while rotating the broader
    // catalog shelves. Each collection still opens its full existing detail.
    let (mut mixes, mut other): (Vec<_>, Vec<_>) = home
        .playlists
        .into_iter()
        .partition(|card| card.id.starts_with("pl.pm-"));
    mixes.shuffle(&mut rand::rng());
    other.shuffle(&mut rand::rng());
    let mut cards = mixes.drain(..mixes.len().min(4)).collect::<Vec<_>>();
    other.extend(mixes);
    cards.extend(other.into_iter().take(12usize.saturating_sub(cards.len())));
    let ids: HashSet<_> = cards.iter().map(|card| card.id.as_str()).collect();
    for tag in &mut home.tags {
        tag.playlist_ids.retain(|id| ids.contains(id.as_str()));
    }
    home.tags.retain(|tag| !tag.playlist_ids.is_empty());
    home.playlists = cards;
    home
}

fn valid_catalog_id(id: &str) -> bool {
    (1..=20).contains(&id.len())
        && id.bytes().all(|byte| byte.is_ascii_digit())
        && id.bytes().any(|byte| byte != b'0')
}

fn text_key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn sample_tracks(
    mut pool: Vec<MusicSearchItem>,
    limit: usize,
    exclude: &[String],
) -> Vec<MusicSearchItem> {
    pool.shuffle(&mut rand::rng());
    select_tracks(pool, limit, exclude)
}

fn select_tracks(
    pool: Vec<MusicSearchItem>,
    limit: usize,
    exclude: &[String],
) -> Vec<MusicSearchItem> {
    let mut ids: HashSet<_> = exclude.iter().cloned().collect();
    // An alternate album edition of an excluded recording must not reappear
    // just because Apple assigned it a second catalog ID.
    let mut recordings: HashSet<_> = pool
        .iter()
        .filter(|item| ids.contains(&item.id))
        .map(|item| (text_key(&item.song), text_key(&item.singer)))
        .collect();
    let mut artists = HashMap::<String, usize>::new();
    let mut selected = Vec::new();
    let mut deferred = Vec::new();
    for item in pool {
        if !valid_catalog_id(&item.id) || !ids.insert(item.id.clone()) {
            continue;
        }
        let artist = text_key(&item.singer);
        if !recordings.insert((text_key(&item.song), artist.clone())) {
            continue;
        }
        let count = artists.entry(artist).or_default();
        if *count >= 3 {
            deferred.push(item);
            continue;
        }
        *count += 1;
        selected.push(item);
        if selected.len() >= limit {
            return selected;
        }
    }
    // An artist-only candidate pool is still useful, but do not allow one
    // artist to crowd out other available artists in the first pass.
    selected.extend(
        deferred
            .into_iter()
            .take(limit.saturating_sub(selected.len())),
    );
    selected
}

/// Apple Music's AutoPlay, as MusicKit implements it: seed a personal station
/// with the last songs of the queue (`POST /v1/me/stations/continuous`,
/// at most `AUTOPLAY_SEEDS`), then keep drawing from the same station
/// (`POST /v1/me/stations/next-tracks/{id}`). Without the account token, or if
/// Apple returns nothing usable, the related-artist pool below is the fallback.
const AUTOPLAY_SEEDS: usize = 10;
const AUTOPLAY_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn continuation_recommendations(
    seed_id: &str,
    seeds: &[String],
    station: Option<&str>,
    exclude: &[String],
    limit: usize,
) -> Result<RecommendationContinuation, BotError> {
    ensure_apple_enabled()?;
    if !valid_catalog_id(seed_id) {
        return Err(BotError::Custom(
            "seed_id 必须为 Apple Music 歌曲数字 ID".into(),
        ));
    }
    let limit = limit.clamp(1, 50);
    let mut excluded = exclude.to_vec();
    excluded.push(seed_id.to_string());
    let seeds = autoplay_seeds(seed_id, seeds);
    let mut station_id = None;
    match autoplay_station_tracks(&seeds, station, limit).await {
        Ok((id, pool)) => {
            let tracks = select_tracks(pool, limit, &excluded);
            if !tracks.is_empty() {
                return Ok(RecommendationContinuation {
                    source: "applemusic-autoplay".into(),
                    tracks,
                    station: Some(id),
                });
            }
            station_id = Some(id);
        }
        Err(error) => tracing::info!("Apple Music AutoPlay station unavailable: {error}"),
    }
    let mut result = related_continuation(seed_id, &mut excluded, exclude, limit).await?;
    result.station = station_id;
    Ok(result)
}

/// The last `AUTOPLAY_SEEDS` distinct catalog songs of the queue, oldest
/// first, always ending with the song that is playing.
fn autoplay_seeds(seed_id: &str, seeds: &[String]) -> Vec<String> {
    let mut ordered = Vec::new();
    for id in seeds
        .iter()
        .map(|id| id.trim())
        .chain(std::iter::once(seed_id))
    {
        if !valid_catalog_id(id) {
            continue;
        }
        ordered.retain(|item: &String| item != id);
        ordered.push(id.to_string());
    }
    let skip = ordered.len().saturating_sub(AUTOPLAY_SEEDS);
    ordered.split_off(skip)
}

/// AutoPlay station ids look like `ra.<kind>-<id>`; anything else is not
/// interpolated into a request path.
fn valid_station_id(id: &str) -> bool {
    id.starts_with("ra.")
        && id.len() <= 160
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

#[derive(Deserialize)]
struct AutoplayStationEnvelope {
    results: Option<AutoplayStationResults>,
}
#[derive(Deserialize)]
struct AutoplayStationResults {
    station: Option<CatalogId>,
    #[serde(default)]
    tracks: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
struct StationTracksResponse {
    #[serde(default)]
    data: Vec<serde_json::Value>,
}

/// Station items are catalog songs; anything that is not (a video, a library
/// song without catalog metadata) is skipped rather than failing the batch.
fn station_items(values: Vec<serde_json::Value>) -> Vec<MusicSearchItem> {
    values
        .into_iter()
        .filter(|value| {
            value
                .get("type")
                .and_then(|kind| kind.as_str())
                .is_none_or(|kind| kind == "songs")
        })
        .filter_map(|value| serde_json::from_value::<AppleMusicResource>(value).ok())
        .map(song_to_item)
        .filter(|item| valid_catalog_id(&item.id))
        .collect()
}

async fn autoplay_station_tracks(
    seeds: &[String],
    station: Option<&str>,
    limit: usize,
) -> Result<(String, Vec<MusicSearchItem>), BotError> {
    let token = normalize_media_user_token(SETTINGS.music.applemusic.media_user_token.trim());
    if token.is_empty() {
        return Err(BotError::Custom(
            "未配置 Apple Music 账号，无法使用 AutoPlay".into(),
        ));
    }
    // A few extra tracks cover songs that are already in the queue.
    let wanted = (limit + 5).min(30).to_string();
    if let Some(id) = station.filter(|id| valid_station_id(id)) {
        let mut url = Url::parse(&format!(
            "{APPLE_MUSIC_API}/v1/me/stations/next-tracks/{id}"
        ))?;
        url.query_pairs_mut()
            .append_pair("limit", &wanted)
            .append_pair("l", &apple_language());
        let next = tokio::time::timeout(
            AUTOPLAY_TIMEOUT,
            apple_post_json_with_media_user_token::<StationTracksResponse>(
                url.as_str(),
                None,
                &token,
                true,
            ),
        )
        .await;
        if let Ok(Ok(response)) = next {
            let tracks = station_items(response.data);
            if !tracks.is_empty() {
                return Ok((id.to_string(), tracks));
            }
        }
        // An expired or exhausted station is recreated from the queue.
    }
    if seeds.is_empty() {
        return Err(BotError::Custom("没有可用于 AutoPlay 的歌曲".into()));
    }
    let mut url = Url::parse(&format!("{APPLE_MUSIC_API}/v1/me/stations/continuous"))?;
    url.query_pairs_mut()
        .append_pair("limit[results:tracks]", &wanted)
        .append_pair("with", "tracks")
        .append_pair("l", &apple_language());
    let body = serde_json::json!({
        "data": seeds.iter().map(|id| serde_json::json!({ "id": id, "type": "songs" })).collect::<Vec<_>>()
    });
    let response: AutoplayStationEnvelope = tokio::time::timeout(
        AUTOPLAY_TIMEOUT,
        apple_post_json_with_media_user_token(url.as_str(), Some(&body), &token, true),
    )
    .await
    .map_err(|_| BotError::Custom("Apple Music AutoPlay 请求超时".into()))??;
    let results = response
        .results
        .ok_or_else(|| BotError::Custom("Apple Music 未返回 AutoPlay 电台".into()))?;
    let station = results
        .station
        .map(|station| station.id)
        .filter(|id| valid_station_id(id))
        .ok_or_else(|| BotError::Custom("Apple Music 未返回 AutoPlay 电台".into()))?;
    Ok((station, station_items(results.tracks)))
}

/// The previous algorithm, kept as the fallback: the seed artist and similar
/// artists' top songs, then the account's own recommendations.
async fn related_continuation(
    seed_id: &str,
    excluded: &mut Vec<String>,
    exclude: &[String],
    limit: usize,
) -> Result<RecommendationContinuation, BotError> {
    let related = related_pool(seed_id).await;
    let mut upstream_failed = related.is_err();
    let mut tracks = sample_tracks(related.unwrap_or_default(), limit, excluded);
    let source = if tracks.is_empty() {
        "applemusic-personal"
    } else {
        "applemusic-related"
    };
    if tracks.len() < limit {
        // Fallback is the account's real Apple recommendations, never a search
        // query such as "chill". Excluded/played songs stay excluded on failure.
        match recommendation_pool(false).await {
            Ok(home) => {
                excluded.extend(tracks.iter().map(|item| item.id.clone()));
                let additional = sample_tracks(home.tracks, limit - tracks.len(), excluded);
                tracks.extend(additional);
                tracks = select_tracks(tracks, limit, exclude);
            }
            Err(_) => upstream_failed = true,
        }
    }
    finish_continuation(source, tracks, upstream_failed)
}

fn finish_continuation(
    source: &str,
    tracks: Vec<MusicSearchItem>,
    upstream_failed: bool,
) -> Result<RecommendationContinuation, BotError> {
    if tracks.is_empty() && upstream_failed {
        return Err(BotError::Http {
            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
            message: "暂时无法获取续播推荐，请稍后重试".into(),
        });
    }
    Ok(RecommendationContinuation {
        source: source.into(),
        tracks,
        station: None,
    })
}

fn cached_related(seed_id: &str) -> Option<Vec<MusicSearchItem>> {
    RELATED_CACHE.read().ok().and_then(|cache| {
        cache
            .get(seed_id)
            .filter(|entry| entry.fetched_at.elapsed() < RELATED_TTL)
            .map(|entry| entry.tracks.clone())
    })
}

async fn related_pool(seed_id: &str) -> Result<Vec<MusicSearchItem>, BotError> {
    if let Some(tracks) = cached_related(seed_id) {
        return Ok(tracks);
    }
    let _permit = RELATED_FETCH_PERMITS
        .acquire()
        .await
        .map_err(|_| BotError::Custom("推荐服务暂不可用".into()))?;
    if let Some(tracks) = cached_related(seed_id) {
        return Ok(tracks);
    }
    let tracks = tokio::time::timeout(Duration::from_secs(24), fetch_related_pool(seed_id))
        .await
        .map_err(|_| BotError::Custom("Apple Music 相关歌曲请求超时".into()))??;
    if !tracks.is_empty()
        && let Ok(mut cache) = RELATED_CACHE.write()
    {
        cache.retain(|_, entry| entry.fetched_at.elapsed() < RELATED_TTL);
        if cache.len() >= MAX_RELATED_CACHE {
            let oldest = cache
                .iter()
                .min_by_key(|(_, entry)| entry.fetched_at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            seed_id.into(),
            RelatedRecommendationCache {
                fetched_at: Instant::now(),
                tracks: tracks.clone(),
            },
        );
    }
    Ok(tracks)
}

#[derive(Deserialize)]
struct CatalogIdsResponse {
    #[serde(default)]
    data: Vec<CatalogId>,
}
#[derive(Deserialize)]
struct CatalogId {
    id: String,
}

fn catalog_url(storefront: &str, path: &str, limit: usize) -> String {
    let mut url = Url::parse(&format!("{APPLE_MUSIC_API}/v1/catalog/{storefront}/{path}")).unwrap();
    url.query_pairs_mut()
        .append_pair("limit", &limit.to_string())
        .append_pair("l", &apple_language());
    url.to_string()
}

async fn catalog_request<T: DeserializeOwned>(url: &str) -> Result<T, BotError> {
    tokio::time::timeout(Duration::from_secs(8), apple_get_json(url, true))
        .await
        .map_err(|_| BotError::Custom("Apple Music 推荐资料请求超时".into()))?
}

async fn fetch_related_pool(seed_id: &str) -> Result<Vec<MusicSearchItem>, BotError> {
    let storefront = apple_storefront();
    let artists: CatalogIdsResponse = catalog_request(&catalog_url(
        &storefront,
        &format!("songs/{seed_id}/artists"),
        3,
    ))
    .await?;
    let artists = artists
        .data
        .into_iter()
        .filter(|a| valid_catalog_id(&a.id))
        .take(2)
        .map(|a| a.id)
        .collect::<Vec<_>>();
    let Some(seed_artist) = artists.first() else {
        return Ok(Vec::new());
    };
    let path = format!("artists/{seed_artist}/view/similar-artists");
    let similar: Result<CatalogIdsResponse, _> =
        catalog_request(&catalog_url(&storefront, &path, 20)).await;
    // Artist relationships are unavailable in some storefronts (verified for
    // cn). Only the public artist graph may come from us; song availability
    // and every actual track below remain in the configured storefront.
    let similar = match similar {
        Ok(response) if !response.data.is_empty() => response,
        _ if storefront != "us" => catalog_request(&catalog_url("us", &path, 20))
            .await
            .unwrap_or(CatalogIdsResponse { data: Vec::new() }),
        _ => CatalogIdsResponse { data: Vec::new() },
    };
    let mut other_artists = similar
        .data
        .into_iter()
        .filter(|a| valid_catalog_id(&a.id))
        .map(|a| a.id)
        .filter(|id| !artists.contains(id))
        .collect::<Vec<_>>();
    other_artists.shuffle(&mut rand::rng());
    let mut candidates = artists;
    candidates.extend(other_artists.into_iter().take(6));
    let batches = stream::iter(candidates.into_iter().map(|id| {
        let storefront = storefront.clone();
        async move {
            let url = catalog_url(&storefront, &format!("artists/{id}/view/top-songs"), 20);
            catalog_request::<AppleMusicResourceList>(&url)
                .await
                .map(|response| {
                    response
                        .data
                        .into_iter()
                        .map(song_to_item)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        }
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    Ok(batches.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn song(id: &str, name: &str, artist: &str) -> MusicSearchItem {
        MusicSearchItem {
            platform: MusicPlatform::AppleMusic,
            id: id.into(),
            song: name.into(),
            singer: artist.into(),
            cover: String::new(),
        }
    }
    #[test]
    fn exclusions_and_duplicate_recordings_stay_excluded() {
        let result = select_tracks(
            vec![
                song("100001", "Old", "A"),
                song("100009", "old", "a"),
                song("100002", "New Song", "B"),
                song("100003", "new-song", "b"),
                song("100002", "New Song", "B"),
                song("100004", "Other", "C"),
            ],
            10,
            &["100001".into()],
        );
        assert_eq!(
            result.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["100002", "100004"]
        );
    }
    #[test]
    fn autoplay_seeds_keep_the_latest_ten_ending_with_the_current_song() {
        let queue = (100001..100015)
            .map(|id| id.to_string())
            .collect::<Vec<_>>();
        let seeds = autoplay_seeds("100003", &queue);
        assert_eq!(seeds.len(), AUTOPLAY_SEEDS);
        assert_eq!(seeds.last().map(String::as_str), Some("100003"));
        assert!(!seeds[..seeds.len() - 1].contains(&"100003".to_string()));
        let invalid = autoplay_seeds("100001", &["abc".into(), "0".into(), "100002".into()]);
        assert_eq!(invalid, ["100002", "100001"]);
    }
    #[test]
    fn station_ids_are_checked_before_use_in_a_path() {
        assert!(valid_station_id("ra.cp-1055074639"));
        assert!(valid_station_id("ra.q-GAI6IDE2OTM0ODY1MjI0MGM2Zjg0"));
        assert!(!valid_station_id("pl.123"));
        assert!(!valid_station_id("ra.../../v1/me/library"));
        assert!(!valid_station_id("ra.a?b=c"));
    }
    #[test]
    fn station_items_keep_catalog_songs_and_skip_others() {
        let items = station_items(vec![
            serde_json::json!({ "id": "100001", "type": "songs", "attributes": { "name": "A", "artistName": "X" } }),
            serde_json::json!({ "id": "100002", "type": "music-videos", "attributes": { "name": "V", "artistName": "X" } }),
            serde_json::json!({ "id": "i.abc", "type": "library-songs", "attributes": { "name": "L" } }),
            serde_json::json!({ "id": "100003", "type": "songs" }),
            serde_json::json!({ "id": "100004", "attributes": { "name": "B", "artistName": "" } }),
        ]);
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["100001", "100004"]
        );
        assert_eq!(items[1].singer, "未知歌手");
    }
    #[test]
    fn autoplay_station_responses_parse_like_musickit() {
        let created: AutoplayStationEnvelope = serde_json::from_value(serde_json::json!({
            "results": { "station": { "id": "ra.q-abc", "type": "stations" }, "tracks": [
                { "id": "100001", "type": "songs", "attributes": { "name": "A", "artistName": "X" } }
            ] }
        }))
        .unwrap();
        let results = created.results.unwrap();
        assert_eq!(results.station.unwrap().id, "ra.q-abc");
        assert_eq!(station_items(results.tracks).len(), 1);
        let next: StationTracksResponse =
            serde_json::from_value(serde_json::json!({ "data": [] })).unwrap();
        assert!(next.data.is_empty());
    }
    #[test]
    fn exhausted_pool_returns_empty_without_repeats() {
        assert!(select_tracks(vec![song("100001", "Old", "A")], 10, &["100001".into()]).is_empty());
    }
    #[test]
    fn selection_prioritizes_artist_variety() {
        let mut pool = (100001..100011)
            .map(|id| song(&id.to_string(), &id.to_string(), "Same artist"))
            .collect::<Vec<_>>();
        pool.push(song("100020", "Different", "Another artist"));
        let result = select_tracks(pool, 5, &[]);
        assert_eq!(result.len(), 5);
        assert_eq!(result[3].singer, "Another artist");
    }
    #[test]
    fn no_private_collections_or_station_names_are_exposed() {
        for (id, kind, allowed) in [
            ("pl.u-private", "playlists", false),
            ("p.private", "library-playlists", false),
            ("ra.private", "stations", false),
            ("pl.pm-safe", "playlists", true),
            ("pl.safe", "playlists", true),
            ("100001", "albums", true),
        ] {
            let value: AppleMusicRecommendationContent =
                serde_json::from_value(serde_json::json!({"id":id,"type":kind})).unwrap();
            assert_eq!(is_shared_catalog_content(&value), allowed);
        }
    }
    #[test]
    fn invalid_ids_cannot_be_used_as_catalog_paths() {
        for id in ["../me", "123456?x=1", "", "0000", "123456789012345678901"] {
            assert!(!valid_catalog_id(id));
        }
        assert!(valid_catalog_id("1624001324"));
        assert!(valid_catalog_id("123"));
    }

    #[test]
    fn upstream_failure_is_not_reported_as_exhausted_candidates() {
        assert!(
            matches!(finish_continuation("test", Vec::new(), true), Err(BotError::Http {status, ..}) if status == reqwest::StatusCode::SERVICE_UNAVAILABLE)
        );
        assert!(
            finish_continuation("test", Vec::new(), false)
                .unwrap()
                .tracks
                .is_empty()
        );
        assert_eq!(
            finish_continuation("test", vec![song("1", "Available", "A")], true)
                .unwrap()
                .tracks
                .len(),
            1
        );
    }

    #[test]
    fn failed_refresh_cooldown_keeps_stale_pool_but_does_not_extend_it_forever() {
        let mut cache = PersonalRecommendationCache {
            fetched_at: Instant::now() - Duration::from_secs(600),
            attempted_at: Instant::now(),
            home: RecommendationHome {
                source: "test".into(),
                tracks: Vec::new(),
                tags: Vec::new(),
                playlists: Vec::new(),
            },
        };
        assert!(home_cache_usable(&cache, REFRESH_COOLDOWN));
        cache.attempted_at = Instant::now() - Duration::from_secs(31);
        assert!(!home_cache_usable(&cache, REFRESH_COOLDOWN));
        cache.attempted_at = Instant::now();
        cache.fetched_at = Instant::now() - Duration::from_secs(1801);
        assert!(!home_cache_usable(&cache, REFRESH_COOLDOWN));
    }
    #[test]
    fn home_samples_from_full_pool_and_never_mutates_it() {
        let pool = RecommendationHome {
            source: "test".into(),
            playlists: Vec::new(),
            tags: Vec::new(),
            tracks: (100000..100200)
                .map(|id| song(&id.to_string(), &id.to_string(), &id.to_string()))
                .collect(),
        };
        let first = sample_home(pool.clone(), 24, &[]);
        let excluded = first
            .tracks
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let second = sample_home(pool.clone(), 24, &excluded);
        assert_eq!(second.tracks.len(), 24);
        assert!(
            second
                .tracks
                .iter()
                .all(|item| !excluded.contains(&item.id))
        );
        assert_eq!(pool.tracks.len(), 200);
    }
    #[tokio::test]
    #[ignore = "read-only public Apple catalog probe; never accesses account recommendations"]
    async fn live_related_catalog_has_playable_candidates() {
        let pool = fetch_related_pool("1624001324").await.unwrap();
        assert!(!pool.is_empty());
        let tracks = sample_tracks(pool, 10, &["1624001324".into()]);
        assert!(!tracks.is_empty());
        assert!(
            tracks
                .iter()
                .all(|track| valid_catalog_id(&track.id) && track.id != "1624001324")
        );
    }
}
