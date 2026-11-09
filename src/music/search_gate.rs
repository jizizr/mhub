//! Paces every platform search that leaves this server and caches the
//! results, so bulk work such as playlist import cannot exhaust a platform's
//! search quota for everyone sharing the server's token and address.
//!
//! - Each platform gets its own pace: one upstream search per `interval`.
//!   A throttled answer (HTTP 429) pauses the platform, honouring
//!   `Retry-After` when given, and doubles the interval; a run of successes
//!   shortens it again. The pace settles just under the real quota without
//!   hard-coding it.
//! - Interactive searches never queue behind bulk ones: they take the next
//!   slot at once and push the bulk queue back by one interval.
//! - Bulk searches carry a deadline. A slot later than the deadline is not
//!   reserved; the caller gets `ApiError::RateLimited` with the wait, so a
//!   request answers in bounded time and the client retries the rest later.
//! - Results (including empty ones) are cached per query, so retries,
//!   re-imports and overlapping playlists cost no upstream requests.

use super::provider::{MusicPlatform, MusicSearchItem};
use crate::{ApiError, ApiResult, upstream_limit};
use std::{
    collections::HashMap,
    future::Future,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub enum Lane {
    /// A person waiting on a search box.
    Interactive,
    /// Background work that can be resumed later; gives up at `deadline`.
    Bulk { deadline: Instant },
}

#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    pub start_interval: Duration,
    pub min_interval: Duration,
    pub max_interval: Duration,
    /// Consecutive successes before the interval is shortened by 10%.
    pub speed_up_after: u32,
    pub first_backoff: Duration,
    pub max_backoff: Duration,
    /// An interactive search waits out a pause up to this long, then fails.
    pub interactive_wait: Duration,
    /// Upper bound for one upstream bulk search.
    pub bulk_timeout: Duration,
    pub cache_ttl: Duration,
    pub cache_capacity: usize,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            start_interval: Duration::from_millis(250),
            min_interval: Duration::from_millis(100),
            max_interval: Duration::from_secs(2),
            speed_up_after: 30,
            first_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(60),
            interactive_wait: Duration::from_secs(3),
            bulk_timeout: Duration::from_secs(12),
            cache_ttl: Duration::from_secs(6 * 60 * 60),
            cache_capacity: 20_000,
        }
    }
}

struct Pace {
    interval: Duration,
    next_slot: Instant,
    paused_until: Instant,
    backoff: Duration,
    calm: u32,
}

type CacheKey = (MusicPlatform, String, usize);

pub struct SearchGate {
    tuning: Tuning,
    paces: Mutex<HashMap<MusicPlatform, Pace>>,
    cache: Mutex<HashMap<CacheKey, (Instant, Vec<MusicSearchItem>)>>,
}

impl SearchGate {
    pub fn new(tuning: Tuning) -> Self {
        Self {
            tuning,
            paces: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn search<F, Fut>(
        &self,
        platform: MusicPlatform,
        keyword: &str,
        limit: usize,
        lane: Lane,
        upstream: F,
    ) -> ApiResult<Vec<MusicSearchItem>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ApiResult<Vec<MusicSearchItem>>>,
    {
        let key = (platform, keyword.trim().to_string(), limit);
        if let Some(items) = self.cached(&key) {
            return Ok(items);
        }
        // Only a search that leaves the cache costs the client.
        let unit = match lane {
            Lane::Interactive => upstream_limit::Kind::Search,
            Lane::Bulk { .. } => upstream_limit::Kind::Import,
        };
        let charge = upstream_limit::charge(unit).await?;
        if let Err(error) = self.admit(platform, lane).await {
            if let Some(charge) = charge {
                charge.refund();
            }
            return Err(error);
        }
        let result = upstream_limit::within(unit, async {
            match lane {
                Lane::Interactive => upstream().await,
                Lane::Bulk { .. } => tokio::time::timeout(self.tuning.bulk_timeout, upstream())
                    .await
                    .unwrap_or_else(|_| Err(ApiError::Custom("平台搜索超时".to_string()))),
            }
        })
        .await;
        match &result {
            Ok(items) => {
                self.record_success(platform);
                self.store(key, items.clone());
            }
            Err(err) => {
                if let Some(retry_after) = throttle_hint(err) {
                    self.record_throttle(platform, retry_after);
                }
            }
        }
        result
    }

    fn pace<'a>(
        &self,
        paces: &'a mut HashMap<MusicPlatform, Pace>,
        platform: MusicPlatform,
    ) -> &'a mut Pace {
        paces.entry(platform).or_insert_with(|| {
            let now = Instant::now();
            Pace {
                interval: self.tuning.start_interval,
                next_slot: now,
                paused_until: now,
                backoff: self.tuning.first_backoff,
                calm: 0,
            }
        })
    }

    async fn admit(&self, platform: MusicPlatform, lane: Lane) -> ApiResult<()> {
        loop {
            let now = Instant::now();
            let slot = {
                let mut paces = self.paces.lock().unwrap();
                let pace = self.pace(&mut paces, platform);
                match lane {
                    Lane::Interactive => {
                        let slot = now.max(pace.paused_until);
                        if slot - now > self.tuning.interactive_wait {
                            return Err(rate_limited(platform, slot - now));
                        }
                        // Take the slot ahead of the bulk queue, which yields
                        // one interval so the overall pace is unchanged.
                        pace.next_slot = pace.next_slot.max(slot) + pace.interval;
                        slot
                    }
                    Lane::Bulk { deadline } => {
                        let slot = now.max(pace.next_slot).max(pace.paused_until);
                        if slot > deadline {
                            return Err(rate_limited(platform, slot - now));
                        }
                        pace.next_slot = slot + pace.interval;
                        slot
                    }
                }
            };
            tokio::time::sleep_until(slot.into()).await;
            // A throttle reported while this caller waited moves its slot.
            let paused_until = self
                .pace(&mut self.paces.lock().unwrap(), platform)
                .paused_until;
            if paused_until <= Instant::now() {
                return Ok(());
            }
        }
    }

    fn record_success(&self, platform: MusicPlatform) {
        let mut paces = self.paces.lock().unwrap();
        let pace = self.pace(&mut paces, platform);
        pace.backoff = self.tuning.first_backoff;
        pace.calm += 1;
        if pace.calm >= self.tuning.speed_up_after {
            pace.calm = 0;
            pace.interval = pace.interval.mul_f64(0.9).max(self.tuning.min_interval);
        }
    }

    fn record_throttle(&self, platform: MusicPlatform, retry_after: Option<Duration>) {
        let mut paces = self.paces.lock().unwrap();
        let pace = self.pace(&mut paces, platform);
        let now = Instant::now();
        // Concurrent in-flight searches report the same throttle; count it once.
        if pace.paused_until > now {
            return;
        }
        let wait = retry_after
            .unwrap_or(pace.backoff)
            .min(self.tuning.max_backoff);
        pace.paused_until = now + wait;
        pace.backoff = (pace.backoff * 2).min(self.tuning.max_backoff);
        pace.interval = (pace.interval * 2).min(self.tuning.max_interval);
        pace.calm = 0;
        tracing::warn!(
            platform = platform.id(),
            wait_ms = wait.as_millis() as u64,
            interval_ms = pace.interval.as_millis() as u64,
            "platform search throttled; pausing"
        );
    }

    fn cached(&self, key: &CacheKey) -> Option<Vec<MusicSearchItem>> {
        let cache = self.cache.lock().unwrap();
        cache
            .get(key)
            .filter(|(at, _)| at.elapsed() < self.tuning.cache_ttl)
            .map(|(_, items)| items.clone())
    }

    fn store(&self, key: CacheKey, items: Vec<MusicSearchItem>) {
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= self.tuning.cache_capacity {
            let ttl = self.tuning.cache_ttl;
            cache.retain(|_, (at, _)| at.elapsed() < ttl);
            if cache.len() >= self.tuning.cache_capacity {
                cache.clear();
            }
        }
        cache.insert(key, (Instant::now(), items));
    }
}

fn rate_limited(platform: MusicPlatform, wait: Duration) -> ApiError {
    ApiError::RateLimited {
        message: format!("{} 搜索繁忙，请稍后再试", platform.label()),
        retry_after: Some(wait),
    }
}

/// `Some(retry_after)` when an upstream error means "too many requests".
fn throttle_hint(err: &ApiError) -> Option<Option<Duration>> {
    match err {
        ApiError::RateLimited { retry_after, .. } => Some(*retry_after),
        ApiError::Http { status, .. } if status.as_u16() == 429 => Some(None),
        ApiError::Request(err) if err.status().map(|s| s.as_u16()) == Some(429) => Some(None),
        other => {
            let text = other.to_string();
            (text.contains("HTTP 429") || text.contains("429 Too Many Requests")).then_some(None)
        }
    }
}

/// Parses an HTTP `Retry-After` header given in seconds.
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn tuning() -> Tuning {
        Tuning {
            start_interval: Duration::from_millis(20),
            min_interval: Duration::from_millis(10),
            max_interval: Duration::from_millis(200),
            speed_up_after: 2,
            first_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(1),
            interactive_wait: Duration::from_millis(30),
            bulk_timeout: Duration::from_secs(1),
            cache_ttl: Duration::from_secs(60),
            cache_capacity: 100,
        }
    }

    fn item(id: &str) -> MusicSearchItem {
        MusicSearchItem {
            platform: MusicPlatform::AppleMusic,
            id: id.to_string(),
            song: id.to_string(),
            singer: String::new(),
            cover: String::new(),
        }
    }

    fn bulk(after: Duration) -> Lane {
        Lane::Bulk {
            deadline: Instant::now() + after,
        }
    }

    const AM: MusicPlatform = MusicPlatform::AppleMusic;

    #[tokio::test]
    async fn bulk_searches_are_spaced_by_the_interval() {
        // Keep the interval fixed: with the shared tuning, the second success
        // would already shorten the last gap to 18ms.
        let gate = SearchGate::new(Tuning {
            speed_up_after: u32::MAX,
            ..tuning()
        });
        let started = Instant::now();
        for index in 0..4 {
            gate.search(
                AM,
                &format!("q{index}"),
                8,
                bulk(Duration::from_secs(5)),
                || async { Ok(vec![]) },
            )
            .await
            .unwrap();
        }
        // Three gaps of 20ms after the immediate first slot.
        assert!(started.elapsed() >= Duration::from_millis(60));
    }

    #[tokio::test]
    async fn repeated_queries_are_served_from_cache() {
        let gate = SearchGate::new(tuning());
        let calls = AtomicUsize::new(0);
        for _ in 0..3 {
            let items = gate
                .search(AM, " 晴天 周杰伦 ", 8, Lane::Interactive, || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(vec![item("1")])
                })
                .await
                .unwrap();
            assert_eq!(items, vec![item("1")]);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn throttle_pauses_the_platform_and_slows_the_pace() {
        let gate = SearchGate::new(tuning());
        let err = gate
            .search(AM, "a", 8, bulk(Duration::from_secs(5)), || async {
                Err(ApiError::Custom(
                    "Apple Music API 请求失败：HTTP 429 Too Many Requests".into(),
                ))
            })
            .await
            .unwrap_err();
        assert!(throttle_hint(&err).is_some());
        // Nothing is cached for a failure, and the deadline falls inside the
        // 100ms pause: the caller is told to come back without a request.
        let calls = AtomicUsize::new(0);
        let deferred = gate
            .search(AM, "a", 8, bulk(Duration::from_millis(50)), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(vec![])
            })
            .await
            .unwrap_err();
        assert!(
            matches!(deferred, ApiError::RateLimited { retry_after: Some(wait), .. } if wait > Duration::from_millis(50))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let paces = gate.paces.lock().unwrap();
        assert_eq!(paces[&AM].interval, Duration::from_millis(40));
    }

    #[tokio::test]
    async fn retry_after_header_sets_the_pause() {
        let gate = SearchGate::new(tuning());
        let _ = gate
            .search(AM, "a", 8, Lane::Interactive, || async {
                Err(ApiError::RateLimited {
                    message: "429".into(),
                    retry_after: Some(Duration::from_millis(500)),
                })
            })
            .await;
        // Interactive searches only wait out short pauses.
        let err = gate
            .search(AM, "b", 8, Lane::Interactive, || async { Ok(vec![]) })
            .await
            .unwrap_err();
        assert!(
            matches!(err, ApiError::RateLimited { retry_after: Some(wait), .. } if wait > Duration::from_millis(400))
        );
    }

    #[tokio::test]
    async fn interactive_search_skips_the_bulk_queue() {
        let gate = Arc::new(SearchGate::new(Tuning {
            start_interval: Duration::from_millis(100),
            ..tuning()
        }));
        // Reserve five bulk slots, 0..400ms out.
        let mut bulk_jobs = Vec::new();
        for index in 0..5 {
            let gate = gate.clone();
            bulk_jobs.push(tokio::spawn(async move {
                gate.search(
                    AM,
                    &format!("bulk{index}"),
                    8,
                    bulk(Duration::from_secs(5)),
                    || async { Ok(vec![]) },
                )
                .await
            }));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        let started = Instant::now();
        gate.search(AM, "now", 8, Lane::Interactive, || async { Ok(vec![]) })
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(50));
        for job in bulk_jobs {
            job.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn successes_shorten_the_interval_down_to_the_floor() {
        let gate = SearchGate::new(tuning());
        for index in 0..20 {
            gate.search(
                AM,
                &format!("q{index}"),
                8,
                bulk(Duration::from_secs(5)),
                || async { Ok(vec![]) },
            )
            .await
            .unwrap();
        }
        let paces = gate.paces.lock().unwrap();
        assert_eq!(paces[&AM].interval, Duration::from_millis(10));
    }

    #[test]
    fn retry_after_seconds_are_parsed() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(7)));
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(parse_retry_after(&headers), None);
    }
}
