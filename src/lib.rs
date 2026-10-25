pub mod accounts;
pub mod api;
pub mod audio;
pub mod cache;
mod loudness;
pub mod matching;
pub mod music;
pub mod settings;
pub mod telegram_cache;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("API请求失败: {0}")]
    Request(#[from] reqwest::Error),
    #[error("API请求失败: {0}")]
    Retry(#[from] reqwest_middleware::Error),
    #[error("{0}")]
    Custom(String),
    #[error("{message}")]
    Http {
        status: axum::http::StatusCode,
        message: String,
    },
    #[error("{0}")]
    IOError(#[from] std::io::Error),
    #[error("{0}")]
    SerdeError(#[from] serde_json::Error),
    #[error("{0}")]
    UrlParseError(#[from] url::ParseError),
    #[error("{0}")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("{0}")]
    Sqlx(#[from] sqlx::Error),
}

pub type BotError = ApiError;
