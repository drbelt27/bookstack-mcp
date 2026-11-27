//! Load state + retry/backoff for the embedder model, so a transient failure
//! (Hugging Face download, provider API blip) degrades instead of crash-looping.

use std::future::Future;
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadState {
    Loading,
    Ready,
    Degraded {
        last_error: String,
        attempt: u32,
        /// Unix seconds; `None` once max attempts are exhausted (no more retries).
        next_retry_at: Option<u64>,
    },
}

pub struct LoadStatus(RwLock<LoadState>);

impl LoadStatus {
    pub fn new() -> Self {
        Self(RwLock::new(LoadState::Loading))
    }

    pub fn get(&self) -> LoadState {
        self.0.read().unwrap().clone()
    }

    fn set(&self, s: LoadState) {
        *self.0.write().unwrap() = s;
    }

    pub fn to_json(&self) -> Value {
        match self.get() {
            LoadState::Loading => json!({"state": "loading"}),
            LoadState::Ready => json!({"state": "ready"}),
            LoadState::Degraded {
                last_error,
                attempt,
                next_retry_at,
            } => json!({
                "state": "degraded",
                "last_error": last_error,
                "attempt": attempt,
                "next_retry_at": next_retry_at,
            }),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RetryCfg {
    pub max_attempts: u32,
    pub base: Duration,
    pub max: Duration,
}

impl RetryCfg {
    pub fn from_env() -> Self {
        let num = |k: &str, d: u64| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(d)
        };
        Self {
            max_attempts: num("BSMCP_EMBED_LOAD_MAX_ATTEMPTS", 8) as u32,
            base: Duration::from_secs(num("BSMCP_EMBED_LOAD_BACKOFF_SECS", 5)),
            max: Duration::from_secs(num("BSMCP_EMBED_LOAD_BACKOFF_MAX_SECS", 300)),
        }
    }

    /// Exponential delay for `attempt` (1-based), capped at `max`, plus up to 20% jitter.
    fn delay(&self, attempt: u32) -> Duration {
        let exp = self
            .base
            .saturating_mul(1u32 << (attempt - 1).min(20))
            .min(self.max);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() as u64);
        let jitter_ms = (exp.as_millis() as u64 / 5).max(1);
        exp + Duration::from_millis(nanos % jitter_ms)
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Strip what must not reach `/health`-style output: URL query strings,
/// bearer tokens, the configured API keys; cap the length.
pub fn sanitize(msg: &str) -> String {
    let mut out = msg.to_string();
    for var in ["BSMCP_EMBED_API_KEY", "BSMCP_RERANK_API_KEY"] {
        if let Ok(k) = std::env::var(var) {
            if k.len() >= 4 {
                out = out.replace(&k, "[redacted]");
            }
        }
    }
    let mut cleaned = String::with_capacity(out.len());
    let mut after_bearer = false;
    for part in out.split_inclusive(char::is_whitespace) {
        let word = part.trim_end();
        let ws = &part[word.len()..];
        if after_bearer {
            cleaned.push_str("[redacted]");
            after_bearer = false;
        } else {
            let w = word.split('?').next().unwrap_or(word);
            cleaned.push_str(w);
            after_bearer = w.eq_ignore_ascii_case("bearer");
        }
        cleaned.push_str(ws);
    }
    cleaned.chars().take(300).collect()
}

/// Run `load` until it succeeds or `cfg.max_attempts` is exhausted, keeping
/// `status` current. Returns `None` when giving up (status stays `Degraded`).
pub async fn retry_load<T, F, Fut>(cfg: &RetryCfg, status: &LoadStatus, mut load: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    for attempt in 1..=cfg.max_attempts {
        match load().await {
            Ok(v) => {
                status.set(LoadState::Ready);
                return Some(v);
            }
            Err(e) => {
                let last_error = sanitize(&e);
                let delay = (attempt < cfg.max_attempts).then(|| cfg.delay(attempt));
                tracing::warn!(attempt, error = %last_error, retry_in_s = ?delay.map(|d| d.as_secs()), "embedder_load_failed");
                status.set(LoadState::Degraded {
                    last_error,
                    attempt,
                    next_retry_at: delay.map(|d| now_unix() + d.as_secs()),
                });
                if let Some(d) = delay {
                    tokio::time::sleep(d).await;
                }
            }
        }
    }
    tracing::error!(
        max_attempts = cfg.max_attempts,
        "embedder_load_gave_up_staying_degraded"
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn cfg(max: u32) -> RetryCfg {
        RetryCfg {
            max_attempts: max,
            base: Duration::from_millis(1),
            max: Duration::from_millis(4),
        }
    }

    #[tokio::test]
    async fn fails_twice_then_ready() {
        let st = LoadStatus::new();
        let n = AtomicU32::new(0);
        let r = retry_load(&cfg(5), &st, || async {
            if n.fetch_add(1, Ordering::SeqCst) < 2 {
                Err("boom".to_string())
            } else {
                Ok(42)
            }
        })
        .await;
        assert_eq!(r, Some(42));
        assert_eq!(n.load(Ordering::SeqCst), 3);
        assert_eq!(st.get(), LoadState::Ready);
    }

    #[tokio::test]
    async fn exceeding_max_stays_degraded() {
        let st = LoadStatus::new();
        let r: Option<()> = retry_load(&cfg(3), &st, || async {
            Err("GET https://huggingface.co/m?token=abc failed".to_string())
        })
        .await;
        assert!(r.is_none());
        match st.get() {
            LoadState::Degraded {
                last_error,
                attempt,
                next_retry_at,
            } => {
                assert_eq!(attempt, 3);
                assert!(next_retry_at.is_none());
                assert!(last_error.contains("huggingface.co/m"));
                assert!(!last_error.contains("token=abc"));
            }
            s => panic!("unexpected {s:?}"),
        }
        assert_ne!(st.get(), LoadState::Ready);
    }

    #[test]
    fn mid_retry_has_next_retry_and_sanitizes_bearer() {
        let st = LoadStatus::new();
        st.set(LoadState::Degraded {
            last_error: sanitize("auth Bearer sk-secret rejected"),
            attempt: 1,
            next_retry_at: Some(now_unix() + 5),
        });
        let j = st.to_json();
        assert_eq!(j["state"], "degraded");
        assert!(!j.to_string().contains("sk-secret"));
        assert!(j["next_retry_at"].is_u64());
    }

    #[test]
    fn backoff_is_capped() {
        let c = RetryCfg {
            max_attempts: 8,
            base: Duration::from_secs(5),
            max: Duration::from_secs(300),
        };
        assert!(c.delay(1) >= Duration::from_secs(5));
        assert!(c.delay(30) <= Duration::from_secs(360));
    }
}
