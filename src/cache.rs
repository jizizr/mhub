use crate::{ApiError, ApiResult, settings::SETTINGS};
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
               from_user_id, from_chat_id
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
               from_user_id, from_chat_id
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

pub async fn upsert_music_cache(cache: &MusicCache) -> ApiResult<()> {
    let pool = pool().await?;
    ensure_schema(pool).await?;
    sqlx::query(
        r#"
        INSERT INTO music_cache (
            platform, track_id, quality, song, singer, album, link, file_ext,
            music_size, bitrate, duration, file_id, thumb_file_id,
            from_user_id, from_chat_id
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
            from_chat_id = VALUES(from_chat_id)
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
    .execute(pool)
    .await?;
    Ok(())
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
                    UNIQUE KEY uniq_music_cache_track (platform, track_id, quality),
                    KEY idx_music_cache_file_id (file_id),
                    KEY idx_music_cache_platform_quality (platform, quality)
                ) CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci
                "#,
            )
            .execute(pool)
            .await?;
            Ok::<(), ApiError>(())
        })
        .await?;
    Ok(())
}

async fn pool() -> ApiResult<&'static MySqlPool> {
    MUSIC_CACHE_POOL
        .get_or_try_init(|| async {
            let url = SETTINGS.db.mysql.url.trim();
            if url.is_empty() {
                return Err(ApiError::Custom(
                    "db.mysql.url 未配置，无法复用 bot-rs music_cache".to_string(),
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
