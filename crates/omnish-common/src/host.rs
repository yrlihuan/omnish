//! Hostname comparison shared by the client and the daemon.
//!
//! macOS `gethostname()` reports either `host` or `host.local` depending on
//! whether mDNS / `scutil HostName` is in play, and it can flip across a
//! network drop or reconnect. Comparing raw strings therefore treats one
//! machine as two, which is why every host comparison must go through here.

/// Strip a trailing `.local` (any case) so both forms of a macOS hostname
/// compare equal.
pub fn normalize_host(h: &str) -> &str {
    let t = h.trim();
    let bytes = t.as_bytes();
    const SUFFIX: &str = ".local";
    if bytes.len() > SUFFIX.len() {
        let split = t.len() - SUFFIX.len();
        if t.is_char_boundary(split) && t[split..].eq_ignore_ascii_case(SUFFIX) {
            return &t[..split];
        }
    }
    t
}

/// True when two hostnames refer to the same machine, ignoring a `.local`
/// suffix and ASCII case.
pub fn hosts_match(a: &str, b: &str) -> bool {
    normalize_host(a).eq_ignore_ascii_case(normalize_host(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_host() {
        assert_eq!(normalize_host("titan"), "titan");
        assert_eq!(normalize_host("MBProM4HF-2.local"), "MBProM4HF-2");
        assert_eq!(normalize_host("MBProM4HF-2.LOCAL"), "MBProM4HF-2");
        assert_eq!(normalize_host("  titan  "), "titan");
        // A bare ".local" is a hostname, not a suffix to strip.
        assert_eq!(normalize_host(".local"), ".local");
        assert_eq!(normalize_host(""), "");
    }

    #[test]
    fn test_hosts_match() {
        // The macOS flip that hid every conversation from /resume (issue #683).
        assert!(hosts_match("MBProM4HF-2", "MBProM4HF-2.local"));
        assert!(hosts_match("MBProM4HF-2.local", "MBProM4HF-2"));
        assert!(hosts_match("titan", "TITAN"));
        assert!(hosts_match("titan", "titan"));
        assert!(!hosts_match("titan", "falcon"));
        assert!(!hosts_match("nexus", "nexusts"));
        assert!(!hosts_match("", "titan"));
    }
}
