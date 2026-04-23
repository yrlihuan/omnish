//! TTL-based expectation registry for daemon-side `NoticePush` messages
//! tagged with a `kind`. Initiators of an action (e.g. Install plugin)
//! register an expectation before sending the RPC; the main loop consumes
//! the matching tagged notice and drops untagged or unexpected ones on
//! peer clients.
//!
//! Semantics:
//! - `NoticePush { kind: None }` is never filtered here - callers display
//!   it directly (legacy deploy broadcasts).
//! - `NoticePush { kind: Some(k) }` is shown only when some client code
//!   path called `expect(k, ttl)` and that entry is still live.
//! - An expectation is consumed on the first matching notice (success or
//!   failure); the next install needs a fresh `expect`.
//! - Expired expectations are lazily evicted on access.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

struct Registry {
    expectations: HashMap<String, Instant>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| {
    Mutex::new(Registry { expectations: HashMap::new() })
});

/// Record that this client is expecting a tagged notice of `kind` within
/// `ttl`. Overwrites any earlier deadline for the same kind - the caller
/// is presumed to have just re-initiated the action.
pub fn expect(kind: &str, ttl: Duration) {
    let mut r = REGISTRY.lock().unwrap();
    r.expectations.insert(kind.to_string(), Instant::now() + ttl);
}

/// Consume an expectation matching `kind`. Returns `true` if an unexpired
/// entry existed (caller should display the notice); `false` otherwise
/// (caller should drop it). Expired entries are evicted as a side effect.
pub fn consume(kind: &str) -> bool {
    let mut r = REGISTRY.lock().unwrap();
    let now = Instant::now();
    match r.expectations.remove(kind) {
        Some(deadline) if now <= deadline => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expect_then_consume_returns_true() {
        // Use a unique kind per test to avoid cross-test interference via
        // the global registry.
        let kind = "test_kind_basic";
        expect(kind, Duration::from_secs(10));
        assert!(consume(kind));
    }

    #[test]
    fn consume_without_expect_returns_false() {
        let kind = "test_kind_no_expect";
        assert!(!consume(kind));
    }

    #[test]
    fn consume_twice_returns_false_on_second_call() {
        let kind = "test_kind_twice";
        expect(kind, Duration::from_secs(10));
        assert!(consume(kind));
        assert!(!consume(kind));
    }

    #[test]
    fn expired_expectation_is_not_consumed() {
        let kind = "test_kind_expired";
        expect(kind, Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(!consume(kind));
    }
}
