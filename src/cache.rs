use crate::{ApiError, ApiResult, settings::SETTINGS};
use music_gateway::{CacheStats, PlatformCacheStats, QualityCacheStats};
use serde::Serialize;
use sqlx::{MySqlPool, mysql::MySqlPoolOptions};
use tokio::sync::OnceCell;

static MUSIC_CACHE_POOL: OnceCell<MySqlPool> = OnceCell::const_new();
static MUSIC_CACHE_SCHEMA: OnceCell<()> = OnceCell::const_new();

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct MusicCache {
    pub platform: String,
    pub track_id: String,
    pub quality: String,
    pub song: String,
    pub singer: String,
    pub album: String,
    pub link: String,
    pub file_ext: String,
    pub music_size: i64,
    pub bitrate: i32,
    pub duration: Option<i32>,
    pub file_id: String,
    pub thumb_file_id: String,
    pub from_user_id: i64,
    pub from_chat_id: i64,
    /// Nonzero once the upload has been through lyrics tagging
    /// (`lyric_tags::LYRICS_TAGGED`); 0 for uploads made before lyrics were
    /// embedded, or whose lyrics could not be fetched yet.
    pub lyrics_tag: i8,
}

pub async fn find_music_cache(
    platform: &str,
    track_id: &str,
    quality: &str,
) -> ApiResult<Option<MusicCache>> {
    let pool = pool().await?;
    ensure_schema(pool).await?;
    let row = sqlx::query_as::<_, MusicCache>(
        r#"
        SELECT platform, track_id, quality, song, singer, album, link, file_ext,
               music_size, bitrate, duration, file_id, thumb_file_id,
               from_user_id, from_chat_id, lyrics_tag
        FROM music_cache
        WHERE platform = ? AND track_id = ? AND quality = ?
        LIMIT 1
        "#,
    )
    .bind(platform)
    .bind(track_id)
    .bind(quality)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn find_music_cache_by_file_id(file_id: &str) -> ApiResult<Option<MusicCache>> {
    let pool = pool().await?;
    ensure_schema(pool).await?;
    let row = sqlx::query_as::<_, MusicCache>(
        r#"
        SELECT platform, track_id, quality, song, singer, album, link, file_ext,
               music_size, bitrate, duration, file_id, thumb_file_id,
               from_user_id, from_chat_id, lyrics_tag
        FROM music_cache
        WHERE file_id = ?
        LIMIT 1
        "#,
    )
    .bind(file_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// An upload on `platform` made from the same downloaded bytes, under any
/// track ID or quality. Platforms hand one file to several quality tiers (a
/// Hi-Res request falls back to the lossless file) and to several IDs (one
/// recording on several albums), and each would otherwise upload it again.
pub async fn find_music_cache_by_source(
    platform: &str,
    source_sha256: &str,
) -> ApiResult<Option<MusicCache>> {
    let pool = pool().await?;
    ensure_schema(pool).await?;
    let row = sqlx::query_as::<_, MusicCache>(
        r#"
        SELECT platform, track_id, quality, song, singer, album, link, file_ext,
               music_size, bitrate, duration, file_id, thumb_file_id,
               from_user_id, from_chat_id, lyrics_tag
        FROM music_cache
        WHERE platform = ? AND source_sha256 = ? AND file_id <> ''
        ORDER BY lyrics_tag DESC
        LIMIT 1
        "#,
    )
    .bind(platform)
    .bind(source_sha256)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Saves the row; `source_sha256` is the SHA-256 of the downloaded audio
/// before lyrics were written in, which `find_music_cache_by_source` matches.
pub async fn upsert_music_cache(cache: &MusicCache, source_sha256: Option<&str>) -> ApiResult<()> {
    let pool = pool().await?;
    ensure_schema(pool).await?;
    sqlx::query(
        r#"
        INSERT INTO music_cache (
            platform, track_id, quality, song, singer, album, link, file_ext,
            music_size, bitrate, duration, file_id, thumb_file_id,
            from_user_id, from_chat_id, lyrics_tag, source_sha256
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON DUPLICATE KEY UPDATE
            song = VALUES(song),
            singer = VALUES(singer),
            album = VALUES(album),
            link = VALUES(link),
            file_ext = VALUES(file_ext),
            music_size = VALUES(music_size),
            bitrate = VALUES(bitrate),
            duration = VALUES(duration),
            file_id = VALUES(file_id),
            thumb_file_id = VALUES(thumb_file_id),
            from_user_id = VALUES(from_user_id),
            from_chat_id = VALUES(from_chat_id),
            lyrics_tag = VALUES(lyrics_tag),
            source_sha256 = VALUES(source_sha256)
        "#,
    )
    .bind(&cache.platform)
    .bind(&cache.track_id)
    .bind(&cache.quality)
    .bind(&cache.song)
    .bind(&cache.singer)
    .bind(&cache.album)
    .bind(&cache.link)
    .bind(&cache.file_ext)
    .bind(cache.music_size)
    .bind(cache.bitrate)
    .bind(cache.duration)
    .bind(&cache.file_id)
    .bind(&cache.thumb_file_id)
    .bind(cache.from_user_id)
    .bind(cache.from_chat_id)
    .bind(cache.lyrics_tag)
    .bind(source_sha256)
    .execute(pool)
    .await?;
    Ok(())
}

/// Swaps in a re-uploaded file (or only a new `lyrics_tag` when the file is
/// unchanged) on every row still holding `old_file_id` (the qualities and
/// track IDs sharing one upload), so a replacement made in the meantime
/// wins. Returns whether any row was updated.
pub async fn replace_music_cache_file(old_file_id: &str, cache: &MusicCache) -> ApiResult<bool> {
    let pool = pool().await?;
    ensure_schema(pool).await?;
    let result = sqlx::query(
        r#"
        UPDATE music_cache
        SET file_id = ?, thumb_file_id = ?, music_size = ?, lyrics_tag = ?,
            from_chat_id = ?
        WHERE file_id = ?
        "#,
    )
    .bind(&cache.file_id)
    .bind(&cache.thumb_file_id)
    .bind(cache.music_size)
    .bind(cache.lyrics_tag)
    .bind(cache.from_chat_id)
    .bind(old_file_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Removes every row on `platform` holding `file_id`: the qualities and
/// track IDs that share the upload lose it together, and a replacement
/// uploaded in the meantime survives. Returns whether a row was deleted.
pub async fn delete_music_cache_file(platform: &str, file_id: &str) -> ApiResult<bool> {
    let pool = pool().await?;
    ensure_schema(pool).await?;
    let result = sqlx::query(
        r#"
        DELETE FROM music_cache
        WHERE platform = ? AND file_id = ?
        "#,
    )
    .bind(platform)
    .bind(file_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Per-platform counts of the shared Telegram cache. Quality keys are folded
/// at their first `:`, so legacy `<quality>:<suffix>` rows count under their
/// tier, and a file shared by several tiers is sized once.
pub async fn music_cache_stats() -> ApiResult<CacheStats> {
    let pool = pool().await?;
    ensure_schema(pool).await?;
    let songs: Vec<(String, i64, i64)> = sqlx::query_as(
        r#"
        SELECT platform,
               COUNT(DISTINCT track_id),
               COUNT(DISTINCT CASE WHEN lyrics_tag > 0 THEN track_id END)
        FROM music_cache
        GROUP BY platform
        "#,
    )
    .fetch_all(pool)
    .await?;
    let files: Vec<(String, i64, i64)> = sqlx::query_as(
        r#"
        SELECT platform, COUNT(*), CAST(COALESCE(SUM(size), 0) AS SIGNED)
        FROM (
            SELECT platform, file_id, MAX(music_size) AS size
            FROM music_cache
            GROUP BY platform, file_id
        ) AS files
        GROUP BY platform
        "#,
    )
    .fetch_all(pool)
    .await?;
    let tiers: Vec<(String, String, i64)> = sqlx::query_as(
        r#"
        SELECT platform, SUBSTRING_INDEX(quality, ':', 1) AS tier, COUNT(DISTINCT track_id)
        FROM music_cache
        GROUP BY platform, tier
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(assemble_stats(songs, files, tiers))
}

fn assemble_stats(
    songs: Vec<(String, i64, i64)>,
    files: Vec<(String, i64, i64)>,
    tiers: Vec<(String, String, i64)>,
) -> CacheStats {
    let count = |value: i64| value.max(0) as u64;
    let mut platforms: Vec<PlatformCacheStats> = songs
        .into_iter()
        .map(|(platform, tracks, with_lyrics)| PlatformCacheStats {
            platform,
            tracks: count(tracks),
            with_lyrics: count(with_lyrics),
            ..Default::default()
        })
        .collect();
    for (platform, file_count, bytes) in files {
        if let Some(entry) = platforms
            .iter_mut()
            .find(|entry| entry.platform == platform)
        {
            entry.files = count(file_count);
            entry.bytes = count(bytes);
        }
    }
    for (platform, quality, tracks) in tiers {
        if let Some(entry) = platforms
            .iter_mut()
            .find(|entry| entry.platform == platform)
        {
            entry.qualities.push(QualityCacheStats {
                quality,
                tracks: count(tracks),
            });
        }
    }
    for entry in &mut platforms {
        entry.qualities.sort_by(|a, b| {
            b.tracks
                .cmp(&a.tracks)
                .then_with(|| a.quality.cmp(&b.quality))
        });
    }
    platforms.sort_by(|a, b| {
        b.tracks
            .cmp(&a.tracks)
            .then_with(|| a.platform.cmp(&b.platform))
    });
    CacheStats { platforms }
}

async fn ensure_schema(pool: &MySqlPool) -> ApiResult<()> {
    MUSIC_CACHE_SCHEMA
        .get_or_try_init(|| async {
            sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS music_cache (
                    id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                    platform VARCHAR(32) NOT NULL,
                    track_id VARCHAR(191) NOT NULL,
                    quality VARCHAR(32) NOT NULL,
                    song VARCHAR(512) NOT NULL,
                    singer VARCHAR(512) NOT NULL,
                    album VARCHAR(512) NOT NULL,
                    link TEXT NOT NULL,
                    file_ext VARCHAR(32) NOT NULL,
                    music_size BIGINT NOT NULL,
                    bitrate INT NOT NULL,
                    duration INT NULL,
                    file_id VARCHAR(512) NOT NULL,
                    thumb_file_id VARCHAR(512) NOT NULL,
                    from_user_id BIGINT NOT NULL,
                    from_chat_id BIGINT NOT NULL,
                    lyrics_tag TINYINT NOT NULL DEFAULT 0,
                    source_sha256 CHAR(64) NULL,
                    UNIQUE KEY uniq_music_cache_track (platform, track_id, quality),
                    KEY idx_music_cache_file_id (file_id),
                    KEY idx_music_cache_platform_quality (platform, quality),
                    KEY idx_music_cache_source (platform, source_sha256)
                ) CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci
                "#,
            )
            .execute(pool)
            .await?;
            // Tables created before lyrics were embedded, or before uploads
            // were matched by content, lack these columns.
            if !has_column(pool, "lyrics_tag").await? {
                sqlx::query(
                    "ALTER TABLE music_cache ADD COLUMN lyrics_tag TINYINT NOT NULL DEFAULT 0",
                )
                .execute(pool)
                .await?;
            }
            if !has_column(pool, "source_sha256").await? {
                sqlx::query(
                    "ALTER TABLE music_cache ADD COLUMN source_sha256 CHAR(64) NULL, \
                     ADD KEY idx_music_cache_source (platform, source_sha256)",
                )
                .execute(pool)
                .await?;
            }
            Ok::<(), ApiError>(())
        })
        .await?;
    Ok(())
}

async fn has_column(pool: &MySqlPool, column: &str) -> ApiResult<bool> {
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM information_schema.COLUMNS
        WHERE TABLE_SCHEMA = DATABASE()
          AND TABLE_NAME = 'music_cache'
          AND COLUMN_NAME = ?
        "#,
    )
    .bind(column)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

async fn pool() -> ApiResult<&'static MySqlPool> {
    MUSIC_CACHE_POOL
        .get_or_try_init(|| async {
            let url = SETTINGS.db.mysql.url.trim();
            if url.is_empty() {
                return Err(ApiError::Custom(
                    "db.mysql.url 未配置，无法使用 music_cache".to_string(),
                ));
            }
            MySqlPoolOptions::new()
                .max_connections(5)
                .connect(url)
                .await
                .map_err(ApiError::from)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_join_platform_rows_and_sort_by_song_count() {
        let stats = assemble_stats(
            vec![("netease".into(), 3, 1), ("tencent".into(), 10, 8)],
            vec![("netease".into(), 2, 300), ("tencent".into(), 9, -1)],
            vec![
                ("tencent".into(), "high".into(), 2),
                ("tencent".into(), "lossless".into(), 9),
                ("netease".into(), "lossless".into(), 3),
            ],
        );
        let ids: Vec<_> = stats
            .platforms
            .iter()
            .map(|p| p.platform.as_str())
            .collect();
        assert_eq!(ids, ["tencent", "netease"]);
        let tencent = &stats.platforms[0];
        assert_eq!(
            (
                tencent.tracks,
                tencent.files,
                tencent.bytes,
                tencent.with_lyrics
            ),
            (10, 9, 0, 8)
        );
        assert_eq!(tencent.qualities[0].quality, "lossless");
        assert_eq!(stats.platforms[1].bytes, 300);
    }
}
