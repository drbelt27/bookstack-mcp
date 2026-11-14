//! Process-global token-bucket rate limiter for BookStack API requests.
//!
//! BookStack runs on Laravel's per-user throttle (default 180 req/min). When
//! the embedder + worker share a token they walk the API in lockstep and
//! cascade through 429s. The limiter caps the local issue rate to whatever
//! BookStack advertises, with backoff on 429.
//!
//! Each BookStackClient uses both `shared()` for the process-wide ceiling and
//! `for_token()` for fairness across short-lived per-request clients.

use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

const DEFAULT_RPM: f64 = 180.0;
const DEFAULT_PER_TOKEN_RPM: f64 = 60.0;

fn per_token_rpm(value: Option<&str>) -> f64 {
    value
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(DEFAULT_PER_TOKEN_RPM)
}

#[derive(Debug)]
struct Inner {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last_refill: Instant,
    observed_limit_rpm: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct RateLimiter {
    inner: Arc<Mutex<Inner>>,
}

impl RateLimiter {
    pub fn new(rpm: f64) -> Self {
        Self::with_burst(rpm, rpm)
    }

    fn with_burst(rpm: f64, burst: f64) -> Self {
        let rpm = rpm.max(1.0);
        let burst = burst.max(rpm);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                capacity: burst,
                tokens: burst,
                refill_per_sec: rpm / 60.0,
                last_refill: Instant::now(),
                observed_limit_rpm: None,
            })),
        }
    }

    pub fn from_env() -> Self {
        let rpm = env::var("BSMCP_BOOKSTACK_RATE_LIMIT_PER_MIN")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(DEFAULT_RPM);
        Self::new(rpm)
    }

    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut inner = self.inner.lock().await;
                refill(&mut inner);
                if inner.tokens >= 1.0 {
                    inner.tokens -= 1.0;
                    return;
                }
                let deficit = 1.0 - inner.tokens;
                let secs = deficit / inner.refill_per_sec.max(f64::MIN_POSITIVE);
                Duration::from_secs_f64(secs.max(0.001))
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Adjust the local cap based on `X-RateLimit-Limit` advertised by
    /// BookStack. The cap is **monotonically non-increasing** for the life
    /// of the process — once we've seen a tighter limit we stay at that
    /// floor even if BookStack later advertises a higher one. To widen the
    /// cap after BookStack relaxes its throttle, restart the process.
    pub fn observe_limit(&self, headers: &reqwest::header::HeaderMap) {
        let Some(value) = headers.get("X-RateLimit-Limit") else {
            return;
        };
        let Some(s) = value.to_str().ok() else { return };
        let Ok(rpm) = s.trim().parse::<u32>() else {
            return;
        };
        let rpm = rpm as f64;
        if rpm <= 0.0 {
            return;
        }
        if let Ok(mut inner) = self.inner.try_lock() {
            inner.observed_limit_rpm = Some(rpm);
            // Defensive: only ever lower the local cap. If BookStack advertises
            // a higher limit than env-configured, ignore it — env wins.
            if rpm < inner.refill_per_sec * 60.0 {
                inner.capacity = inner.capacity.min(rpm);
                inner.refill_per_sec = rpm / 60.0;
                inner.tokens = inner.tokens.min(inner.capacity);
            }
        }
    }
}

/// Counter used to ensure successive jitter calls within the same process
/// can't return the same value when invoked back-to-back on different
/// callers (embedder + worker) — paired with wall-clock nanos this gives
/// us enough entropy to desync stampedes without pulling in `rand`.
static JITTER_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Return a uniform jitter in `[0, max_ms)` milliseconds. Uses
/// wall-clock nanoseconds + a process-local counter for entropy — no
/// external RNG dependency. Good enough to desync two workers that share
/// a `Retry-After=N` value.
pub fn jitter(max_ms: u64) -> Duration {
    if max_ms == 0 {
        return Duration::from_millis(0);
    }
    let counter = JITTER_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    // Mix nanos and counter so two calls in the same nanosecond still
    // diverge. The exact distribution doesn't matter — we just need
    // non-trivially different sleeps per caller.
    let mixed = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15) // golden-ratio multiplier
        .wrapping_add(counter.wrapping_mul(0xBF58_476D_1CE4_E5B9));
    Duration::from_millis(mixed % max_ms)
}

fn refill(inner: &mut Inner) {
    let now = Instant::now();
    let elapsed = now.duration_since(inner.last_refill).as_secs_f64();
    if elapsed > 0.0 {
        inner.tokens = (inner.tokens + elapsed * inner.refill_per_sec).min(inner.capacity);
        inner.last_refill = now;
    }
}

static GLOBAL: OnceLock<RateLimiter> = OnceLock::new();
static PER_TOKEN: OnceLock<std::sync::Mutex<HashMap<String, (RateLimiter, Instant)>>> =
    OnceLock::new();

pub fn shared() -> RateLimiter {
    GLOBAL.get_or_init(RateLimiter::from_env).clone()
}

/// A per-token cap shared across short-lived clients in this process. The key
/// hashes the full credential, so an invalid secret cannot consume another
/// account's bucket. Idle buckets expire after 10 minutes.
pub fn for_token(instance: &str, token_id: &str, token_secret: &str) -> RateLimiter {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(instance.as_bytes());
    hasher.update([0]);
    hasher.update(token_id.as_bytes());
    hasher.update([0]);
    hasher.update(token_secret.as_bytes());
    let key = format!("{:x}", hasher.finalize());
    let mut buckets = PER_TOKEN
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    let now = Instant::now();
    if let Some((limiter, last_used)) = buckets.get_mut(&key) {
        *last_used = now;
        return limiter.clone();
    }
    buckets.retain(|_, (limiter, last_used)| {
        Arc::strong_count(&limiter.inner) > 1
            || now.duration_since(*last_used) < Duration::from_secs(600)
    });
    let rpm = per_token_rpm(
        env::var("BSMCP_BOOKSTACK_PER_TOKEN_RATE_LIMIT_PER_MIN")
            .ok()
            .as_deref(),
    );
    let limiter = RateLimiter::with_burst(rpm, rpm.max(90.0));
    buckets.insert(key, (limiter.clone(), now));
    limiter
}

/// Parse a `Retry-After` HTTP header value as integer seconds. Returns
/// `None` for HTTP-date forms (RFC 7231 allows them but BookStack/Laravel
/// emits integer seconds in practice).
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn per_token_bucket_is_shared_without_blocking_another_token() {
        let a = for_token("http://bookstack.test", "reader-a", "secret-a");
        let same_a = for_token("http://bookstack.test", "reader-a", "secret-a");
        let b = for_token("http://bookstack.test", "reader-b", "secret-b");
        for _ in 0..90 {
            a.acquire().await;
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(10), same_a.acquire())
                .await
                .is_err()
        );
        assert!(tokio::time::timeout(Duration::from_millis(10), b.acquire())
            .await
            .is_ok());
        drop(a);
        drop(same_a);
        let next_request = for_token("http://bookstack.test", "reader-a", "secret-a");
        assert!(
            tokio::time::timeout(Duration::from_millis(10), next_request.acquire())
                .await
                .is_err(),
            "a new MCP request must not reset the token budget"
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn per_token_burst_covers_a_large_acl_search_without_raising_sustained_rate() {
        let limiter = RateLimiter::with_burst(60.0, 90.0);
        for _ in 0..90 {
            assert!(
                tokio::time::timeout(Duration::from_millis(10), limiter.acquire())
                    .await
                    .is_ok()
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(10), limiter.acquire())
                .await
                .is_err()
        );
        // `acquire()` starts after the clock advances; refill is calculated
        // from its last actual acquisition, not from a spawned timer.
        tokio::time::advance(Duration::from_secs(2)).await;
        let inner = limiter.inner.lock().await;
        assert!(inner.tokens < 1.0);
        assert_eq!(inner.capacity, 90.0);
        assert_eq!(inner.refill_per_sec, 1.0);
    }

    #[test]
    fn per_token_rate_accepts_positive_override_only() {
        assert_eq!(per_token_rpm(Some("180")), 180.0);
        assert_eq!(per_token_rpm(Some("0")), 60.0);
        assert_eq!(per_token_rpm(Some("bad")), 60.0);
        assert_eq!(per_token_rpm(None), 60.0);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn wrong_secret_does_not_drain_the_real_tokens_budget() {
        let real = for_token("http://same.test", "shared-id", "right-secret");
        let wrong = for_token("http://same.test", "shared-id", "wrong-secret");
        for _ in 0..60 {
            wrong.acquire().await;
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(10), real.acquire())
                .await
                .is_ok()
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn active_sse_bucket_is_not_evicted_when_another_user_connects() {
        let session = for_token("http://sse.test", "session-id", "secret");
        tokio::time::advance(Duration::from_secs(601)).await;
        for_token("http://sse.test", "other-id", "secret");
        let next_request = for_token("http://sse.test", "session-id", "secret");
        assert!(
            Arc::ptr_eq(&session.inner, &next_request.inner),
            "a live SSE client must retain its original bucket"
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn global_bucket_still_caps_distinct_tokens() {
        let global = RateLimiter::new(2.0);
        for token in ["first", "second"] {
            for_token("http://separate.test", token, "secret")
                .acquire()
                .await;
            global.acquire().await;
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(10), global.acquire())
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn acquire_blocks_when_bucket_empty() {
        let limiter = RateLimiter::new(60.0);
        for _ in 0..60 {
            limiter.acquire().await;
        }
        let start = Instant::now();
        let acquire = limiter.acquire();
        tokio::time::advance(Duration::from_millis(1100)).await;
        acquire.await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(900),
            "expected to wait ~1s for refill, got {:?}",
            elapsed
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn refill_replenishes_over_time() {
        let limiter = RateLimiter::new(60.0);
        for _ in 0..60 {
            limiter.acquire().await;
        }
        // Advance 30 seconds: at 60 rpm = 1 token/sec, we should have ~30 tokens.
        tokio::time::advance(Duration::from_secs(30)).await;
        for _ in 0..29 {
            limiter.acquire().await;
        }
    }

    #[tokio::test]
    async fn observe_limit_lowers_capacity() {
        let limiter = RateLimiter::new(180.0);
        let mut headers = HeaderMap::new();
        headers.insert("X-RateLimit-Limit", HeaderValue::from_static("60"));
        limiter.observe_limit(&headers);
        let inner = limiter.inner.lock().await;
        assert_eq!(inner.capacity, 60.0);
        assert!((inner.refill_per_sec - 1.0).abs() < f64::EPSILON);
        assert_eq!(inner.observed_limit_rpm, Some(60.0));
    }

    #[tokio::test]
    async fn observed_limit_between_refill_and_burst_never_raises_refill() {
        let limiter = RateLimiter::with_burst(60.0, 90.0);
        let mut headers = HeaderMap::new();
        headers.insert("X-RateLimit-Limit", HeaderValue::from_static("80"));
        limiter.observe_limit(&headers);
        let inner = limiter.inner.lock().await;
        assert_eq!(inner.refill_per_sec, 1.0);
        assert_eq!(inner.capacity, 90.0);
    }

    #[tokio::test]
    async fn observe_limit_never_raises() {
        let limiter = RateLimiter::new(60.0);
        let mut headers = HeaderMap::new();
        headers.insert("X-RateLimit-Limit", HeaderValue::from_static("9999"));
        limiter.observe_limit(&headers);
        let inner = limiter.inner.lock().await;
        assert_eq!(inner.capacity, 60.0);
        assert_eq!(inner.observed_limit_rpm, Some(9999.0));
    }

    #[tokio::test]
    async fn observe_limit_ignores_garbage_header() {
        let limiter = RateLimiter::new(60.0);
        let mut headers = HeaderMap::new();
        headers.insert("X-RateLimit-Limit", HeaderValue::from_static("nope"));
        limiter.observe_limit(&headers);
        let inner = limiter.inner.lock().await;
        assert_eq!(inner.capacity, 60.0);
        assert_eq!(inner.observed_limit_rpm, None);
    }

    #[test]
    fn parse_retry_after_integer_seconds() {
        assert_eq!(parse_retry_after("12"), Some(Duration::from_secs(12)));
        assert_eq!(parse_retry_after(" 7 "), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after("0"), Some(Duration::from_secs(0)));
    }

    #[test]
    fn parse_retry_after_rejects_http_date() {
        // HTTP-date form is allowed by RFC 7231 but BookStack/Laravel uses
        // integer seconds. Document the limitation by asserting we don't
        // accidentally parse a date.
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("not-a-number"), None);
    }

    #[test]
    fn jitter_within_bound() {
        for _ in 0..100 {
            let j = jitter(500);
            assert!(j < Duration::from_millis(500), "jitter {:?} not < 500ms", j);
        }
    }

    #[test]
    fn jitter_zero_returns_zero() {
        assert_eq!(jitter(0), Duration::from_millis(0));
    }

    #[test]
    fn jitter_desyncs_concurrent_callers() {
        // Two back-to-back callers with the same Retry-After value should
        // not sleep for the exact same duration — that's the whole point.
        // Sample a batch and assert at least two distinct values appear.
        let mut samples = std::collections::HashSet::new();
        for _ in 0..32 {
            samples.insert(jitter(500).as_millis());
        }
        assert!(
            samples.len() > 1,
            "jitter degenerated to a constant — embedder + worker would still stampede"
        );
    }
}
