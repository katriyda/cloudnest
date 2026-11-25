use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Failure budget window for an authentication style check.
#[derive(Debug, Clone, Copy)]
pub struct FailureLimit {
    pub max_failures: u32,
    pub window_secs: i64,
    pub block_secs: i64,
}

impl FailureLimit {
    pub const fn new(max_failures: u32, window_secs: i64, block_secs: i64) -> Self {
        Self {
            max_failures,
            window_secs,
            block_secs,
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    failures: u32,
    window_start: i64,
    blocked_until: i64,
}

/// Process-local failure tracker used to slow down credential guessing.
/// Keys are opaque strings such as `login-user:<name>` or `unlock-token:<token>`.
#[derive(Clone, Default)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Seconds left before the key may be tried again, if it is currently blocked.
    pub fn blocked_seconds(&self, key: &str) -> Option<i64> {
        let now = crate::util::now_ts();
        let entry = self.entries().get(key).cloned()?;
        (entry.blocked_until > now).then(|| entry.blocked_until - now)
    }

    /// Record one failed attempt. Returns the number of seconds the key is
    /// blocked for once the budget is exhausted.
    pub fn record_failure(&self, key: &str, limit: FailureLimit) -> Option<i64> {
        let now = crate::util::now_ts();
        let mut map = self.entries();
        if map.len() > 20_000 {
            map.retain(|_, entry| {
                entry.blocked_until > now || entry.window_start + limit.window_secs > now
            });
        }
        let entry = map.entry(key.to_string()).or_insert(Entry {
            failures: 0,
            window_start: now,
            blocked_until: 0,
        });
        if entry.blocked_until > now {
            return Some(entry.blocked_until - now);
        }
        if now.saturating_sub(entry.window_start) > limit.window_secs {
            entry.failures = 0;
            entry.window_start = now;
        }
        entry.failures = entry.failures.saturating_add(1);
        if entry.failures >= limit.max_failures {
            entry.failures = 0;
            entry.window_start = now;
            entry.blocked_until = now.saturating_add(limit.block_secs);
            return Some(limit.block_secs);
        }
        None
    }

    /// Clear the failure budget after a successful check.
    pub fn reset(&self, key: &str) {
        let _ = self.entries().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_budget() {
        let limiter = RateLimiter::new();
        let limit = FailureLimit::new(3, 60, 120);
        assert_eq!(limiter.record_failure("k", limit), None);
        assert_eq!(limiter.record_failure("k", limit), None);
        assert_eq!(limiter.record_failure("k", limit), Some(120));
        assert!(limiter.blocked_seconds("k").is_some_and(|secs| secs > 0));
        limiter.reset("k");
        assert!(limiter.blocked_seconds("k").is_none());
    }

    #[test]
    fn keys_are_independent() {
        let limiter = RateLimiter::new();
        let limit = FailureLimit::new(1, 60, 60);
        assert!(limiter.record_failure("a", limit).is_some());
        assert!(limiter.blocked_seconds("a").is_some());
        assert!(limiter.blocked_seconds("b").is_none());
    }
}
