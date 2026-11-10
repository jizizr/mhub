use crate::Platform;
use crate::protocol::{
    CacheInvalidateRequest, CacheLookupRequest, CacheStats, CachedTrack, Capabilities, Collection,
    CollectionRequest, ErrorBody, ErrorCode, JobState, Lyrics, LyricsRequest, Media, MediaJob,
    MediaLookup, MediaRequest, PROTOCOL_HEADER, PROTOCOL_VERSION, ResolveRequest, SearchItem,
    SearchRequest, Track, paths,
};
use reqwest::{Method, RequestBuilder, StatusCode, header::AUTHORIZATION};
use serde::{Serialize, de::DeserializeOwned};
use std::time::Duration;
use tokio::time::Instant;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long one media poll may wait on the server for the phase to change;
/// also how often a downloading job reports fresh byte counts.
const POLL_WAIT: Duration = Duration::from_secs(3);
const DEFAULT_MEDIA_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// MHub could not be reached or answered something other than the
    /// protocol. The source error is for logs only.
    #[error("音乐服务暂不可用，请稍后再试")]
    Unavailable(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("{0}")]
    Remote(ErrorBody),
    #[error("音乐服务处理超时，请稍后再试")]
    Timeout,
}

impl GatewayError {
    pub fn code(&self) -> ErrorCode {
        match self {
            GatewayError::Remote(body) => body.code,
            GatewayError::Unavailable(_) | GatewayError::Timeout => ErrorCode::Unavailable,
        }
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            GatewayError::Remote(body) => body.retry_after_ms.map(Duration::from_millis),
            _ => None,
        }
    }

    fn unavailable(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        GatewayError::Unavailable(Box::new(error))
    }
}

/// Client for MHub's loopback gateway. Cheap to clone.
#[derive(Clone, Debug)]
pub struct GatewayClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
    media_timeout: Duration,
}

impl GatewayClient {
    /// `base_url` is the gateway origin, e.g. `http://127.0.0.1:8091`.
    pub fn new(base_url: &str, token: &str) -> Result<Self, GatewayError> {
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(GatewayError::Remote(ErrorBody::new(
                ErrorCode::BadRequest,
                "音乐网关地址必须以 http:// 或 https:// 开头",
            )));
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            // The gateway is a local service; an environment proxy must not
            // receive its token.
            .no_proxy()
            .build()
            .map_err(GatewayError::unavailable)?;
        Ok(Self {
            http,
            base_url,
            token: token.trim().to_string(),
            media_timeout: DEFAULT_MEDIA_TIMEOUT,
        })
    }

    /// Upper bound for [`GatewayClient::media`], downloads included.
    pub fn with_media_timeout(mut self, timeout: Duration) -> Self {
        self.media_timeout = timeout;
        self
    }

    pub async fn capabilities(&self) -> Result<Capabilities, GatewayError> {
        self.send(
            self.request(Method::GET, paths::CAPABILITIES),
            REQUEST_TIMEOUT,
        )
        .await
    }

    pub async fn search(
        &self,
        platform: Platform,
        keyword: &str,
        limit: usize,
    ) -> Result<Vec<SearchItem>, GatewayError> {
        self.post(
            paths::SEARCH,
            &SearchRequest {
                platform,
                keyword: keyword.to_string(),
                limit,
            },
        )
        .await
    }

    /// `None` when the keyword is not a playlist or album of the platform.
    pub async fn collection(
        &self,
        platform: Platform,
        keyword: &str,
        limit: usize,
    ) -> Result<Option<Collection>, GatewayError> {
        self.post(
            paths::COLLECTION,
            &CollectionRequest {
                platform,
                keyword: keyword.to_string(),
                limit,
            },
        )
        .await
    }

    pub async fn lyrics(
        &self,
        platform: Platform,
        track_id: &str,
    ) -> Result<Option<Lyrics>, GatewayError> {
        self.post(
            paths::LYRICS,
            &LyricsRequest {
                platform,
                track_id: track_id.to_string(),
            },
        )
        .await
    }

    pub async fn resolve(&self, request: &ResolveRequest) -> Result<Track, GatewayError> {
        self.post(paths::RESOLVE, request).await
    }

    /// The cached file for the request, if any, without starting a download.
    pub async fn lookup_media(&self, request: &MediaRequest) -> Result<MediaLookup, GatewayError> {
        self.post(paths::MEDIA_LOOKUP, request).await
    }

    pub async fn start_media(&self, request: &MediaRequest) -> Result<MediaJob, GatewayError> {
        self.post(paths::MEDIA, request).await
    }

    /// Returns once the job leaves `since` or after `wait`, whichever is first.
    pub async fn poll_media(
        &self,
        id: &str,
        since: Option<&str>,
        wait: Duration,
    ) -> Result<MediaJob, GatewayError> {
        let wait_ms = wait.as_millis().to_string();
        let mut query = vec![("wait_ms", wait_ms.as_str())];
        if let Some(since) = since {
            query.push(("since", since));
        }
        let path = format!("{}/{id}", paths::MEDIA);
        self.send(
            self.request(Method::GET, &path).query(&query),
            wait + REQUEST_TIMEOUT,
        )
        .await
    }

    /// Starts or joins the job and follows it to the end, reporting every job
    /// snapshot it observes, the final `Ready` one included.
    pub async fn media<F>(
        &self,
        request: &MediaRequest,
        mut on_update: F,
    ) -> Result<Media, GatewayError>
    where
        F: FnMut(&MediaJob),
    {
        let deadline = Instant::now() + self.media_timeout;
        let mut job = self.start_media(request).await?;
        loop {
            on_update(&job);
            match job.state {
                JobState::Ready { media } => return Ok(*media),
                JobState::Failed { error } => return Err(GatewayError::Remote(error)),
                _ => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(GatewayError::Timeout);
            }
            job = self
                .poll_media(&job.id, Some(job.state.phase()), POLL_WAIT.min(remaining))
                .await?;
        }
    }

    /// The cached song a Telegram `file_id` belongs to, if MHub stored it.
    pub async fn lookup_cache(&self, file_id: &str) -> Result<Option<CachedTrack>, GatewayError> {
        self.post(
            paths::CACHE_LOOKUP,
            &CacheLookupRequest {
                file_id: file_id.to_string(),
            },
        )
        .await
    }

    pub async fn invalidate_cache(
        &self,
        request: &CacheInvalidateRequest,
    ) -> Result<(), GatewayError> {
        self.post::<_, serde::de::IgnoredAny>(paths::CACHE_INVALIDATE, request)
            .await
            .map(|_| ())
    }

    /// Songs the shared Telegram cache holds, per platform.
    pub async fn cache_stats(&self) -> Result<CacheStats, GatewayError> {
        self.send(
            self.request(Method::GET, paths::CACHE_STATS),
            REQUEST_TIMEOUT,
        )
        .await
    }

    async fn post<B, T>(&self, path: &str, body: &B) -> Result<T, GatewayError>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        self.send(self.request(Method::POST, path).json(body), REQUEST_TIMEOUT)
            .await
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base_url))
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(PROTOCOL_HEADER, PROTOCOL_VERSION.to_string())
    }

    async fn send<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        timeout: Duration,
    ) -> Result<T, GatewayError> {
        let response = request
            .timeout(timeout)
            .send()
            .await
            .map_err(GatewayError::unavailable)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(GatewayError::unavailable)?;
        if status.is_success() {
            return serde_json::from_slice(&bytes).map_err(GatewayError::unavailable);
        }
        match serde_json::from_slice::<ErrorBody>(&bytes) {
            Ok(body) => Err(GatewayError::Remote(body)),
            Err(_) => Err(GatewayError::unavailable(UnexpectedStatus(status))),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("gateway answered HTTP {0} without an error body")]
struct UnexpectedStatus(StatusCode);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Quality;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::mpsc,
    };

    /// Answers each connection with the next canned `(status, body)` and
    /// reports the request head it received.
    async fn canned_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, mpsc::UnboundedReceiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 8192];
                let read = socket.read(&mut request).await.unwrap();
                let _ = tx.send(String::from_utf8_lossy(&request[..read]).to_string());
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(head.as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://{address}"), rx)
    }

    fn media_request() -> MediaRequest {
        MediaRequest {
            platform: Platform::Netease,
            keyword: "晴天".into(),
            selected_id: None,
            quality: Quality::Lossless,
            include_cover: true,
            wait_for_lyrics: false,
        }
    }

    #[tokio::test]
    async fn media_follows_the_job_until_ready_and_sends_auth_and_protocol() {
        let ready = r#"{"id":"j","state":{"phase":"ready","media":{"track":{"platform":"netease","id":"1","song":"晴天","singer":"周杰伦","format":"flac"},"quality":"lossless","file_id":"F","byte_size":42,"cached":false}}}"#;
        let (base, mut requests) = canned_server(vec![
            (202, r#"{"id":"j","state":{"phase":"queued"}}"#),
            (
                200,
                r#"{"id":"j","state":{"phase":"downloading","written":5,"total":10}}"#,
            ),
            (200, ready),
        ])
        .await;
        let client = GatewayClient::new(&base, "secret").unwrap();
        let mut phases = Vec::new();
        let media = client
            .media(&media_request(), |job| phases.push(job.state.phase()))
            .await
            .unwrap();
        assert_eq!(media.file_id, "F");
        assert_eq!(media.track.file_name(), "晴天 - 周杰伦.flac");
        assert_eq!(phases, ["queued", "downloading", "ready"]);

        let start = requests.recv().await.unwrap().to_ascii_lowercase();
        assert!(start.starts_with("post /v1/media "));
        assert!(start.contains("authorization: bearer secret"));
        assert!(start.contains(&format!("{PROTOCOL_HEADER}: {PROTOCOL_VERSION}")));
        let poll = requests.recv().await.unwrap();
        assert!(poll.starts_with("GET /v1/media/j?wait_ms=3000&since=queued "));
        let poll = requests.recv().await.unwrap();
        assert!(poll.contains("since=downloading"));
    }

    #[tokio::test]
    async fn remote_errors_keep_their_code_and_user_message() {
        let (base, _requests) = canned_server(vec![
            (
                429,
                r#"{"code":"rate_limited","message":"请求过于频繁","retry_after_ms":1500}"#,
            ),
            (502, "<html>bad gateway</html>"),
        ])
        .await;
        let client = GatewayClient::new(&base, "t").unwrap();
        let error = client
            .search(Platform::Tencent, "晴天", 5)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::RateLimited);
        assert_eq!(error.to_string(), "请求过于频繁");
        assert_eq!(error.retry_after(), Some(Duration::from_millis(1500)));

        let error = client
            .search(Platform::Tencent, "晴天", 5)
            .await
            .unwrap_err();
        assert!(matches!(error, GatewayError::Unavailable(_)));
        assert_eq!(error.to_string(), "音乐服务暂不可用，请稍后再试");
    }

    #[tokio::test]
    async fn failed_jobs_surface_as_remote_errors() {
        let (base, _requests) = canned_server(vec![(
            200,
            r#"{"id":"j","state":{"phase":"failed","error":{"code":"upstream","message":"该歌曲需要会员"}}}"#,
        )])
        .await;
        let client = GatewayClient::new(&base, "t").unwrap();
        let error = client.media(&media_request(), |_| {}).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::Upstream);
        assert_eq!(error.to_string(), "该歌曲需要会员");
    }

    #[test]
    fn rejects_non_http_base_urls() {
        assert!(GatewayClient::new("127.0.0.1:8091", "t").is_err());
        assert!(GatewayClient::new("http://127.0.0.1:8091/", "t").is_ok());
    }
}
