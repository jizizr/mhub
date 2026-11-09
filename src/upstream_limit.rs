//! Per-client quotas on work that reaches a music platform.
//!
//! Only upstream work is metered, at the points where this server decides to
//! leave its own caches: a new download worker, a search the search cache
//! could not answer, a lyrics lookup, catalog metadata. Status polls, audio,
//! covers and anything answered from memory, Telegram or local files cost
//! nothing, so skipping between songs that are already cached is free.
//!
//! - Each client address has its own token bucket per [`Kind`]; nothing is
//!   shared between clients. IPv6 addresses are grouped by /64.
//! - A request short of tokens by at most `max_wait_ms` reserves them and
//!   waits; a longer shortfall is refused with `Retry-After` and consumes
//!   nothing.
//! - A charge can be refunded when the work turns out not to need the
//!   platform after all (a download that joins another one in flight).
//! - `shadow` mode keeps the same books and logs every decision but never
//!   waits or refuses, so quotas can be sized from real traffic first.
//! - Requests without a client (the loopback gateway, background tasks,
//!   loopback callers) are not metered.
//!
//! Every outbound platform request passes [`AuditMiddleware`], which counts
//! it under the unit of work it belongs to; requests seen outside any metered
//! unit point at an upstream path that has no charge point yet.

use crate::{ApiError, ApiResult, settings::SETTINGS};
use axum::{
    extract::{ConnectInfo, Request},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use serde::Deserialize;
use std::{
    collections::HashMap,
    future::Future,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Sweep cadence for the bucket table, in decisions.
const PRUNE_EVERY: u64 = 512;
/// How often the audit and charge counters are written to the log.
const REPORT_EVERY: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Kind {
    /// A new download worker that fetches audio from the platform.
    Download,
    /// An interactive search the search cache could not answer.
    Search,
    /// A paced bulk search for playlist import.
    Import,
    /// A lyrics lookup.
    Lyrics,
    /// Track details, recommendations and playlist resolution.
    Metadata,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Download,
        Kind::Search,
        Kind::Import,
        Kind::Lyrics,
        Kind::Metadata,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Download => "download",
            Kind::Search => "search",
            Kind::Import => "import",
            Kind::Lyrics => "lyrics",
            Kind::Metadata => "metadata",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    /// Keep the books and log, never wait or refuse.
    Shadow,
    #[default]
    Enforce,
}

/// `burst` tokens at most, refilled at `per_minute`. A zero `per_minute`
/// disables the quota for that kind.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Quota {
    pub burst: f64,
    pub per_minute: f64,
}

impl Quota {
    const fn new(burst: f64, per_minute: f64) -> Self {
        Self { burst, per_minute }
    }

    fn enabled(self) -> bool {
        self.burst > 0.0 && self.per_minute > 0.0
    }

    fn per_second(self) -> f64 {
        self.per_minute / 60.0
    }
}

/// `[upstream_limit]` in the config file.
///
/// Defaults come from seven days of access logs (2026-11-02..09). Bursts
/// cover the busiest ten minutes of every ordinary client, counted in
/// requests, which overstates upstream units since cache hits are free;
/// refill rates sit under the sustained hourly rate of the one client that
/// polled and played around the clock (6.1 playback starts, 6.0 searches and
/// 5.5 lyric loads per minute), so only that pattern runs dry. Both were then
/// halved (2026-11-09), and download/lyrics tightened again after replaying
/// the shadow log: a scraper at ~7 songs a minute now runs dry within about
/// five minutes, while no other client of that evening came near a limit.
/// The download burst went down to 20 once enforced (2026-11-09).
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: Mode,
    /// Peers allowed to name the client in `X-Real-IP`, as CIDR blocks.
    pub trusted_proxies: Vec<String>,
    /// A shortfall that refills within this many milliseconds waits instead
    /// of being refused.
    pub max_wait_ms: u64,
    pub download: Quota,
    pub search: Quota,
    pub import: Quota,
    pub lyrics: Quota,
    pub metadata: Quota,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Enforce,
            trusted_proxies: vec!["127.0.0.1/32".to_string(), "::1/128".to_string()],
            max_wait_ms: 3000,
            // Ordinary peak: 59 playback starts in ten minutes.
            download: Quota::new(20.0, 1.0),
            // Ordinary peak: 40 searches in ten minutes, 27 in one.
            search: Quota::new(20.0, 2.0),
            // A match request carries 25 songs and up to two searches each;
            // a 1000-song import takes about forty minutes.
            import: Quota::new(150.0, 30.0),
            // Lyrics page (50 in ten minutes) plus one per played song.
            lyrics: Quota::new(25.0, 2.0),
            // Track details (28 in ten minutes), playlists, recommendations.
            metadata: Quota::new(20.0, 3.0),
        }
    }
}

impl Settings {
    fn quota(&self, kind: Kind) -> Quota {
        match kind {
            Kind::Download => self.download,
            Kind::Search => self.search,
            Kind::Import => self.import,
            Kind::Lyrics => self.lyrics,
            Kind::Metadata => self.metadata,
        }
    }
}

/// The client a request is metered against; `None` is never metered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Principal(Option<IpAddr>);

impl Principal {
    pub const EXEMPT: Principal = Principal(None);

    pub fn client(ip: IpAddr) -> Self {
        let ip = bucket_address(ip);
        Principal((!ip.is_loopback() && !ip.is_unspecified()).then_some(ip))
    }

    pub fn address(self) -> Option<IpAddr> {
        self.0
    }
}

/// IPv4-mapped IPv6 becomes IPv4; other IPv6 addresses are grouped by /64,
/// the block one subscriber usually gets.
fn bucket_address(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return IpAddr::V4(v4);
            }
            let prefix = u128::from(v6) & (u128::MAX << 64);
            IpAddr::V6(Ipv6Addr::from(prefix))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        let (address, prefix) = match value.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix.parse::<u8>().ok()?)),
            None => (value, None),
        };
        let network: IpAddr = address.parse().ok()?;
        let width = if network.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(width);
        (prefix <= width).then_some(Self { network, prefix })
    }

    fn contains(self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            ip => ip,
        };
        match (self.network, ip) {
            (IpAddr::V4(network), IpAddr::V4(ip)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(network) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(network), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(network) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

/// The client behind a connection: `X-Real-IP` when the peer is a trusted
/// proxy (the reverse proxy overwrites it with the verified address), the
/// peer itself otherwise. No peer (an in-process test server) is exempt.
fn principal_for(peer: Option<IpAddr>, headers: &HeaderMap, trusted: &[Cidr]) -> Principal {
    let Some(peer) = peer else {
        return Principal::EXEMPT;
    };
    if trusted.iter().any(|cidr| cidr.contains(peer)) {
        let forwarded = headers
            .get("x-real-ip")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<IpAddr>().ok());
        return Principal::client(forwarded.unwrap_or(peer));
    }
    Principal::client(peer)
}

tokio::task_local! {
    static PRINCIPAL: Principal;
    static UNIT: Kind;
}

/// Axum middleware: runs the request with its client attached.
pub async fn attach_principal(request: Request, next: Next) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    let principal = principal_for(peer, request.headers(), &LIMITER.trusted);
    PRINCIPAL.scope(principal, next.run(request)).await
}

fn current_principal() -> Principal {
    PRINCIPAL
        .try_with(|principal| *principal)
        .unwrap_or(Principal::EXEMPT)
}

/// Runs `work` as part of a `kind` unit, for the outbound request audit. It
/// does not charge; use it where the charge was taken elsewhere.
pub async fn within<F: Future>(kind: Kind, work: F) -> F::Output {
    UNIT.scope(kind, work).await
}

/// Charges the current client one `kind` unit, then runs `work` as that unit.
pub async fn metered<T, F>(kind: Kind, work: F) -> ApiResult<T>
where
    F: Future<Output = ApiResult<T>>,
{
    let _charge = charge(kind).await?;
    within(kind, work).await
}

/// One unit taken from a client's bucket. Dropping it keeps the charge;
/// [`Charge::refund`] gives it back when the platform was not needed.
#[must_use = "a charge is kept unless refunded"]
#[derive(Debug)]
pub struct Charge {
    client: IpAddr,
    kind: Kind,
}

impl Charge {
    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn refund(self) {
        LIMITER.refund(self.client, self.kind, Instant::now());
        LIMITER.count(self.kind, |counts| counts.refunded += 1);
        tracing::info!(
            target: "upstream_limit",
            client = %self.client,
            kind = self.kind.as_str(),
            "upstream charge refunded"
        );
    }
}

/// Charges the current client one `kind` unit. `None` when the request is
/// not metered (exempt client, mode off or quota disabled).
pub async fn charge(kind: Kind) -> ApiResult<Option<Charge>> {
    let Some(client) = current_principal().address() else {
        return Ok(None);
    };
    LIMITER.charge(client, kind).await
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Decision {
    Admit,
    /// Tokens reserved; proceed after this wait.
    Wait(Duration),
    /// Nothing taken; retry after this wait.
    Refuse(Duration),
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl Bucket {
    fn refill(&mut self, quota: Quota, now: Instant) {
        let elapsed = now.saturating_duration_since(self.updated).as_secs_f64();
        self.tokens = (self.tokens + elapsed * quota.per_second()).min(quota.burst);
        self.updated = now;
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Counts {
    charged: u64,
    refunded: u64,
    waited: u64,
    refused: u64,
}

#[derive(Default)]
struct Audit {
    started: Option<Instant>,
    /// (unit or "none", host) -> outbound requests.
    requests: HashMap<(&'static str, String), u64>,
    counts: HashMap<Kind, Counts>,
}

pub struct Limiter {
    settings: Settings,
    trusted: Vec<Cidr>,
    buckets: Mutex<HashMap<(IpAddr, Kind), Bucket>>,
    decisions: AtomicU64,
    audit: Mutex<Audit>,
}

lazy_static::lazy_static! {
    static ref LIMITER: Limiter = Limiter::new(SETTINGS.upstream_limit.clone());
}

impl Limiter {
    fn new(settings: Settings) -> Self {
        let trusted = settings
            .trusted_proxies
            .iter()
            .filter_map(|value| {
                let cidr = Cidr::parse(value);
                if cidr.is_none() {
                    tracing::warn!(
                        "ignoring invalid upstream_limit.trusted_proxies entry {value:?}"
                    );
                }
                cidr
            })
            .collect();
        Self {
            settings,
            trusted,
            buckets: Mutex::new(HashMap::new()),
            decisions: AtomicU64::new(0),
            audit: Mutex::new(Audit::default()),
        }
    }

    fn decide(&self, client: IpAddr, kind: Kind, now: Instant) -> Decision {
        let quota = self.settings.quota(kind);
        let max_wait = Duration::from_millis(self.settings.max_wait_ms);
        let mut buckets = self.buckets.lock().unwrap();
        if self.decisions.fetch_add(1, Ordering::Relaxed) % PRUNE_EVERY == PRUNE_EVERY - 1 {
            let settings = &self.settings;
            buckets.retain(|(_, kind), bucket| {
                let quota = settings.quota(*kind);
                bucket.refill(quota, now);
                bucket.tokens < quota.burst
            });
        }
        let bucket = buckets.entry((client, kind)).or_insert(Bucket {
            tokens: quota.burst,
            updated: now,
        });
        bucket.refill(quota, now);
        let after = bucket.tokens - 1.0;
        if after >= 0.0 {
            bucket.tokens = after;
            return Decision::Admit;
        }
        let wait = Duration::from_secs_f64(-after / quota.per_second());
        if wait <= max_wait {
            bucket.tokens = after;
            Decision::Wait(wait)
        } else {
            // What is missing for one token to be available again.
            let short = 1.0 - bucket.tokens.min(1.0);
            Decision::Refuse(Duration::from_secs_f64(short / quota.per_second()))
        }
    }

    fn refund(&self, client: IpAddr, kind: Kind, now: Instant) {
        let quota = self.settings.quota(kind);
        if let Some(bucket) = self.buckets.lock().unwrap().get_mut(&(client, kind)) {
            bucket.refill(quota, now);
            bucket.tokens = (bucket.tokens + 1.0).min(quota.burst);
        }
    }

    fn remaining(&self, client: IpAddr, kind: Kind) -> f64 {
        self.buckets
            .lock()
            .unwrap()
            .get(&(client, kind))
            .map_or(self.settings.quota(kind).burst, |bucket| bucket.tokens)
    }

    async fn charge(&self, client: IpAddr, kind: Kind) -> ApiResult<Option<Charge>> {
        let quota = self.settings.quota(kind);
        if self.settings.mode == Mode::Off || !quota.enabled() {
            return Ok(None);
        }
        let enforce = self.settings.mode == Mode::Enforce;
        let decision = self.decide(client, kind, Instant::now());
        let remaining = self.remaining(client, kind);
        match decision {
            Decision::Admit => {
                self.count(kind, |counts| counts.charged += 1);
                tracing::info!(
                    target: "upstream_limit",
                    %client,
                    kind = kind.as_str(),
                    remaining = format_args!("{remaining:.2}"),
                    "upstream charge"
                );
            }
            Decision::Wait(wait) => {
                self.count(kind, |counts| {
                    counts.charged += 1;
                    counts.waited += 1;
                });
                tracing::info!(
                    target: "upstream_limit",
                    %client,
                    kind = kind.as_str(),
                    remaining = format_args!("{remaining:.2}"),
                    wait_ms = wait.as_millis() as u64,
                    enforced = enforce,
                    "upstream charge waits for tokens"
                );
                if enforce {
                    tokio::time::sleep(wait).await;
                }
            }
            Decision::Refuse(retry_after) => {
                self.count(kind, |counts| counts.refused += 1);
                tracing::warn!(
                    target: "upstream_limit",
                    %client,
                    kind = kind.as_str(),
                    remaining = format_args!("{remaining:.2}"),
                    retry_after_ms = retry_after.as_millis() as u64,
                    enforced = enforce,
                    "upstream charge over quota"
                );
                if enforce {
                    return Err(ApiError::ClientRateLimited { retry_after });
                }
                // Shadow mode: nothing was taken, so nothing to refund.
                return Ok(None);
            }
        }
        Ok(Some(Charge { client, kind }))
    }

    fn count(&self, kind: Kind, update: impl FnOnce(&mut Counts)) {
        let mut audit = self.audit.lock().unwrap();
        update(audit.counts.entry(kind).or_default());
        self.maybe_report(&mut audit);
    }

    fn observe(&self, unit: Option<Kind>, host: &str) {
        let mut audit = self.audit.lock().unwrap();
        let unit = unit.map_or("none", Kind::as_str);
        *audit.requests.entry((unit, host.to_string())).or_default() += 1;
        self.maybe_report(&mut audit);
    }

    fn maybe_report(&self, audit: &mut Audit) {
        let now = Instant::now();
        let started = *audit.started.get_or_insert(now);
        if now.duration_since(started) < REPORT_EVERY {
            return;
        }
        let minutes = now.duration_since(started).as_secs() / 60;
        for kind in Kind::ALL {
            if let Some(counts) = audit.counts.get(&kind) {
                tracing::info!(
                    target: "upstream_audit",
                    kind = kind.as_str(),
                    minutes,
                    charged = counts.charged,
                    refunded = counts.refunded,
                    waited = counts.waited,
                    refused = counts.refused,
                    "upstream charges"
                );
            }
        }
        let mut requests: Vec<_> = audit.requests.drain().collect();
        requests.sort();
        for ((unit, host), count) in requests {
            tracing::info!(
                target: "upstream_audit",
                unit,
                host = %host,
                minutes,
                requests = count,
                "upstream requests"
            );
        }
        audit.counts.clear();
        audit.started = Some(now);
    }
}

/// Counts every outbound platform request under its unit of work.
pub struct AuditMiddleware;

#[async_trait::async_trait]
impl reqwest_middleware::Middleware for AuditMiddleware {
    async fn handle(
        &self,
        request: reqwest::Request,
        extensions: &mut axum::http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        let unit = UNIT.try_with(|kind| *kind).ok();
        LIMITER.observe(unit, request.url().host_str().unwrap_or("-"));
        next.run(request, extensions).await
    }
}

/// Wraps a platform HTTP client so its requests are audited.
pub fn audited(client: reqwest::Client) -> reqwest_middleware::ClientWithMiddleware {
    reqwest_middleware::ClientBuilder::new(client)
        .with(AuditMiddleware)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn limiter(burst: f64, per_minute: f64, mode: Mode) -> Limiter {
        let quota = Quota::new(burst, per_minute);
        Limiter::new(Settings {
            mode,
            download: quota,
            search: quota,
            import: quota,
            lyrics: quota,
            metadata: quota,
            ..Settings::default()
        })
    }

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, last))
    }

    #[test]
    fn burst_then_wait_then_refuse() {
        // 2 tokens, one per second, waits up to 3 s.
        let limiter = limiter(2.0, 60.0, Mode::Enforce);
        let now = Instant::now();
        assert_eq!(limiter.decide(ip(1), Kind::Download, now), Decision::Admit);
        assert_eq!(limiter.decide(ip(1), Kind::Download, now), Decision::Admit);
        assert_eq!(
            limiter.decide(ip(1), Kind::Download, now),
            Decision::Wait(Duration::from_secs(1))
        );
        assert_eq!(
            limiter.decide(ip(1), Kind::Download, now),
            Decision::Wait(Duration::from_secs(2))
        );
        assert_eq!(
            limiter.decide(ip(1), Kind::Download, now),
            Decision::Wait(Duration::from_secs(3))
        );
        // Three tokens are owed; the next one is 4 s away, over the limit.
        let Decision::Refuse(retry) = limiter.decide(ip(1), Kind::Download, now) else {
            panic!("expected a refusal");
        };
        assert_eq!(retry, Duration::from_secs(4));
        // The refusal took nothing: the three owed tokens and the next one
        // have refilled after 4 s.
        let later = now + Duration::from_secs(4);
        assert_eq!(
            limiter.decide(ip(1), Kind::Download, later),
            Decision::Admit
        );
    }

    #[test]
    fn clients_and_kinds_have_their_own_buckets() {
        let limiter = limiter(1.0, 1.0, Mode::Enforce);
        let now = Instant::now();
        assert_eq!(limiter.decide(ip(1), Kind::Download, now), Decision::Admit);
        assert!(matches!(
            limiter.decide(ip(1), Kind::Download, now),
            Decision::Refuse(_)
        ));
        assert_eq!(limiter.decide(ip(2), Kind::Download, now), Decision::Admit);
        assert_eq!(limiter.decide(ip(1), Kind::Search, now), Decision::Admit);
    }

    #[test]
    fn refund_returns_the_token() {
        let limiter = limiter(1.0, 1.0, Mode::Enforce);
        let now = Instant::now();
        assert_eq!(limiter.decide(ip(1), Kind::Download, now), Decision::Admit);
        limiter.refund(ip(1), Kind::Download, now);
        assert_eq!(limiter.decide(ip(1), Kind::Download, now), Decision::Admit);
        // Refunds never lift a bucket over its burst.
        limiter.refund(ip(1), Kind::Download, now);
        limiter.refund(ip(1), Kind::Download, now);
        assert_eq!(limiter.remaining(ip(1), Kind::Download), 1.0);
    }

    #[tokio::test]
    async fn shadow_mode_never_refuses() {
        let limiter = limiter(1.0, 1.0, Mode::Shadow);
        assert!(limiter.charge(ip(1), Kind::Search).await.unwrap().is_some());
        assert!(limiter.charge(ip(1), Kind::Search).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn enforce_mode_refuses_with_retry_after() {
        let limiter = limiter(1.0, 1.0, Mode::Enforce);
        assert!(limiter.charge(ip(1), Kind::Search).await.unwrap().is_some());
        let err = limiter.charge(ip(1), Kind::Search).await.unwrap_err();
        assert!(
            matches!(err, ApiError::ClientRateLimited { retry_after } if retry_after > Duration::from_secs(50))
        );
    }

    #[tokio::test]
    async fn off_mode_and_disabled_quota_are_free() {
        let off = limiter(1.0, 1.0, Mode::Off);
        assert!(off.charge(ip(1), Kind::Search).await.unwrap().is_none());
        let disabled = limiter(1.0, 0.0, Mode::Enforce);
        for _ in 0..5 {
            assert!(
                disabled
                    .charge(ip(1), Kind::Search)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn ipv6_clients_share_their_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(Principal::client(a), Principal::client(b));
        assert_ne!(Principal::client(a), Principal::client(c));
        let mapped: IpAddr = "::ffff:203.0.113.7".parse().unwrap();
        assert_eq!(Principal::client(mapped), Principal::client(ip(7)));
    }

    #[test]
    fn cidr_matching() {
        let v4 = Cidr::parse("10.1.0.0/16").unwrap();
        assert!(v4.contains("10.1.200.3".parse().unwrap()));
        assert!(!v4.contains("10.2.0.1".parse().unwrap()));
        assert!(v4.contains("::ffff:10.1.0.9".parse().unwrap()));
        let all = Cidr::parse("0.0.0.0/0").unwrap();
        assert!(all.contains(ip(1)));
        let single = Cidr::parse("::1").unwrap();
        assert!(single.contains("::1".parse().unwrap()));
        assert!(!single.contains("::2".parse().unwrap()));
        assert!(Cidr::parse("10.0.0.0/33").is_none());
        assert!(Cidr::parse("nonsense").is_none());
    }

    #[test]
    fn real_ip_is_trusted_only_from_proxies() {
        let trusted = vec![Cidr::parse("127.0.0.1/32").unwrap()];
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "198.51.100.4".parse().unwrap());
        let loopback = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(
            principal_for(loopback, &headers, &trusted).address(),
            Some("198.51.100.4".parse().unwrap())
        );
        // A direct client cannot pick its own bucket.
        assert_eq!(
            principal_for(Some(ip(9)), &headers, &trusted).address(),
            Some(ip(9))
        );
        // The proxy without a header, and in-process callers, are exempt.
        assert_eq!(
            principal_for(loopback, &HeaderMap::new(), &trusted),
            Principal::EXEMPT
        );
        assert_eq!(principal_for(None, &headers, &trusted), Principal::EXEMPT);
        // Garbage in the header falls back to the (loopback) peer.
        headers.insert("x-real-ip", "not-an-ip".parse().unwrap());
        assert_eq!(
            principal_for(loopback, &headers, &trusted),
            Principal::EXEMPT
        );
    }

    #[tokio::test]
    async fn middleware_meters_the_forwarded_client() {
        use axum::{Router, routing::get};
        async fn whoami() -> String {
            current_principal()
                .address()
                .map_or("exempt".to_string(), |ip| ip.to_string())
        }
        let app = Router::new()
            .route("/", get(whoami))
            .layer(axum::middleware::from_fn(attach_principal));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap()
        });
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/");
        let forwarded = client
            .get(&url)
            .header("x-real-ip", "198.51.100.4")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(forwarded, "198.51.100.4");
        let direct = client.get(&url).send().await.unwrap().text().await.unwrap();
        assert_eq!(direct, "exempt");
        server.abort();
    }

    #[tokio::test]
    async fn requests_without_a_client_are_not_metered() {
        assert!(charge(Kind::Download).await.unwrap().is_none());
        let exempt = PRINCIPAL
            .scope(Principal::EXEMPT, charge(Kind::Download))
            .await
            .unwrap();
        assert!(exempt.is_none());
    }
}
