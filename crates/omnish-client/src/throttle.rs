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
        for _ in 0..100 {
            assert!(t.should_send(1000));
            t.record_sent(1000);
        }
        assert_eq!(t.command_bytes, 100_000);
    }

    #[test]
    fn test_threshold_transition() {
        let mut t = OutputThrottle::new();
        let under = (DEFAULT_THRESHOLD_BYTES - 1000) as usize;
        assert!(t.should_send(under));
        t.record_sent(under);
        // This chunk crosses the threshold — bucket is 0, so large chunk rejected
        assert!(!t.should_send(4096));
    }

    #[test]
    fn test_throttled_rejects_without_elapsed_time() {
        let mut t = OutputThrottle::new();
        let over = DEFAULT_THRESHOLD_BYTES as usize;
        assert!(t.should_send(over));
        t.record_sent(over);
        assert!(!t.should_send(4096));
    }

    #[test]
    fn test_throttled_allows_after_time() {
        let mut t = OutputThrottle::new();
        let over = DEFAULT_THRESHOLD_BYTES as usize;
        assert!(t.should_send(over));
        t.record_sent(over);
        // Simulate 1 second passing
        t.last_refill = Instant::now() - std::time::Duration::from_secs(1);
        // After 1s at 10kB/s, bucket ~10240 bytes, 4096-byte chunk should pass
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
        // Bucket caps at 1 second (10240), so 10240-byte chunk succeeds
        assert!(t.should_send(10240));
        t.record_sent(10240);
        // Immediately after, bucket drained, next 10240 fails
        assert!(!t.should_send(10240));
    }

    #[test]
    fn test_reset_returns_to_normal() {
        let mut t = OutputThrottle::new();
        let over = DEFAULT_THRESHOLD_BYTES as usize;
        assert!(t.should_send(over));
        t.record_sent(over);
        assert!(!t.should_send(4096));
        t.reset();
        assert!(t.should_send(4096));
        t.record_sent(4096);
        assert_eq!(t.command_bytes, 4096);
    }
}
