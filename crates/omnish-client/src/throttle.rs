/// Hard stop: never send more than this many bytes for a single command.
/// Prevents programs like `dstat` from streaming unbounded output to the daemon.
const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024; // 4MB

/// Hard stop: never send more than this many IoData messages for a single command.
/// Prevents high-frequency small-update programs from flooding the daemon.
const DEFAULT_MAX_REQUESTS: u64 = 1_000;

pub struct OutputThrottle {
    max_bytes: u64,
    max_requests: u64,
    command_bytes: u64,
    command_requests: u64,
}

impl OutputThrottle {
    pub fn new() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_requests: DEFAULT_MAX_REQUESTS,
            command_bytes: 0,
            command_requests: 0,
        }
    }

    /// Returns true if this command's output is still under both caps.
    pub fn should_send(&self, _chunk_len: usize) -> bool {
        self.command_bytes < self.max_bytes && self.command_requests < self.max_requests
    }

    /// Record that `n` bytes were actually sent.
    /// Call this after a successful send.
    pub fn record_sent(&mut self, n: usize) {
        self.command_bytes += n as u64;
        self.command_requests += 1;
    }

    /// Reset for the next command.
    pub fn reset(&mut self) {
        self.command_bytes = 0;
        self.command_requests = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_under_cap_sends() {
        let mut t = OutputThrottle::new();
        assert!(t.should_send(1000));
        t.record_sent(1000);
        assert_eq!(t.command_bytes, 1000);
    }

    #[test]
    fn test_hard_cap_stops_sending() {
        let mut t = OutputThrottle::new();
        t.command_bytes = DEFAULT_MAX_BYTES;
        assert!(!t.should_send(1));
    }

    #[test]
    fn test_cap_boundary() {
        let mut t = OutputThrottle::new();
        t.command_bytes = DEFAULT_MAX_BYTES - 1;
        assert!(t.should_send(1));
        t.record_sent(1);
        assert!(!t.should_send(1));
    }

    #[test]
    fn test_requests_cap_stops_sending() {
        let mut t = OutputThrottle::new();
        t.command_requests = DEFAULT_MAX_REQUESTS;
        assert!(!t.should_send(1));
        t.reset();
        assert!(t.should_send(1));
    }

    #[test]
    fn test_requests_cap_boundary() {
        let mut t = OutputThrottle::new();
        t.command_requests = DEFAULT_MAX_REQUESTS - 1;
        assert!(t.should_send(1));
        t.record_sent(1);
        assert!(!t.should_send(1));
    }

    #[test]
    fn test_reset_returns_to_normal() {
        let mut t = OutputThrottle::new();
        t.command_bytes = DEFAULT_MAX_BYTES;
        assert!(!t.should_send(1));
        t.reset();
        assert!(t.should_send(1));
        assert_eq!(t.command_bytes, 0);
    }
}
