//! Pre-authentication guard for credential validation.
//!
//! Every tool call re-validates its credentials against BookStack, so a caller
//! with invented credentials could burn the shared BookStack request budget
//! without ever being authenticated. The guard remembers recent verdicts so
//! repeated bad credentials are answered locally, and caps how many *distinct*
//! unknown credentials may be tried per window. Credentials that validated
//! recently bypass the cap, so an attack does not lock legitimate users out.
//! Validation itself is never skipped: a revoked token still fails at once.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

#[derive(Debug, PartialEq, Eq)]
pub enum Gate {
    /// Ask BookStack.
    Allow,
    /// This exact credential was rejected moments ago; answer 401 locally.
    KnownBad,
    /// Too many distinct unknown credentials failed recently; answer 429.
    Throttled,
}

struct Inner {
    good: HashMap<String, Instant>,
    bad: HashMap<String, Instant>,
    rejections: VecDeque<Instant>,
    pending: HashMap<String, Instant>,
}

pub struct AuthGuard {
    inner: Mutex<Inner>,
    max_rejections: usize,
    window: Duration,
    bad_ttl: Duration,
    good_ttl: Duration,
    max_entries: usize,
}

/// Hash of the whole credential, so the guard never stores a raw secret and a
/// wrong secret never shares a verdict with the right one.
pub fn credential_key(instance: &str, token_id: &str, token_secret: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [instance, token_id, token_secret] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

impl AuthGuard {
    pub fn new() -> Self {
        Self::with_limits(
            30,
            Duration::from_secs(60),
            Duration::from_secs(30),
            Duration::from_secs(600),
            10_000,
        )
    }

    pub fn with_limits(
        max_rejections: usize,
        window: Duration,
        bad_ttl: Duration,
        good_ttl: Duration,
        max_entries: usize,
    ) -> Self {
        Self {
            inner: Mutex::new(Inner {
                good: HashMap::new(),
                bad: HashMap::new(),
                rejections: VecDeque::new(),
                pending: HashMap::new(),
            }),
            max_rejections,
            window,
            bad_ttl,
            good_ttl,
            max_entries,
        }
    }

    pub fn admit(&self, key: &str) -> Gate {
        self.admit_at(key, Instant::now())
    }

    pub fn record(&self, key: &str, valid: bool) {
        self.record_at(key, valid, Instant::now());
    }

    pub fn admit_at(&self, key: &str, now: Instant) -> Gate {
        let mut inner = self.inner.lock().unwrap();
        self.prune(&mut inner, now);
        if inner.good.contains_key(key) {
            return Gate::Allow;
        }
        if inner.bad.contains_key(key) {
            return Gate::KnownBad;
        }
        if inner.pending.contains_key(key)
            || inner.rejections.len() + inner.pending.len() >= self.max_rejections
        {
            return Gate::Throttled;
        }
        inner.pending.insert(key.to_string(), now);
        Gate::Allow
    }

    pub fn record_at(&self, key: &str, valid: bool, now: Instant) {
        let mut inner = self.inner.lock().unwrap();
        self.prune(&mut inner, now);
        inner.pending.remove(key);
        if valid {
            inner.bad.remove(key);
            if inner.good.len() < self.max_entries || inner.good.contains_key(key) {
                inner.good.insert(key.to_string(), now);
            }
        } else {
            inner.good.remove(key);
            inner.rejections.push_back(now);
            if inner.bad.len() < self.max_entries || inner.bad.contains_key(key) {
                inner.bad.insert(key.to_string(), now);
            }
        }
    }

    fn prune(&self, inner: &mut Inner, now: Instant) {
        inner
            .pending
            .retain(|_, t| now.duration_since(*t) < self.window);
        while inner
            .rejections
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.window)
        {
            inner.rejections.pop_front();
        }
        let (bad_ttl, good_ttl) = (self.bad_ttl, self.good_ttl);
        inner.bad.retain(|_, t| now.duration_since(*t) < bad_ttl);
        inner.good.retain(|_, t| now.duration_since(*t) < good_ttl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> AuthGuard {
        AuthGuard::with_limits(
            3,
            Duration::from_secs(60),
            Duration::from_secs(30),
            Duration::from_secs(600),
            1000,
        )
    }

    #[test]
    fn a_rejected_credential_is_answered_locally_until_it_expires() {
        let g = guard();
        let t0 = Instant::now();
        assert_eq!(g.admit_at("bad", t0), Gate::Allow);
        g.record_at("bad", false, t0);
        assert_eq!(
            g.admit_at("bad", t0 + Duration::from_secs(5)),
            Gate::KnownBad
        );
        assert_eq!(
            g.admit_at("bad", t0 + Duration::from_secs(31)),
            Gate::Allow,
            "the verdict must expire so a fixed account recovers"
        );
    }

    #[test]
    fn concurrent_unknown_credentials_reserve_slots_before_validation() {
        let g = guard();
        let t0 = Instant::now();
        for i in 0..3 {
            assert_eq!(g.admit_at(&format!("pending-{i}"), t0), Gate::Allow);
        }
        assert_eq!(g.admit_at("overflow", t0), Gate::Throttled);
        g.record_at("pending-0", true, t0);
        assert_eq!(g.admit_at("after-success", t0), Gate::Allow);
    }

    #[test]
    fn repeated_unknown_credential_cannot_validate_concurrently() {
        let g = guard();
        let t0 = Instant::now();
        assert_eq!(g.admit_at("same", t0), Gate::Allow);
        assert_eq!(g.admit_at("same", t0), Gate::Throttled);
        g.record_at("same", false, t0);
        assert_eq!(g.admit_at("same", t0), Gate::KnownBad);
    }

    #[test]
    fn distinct_unknown_credentials_are_capped_per_window() {
        let g = guard();
        let t0 = Instant::now();
        for i in 0..3 {
            let key = format!("random-{i}");
            assert_eq!(g.admit_at(&key, t0), Gate::Allow);
            g.record_at(&key, false, t0);
        }
        assert_eq!(g.admit_at("random-3", t0), Gate::Throttled);
        assert_eq!(
            g.admit_at("random-3", t0 + Duration::from_secs(61)),
            Gate::Allow,
            "the cap is per window, not permanent"
        );
    }

    #[test]
    fn recently_valid_credentials_are_never_locked_out_by_an_attack() {
        let g = guard();
        let t0 = Instant::now();
        g.record_at("customer", true, t0);
        for i in 0..5 {
            g.record_at(&format!("random-{i}"), false, t0);
        }
        assert_eq!(g.admit_at("random-new", t0), Gate::Throttled);
        assert_eq!(g.admit_at("customer", t0), Gate::Allow);
    }

    #[test]
    fn a_credential_that_turns_invalid_loses_its_bypass() {
        let g = guard();
        let t0 = Instant::now();
        g.record_at("revoked", true, t0);
        g.record_at("revoked", false, t0);
        assert_eq!(g.admit_at("revoked", t0), Gate::KnownBad);
    }

    #[test]
    fn memory_is_bounded_under_a_flood_of_distinct_credentials() {
        let g = AuthGuard::with_limits(
            1_000_000,
            Duration::from_secs(60),
            Duration::from_secs(30),
            Duration::from_secs(600),
            50,
        );
        let t0 = Instant::now();
        for i in 0..500 {
            g.record_at(&format!("flood-{i}"), false, t0);
        }
        let inner = g.inner.lock().unwrap();
        assert!(
            inner.bad.len() <= 50,
            "bad cache grew to {}",
            inner.bad.len()
        );
    }

    #[test]
    fn the_key_depends_on_the_secret_and_the_instance() {
        assert_ne!(
            credential_key("i", "id", "right"),
            credential_key("i", "id", "wrong")
        );
        assert_ne!(
            credential_key("i1", "id", "s"),
            credential_key("i2", "id", "s")
        );
        assert_eq!(
            credential_key("i", "id", "s"),
            credential_key("i", "id", "s")
        );
    }
}
