# Output Throttle Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Throttle per-command output IoData on the client side — full speed under 2MB, then 10kB/s — to reduce disk and network overhead for long-running commands.

**Architecture:** A new `OutputThrottle` struct in `omnish-client` implements a token-bucket rate limiter that activates after a per-command byte threshold. It integrates into the existing PTY output path in `main.rs` with a single `should_send()` guard. The throttle resets on each command boundary.

**Tech Stack:** Rust, `std::time::Instant`, token bucket algorithm

---

### Task 1: Create `OutputThrottle` with tests

**Files:**
- Create: `crates/omnish-client/src/throttle.rs`
- Modify: `crates/omnish-client/src/main.rs` (add `mod throttle;`)

**Step 1:** Add `mod throttle;` to `main.rs`, right after the existing module declarations:

```rust
mod throttle;
```

(Add after line 5: `mod probe;`)

**Step 2:** Create `crates/omnish-client/src/throttle.rs` with the full implementation and tests:

```rust
use std::time::Instant;

const DEFAULT_THRESHOLD_BYTES: u64 = 2 * 1024 * 1024; // 2MB
const DEFAULT_THROTTLE_RATE: f64 = 10_240.0; // 10kB/s

pub struct OutputThrottle {
    threshold_bytes: u64,
    throttle_rate: f64,
    command_bytes: u64,
    bucket: f64,
    last_refill: Instant,
}

impl OutputThrottle {
    pub fn new() -> Self {
        Self {
            threshold_bytes: DEFAULT_THRESHOLD_BYTES,
            throttle_rate: DEFAULT_THROTTLE_RATE,
            command_bytes: 0,
            bucket: 0.0,
            last_refill: Instant::now(),
        }
    }

    /// Check whether a chunk of `chunk_len` bytes should be sent.
    /// Under the threshold: always true.
    /// Over the threshold: uses token bucket at `throttle_rate` bytes/sec.
    pub fn should_send(&mut self, chunk_len: usize) -> bool {
        let len = chunk_len as u64;

        // Normal phase: under threshold
        if self.command_bytes + len <= self.threshold_bytes {
            return true;
        }

        // Throttled phase: refill token bucket
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.bucket += elapsed * self.throttle_rate;
        // Cap burst to 1 second worth
        if self.bucket > self.throttle_rate {
            self.bucket = self.throttle_rate;
        }

        if self.bucket >= chunk_len as f64 {
            self.bucket -= chunk_len as f64;
            true
        } else {
            false
        }
    }

    /// Record that `n` bytes were actually sent.
    /// Call this after a successful send.
    pub fn record_sent(&mut self, n: usize) {
        self.command_bytes += n as u64;
    }

    /// Reset for the next command.
    pub fn reset(&mut self) {
        self.command_bytes = 0;
        self.bucket = 0.0;
        self.last_refill = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_under_threshold_always_sends() {
        let mut t = OutputThrottle::new();
        // Small chunks well under 2MB
        for _ in 0..100 {
            assert!(t.should_send(1000));
            t.record_sent(1000);
        }
        // 100KB total, still under 2MB
        assert_eq!(t.command_bytes, 100_000);
    }

    #[test]
    fn test_threshold_transition() {
        let mut t = OutputThrottle::new();
        // Send just under the threshold
        let under = (DEFAULT_THRESHOLD_BYTES - 1000) as usize;
        assert!(t.should_send(under));
        t.record_sent(under);

        // This chunk crosses the threshold — should_send transitions to throttled.
        // With no elapsed time, bucket is 0, so a large chunk is rejected.
        assert!(!t.should_send(4096));
    }

    #[test]
    fn test_throttled_rejects_without_elapsed_time() {
        let mut t = OutputThrottle::new();
        // Push past threshold
        let over = DEFAULT_THRESHOLD_BYTES as usize;
        assert!(t.should_send(over));
        t.record_sent(over);

        // Immediately: no time elapsed, bucket ~0, should reject
        assert!(!t.should_send(4096));
    }

    #[test]
    fn test_throttled_allows_after_time() {
        let mut t = OutputThrottle::new();
        // Push past threshold
        let over = DEFAULT_THRESHOLD_BYTES as usize;
        assert!(t.should_send(over));
        t.record_sent(over);

        // Simulate 1 second passing by backdating last_refill
        t.last_refill = Instant::now() - std::time::Duration::from_secs(1);

        // After 1s at 10kB/s, bucket should have ~10240 bytes
        // A 4096-byte chunk should be allowed
        assert!(t.should_send(4096));
    }

    #[test]
    fn test_bucket_caps_at_one_second_burst() {
        let mut t = OutputThrottle::new();
        let over = DEFAULT_THRESHOLD_BYTES as usize;
        assert!(t.should_send(over));
        t.record_sent(over);

        // Simulate 10 seconds passing
        t.last_refill = Instant::now() - std::time::Duration::from_secs(10);

        // Bucket caps at throttle_rate (1 second worth = 10240)
        // So a 10240-byte chunk should succeed
        assert!(t.should_send(10240));
        t.record_sent(10240);

        // But immediately after, a second 10240-byte chunk should fail (bucket drained)
        assert!(!t.should_send(10240));
    }

    #[test]
    fn test_reset_returns_to_normal() {
        let mut t = OutputThrottle::new();
        // Push past threshold
        let over = DEFAULT_THRESHOLD_BYTES as usize;
        assert!(t.should_send(over));
        t.record_sent(over);
        assert!(!t.should_send(4096));

        // Reset
        t.reset();

        // Back to normal phase
        assert!(t.should_send(4096));
        t.record_sent(4096);
        assert_eq!(t.command_bytes, 4096);
    }
}
```

**Step 3:** Verify: `cargo test -p omnish-client`

**Step 4:** Commit: `feat(client): add OutputThrottle with token bucket rate limiting`

---

### Task 2: Integrate throttle into the output path

**File:** `crates/omnish-client/src/main.rs`

**Step 1:** Add throttle creation in `main()`, after the `command_tracker` initialization (around line 98):

```rust
    let mut throttle = throttle::OutputThrottle::new();
```

**Step 2:** Guard the output IoData send (around line 250-258). Replace:

```rust
                    // Send IoData to daemon first (so stream is written before CommandComplete)
                    if let Some(ref rpc) = daemon_conn {
                        let msg = Message::IoData(IoData {
                            session_id: session_id.clone(),
                            direction: IoDirection::Output,
                            timestamp_ms: timestamp_ms(),
                            data: output_buf[..n].to_vec(),
                        });
                        send_or_buffer(rpc, msg, &pending_buffer).await;
                    }
```

With:

```rust
                    // Send IoData to daemon (throttled for long-running commands)
                    if let Some(ref rpc) = daemon_conn {
                        if throttle.should_send(n) {
                            let msg = Message::IoData(IoData {
                                session_id: session_id.clone(),
                                direction: IoDirection::Output,
                                timestamp_ms: timestamp_ms(),
                                data: output_buf[..n].to_vec(),
                            });
                            send_or_buffer(rpc, msg, &pending_buffer).await;
                            throttle.record_sent(n);
                        }
                    }
```

**Step 3:** Add throttle reset after command completion (around line 261-270). After the `for record in &completed` loop, add:

```rust
                    if !completed.is_empty() {
                        throttle.reset();
                    }
```

**Step 4:** Verify: `cargo build -p omnish-client && cargo test -p omnish-client`

**Step 5:** Commit: `feat(client): integrate output throttle in PTY output path`

---

### Task 3: Full workspace verification

Run: `cargo test --workspace`
