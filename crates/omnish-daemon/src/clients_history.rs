//! Persistent index of clients that have ever connected to this daemon.
//!
//! Entries survive the per-session 48h directory cleanup so the
//! `config -> general -> clients` deploy menu still shows hosts that
//! disconnected days ago. Keyed by `(deploy_addr, hostname)` to mirror
//! `SessionManager::list_clients`. Persisted as JSON at
//! `$omnish_dir/clients.json`.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientEntry {
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientsHistory {
    /// Key format: `"{deploy_addr}|{hostname}"`. Pipe is chosen because it
    /// cannot appear in either field (deploy addrs are `host` or `user@host`,
    /// hostnames are DNS-style).
    pub entries: HashMap<String, ClientEntry>,
}

impl ClientsHistory {
    /// Load from disk. Missing or unparseable file → empty history (logged).
    pub fn load(path: &Path) -> Self {
        let body = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!("clients_history: read {} failed: {}; starting empty", path.display(), e);
                return Self::default();
            }
        };
        match serde_json::from_str(&body) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("clients_history: parse {} failed: {}; starting empty", path.display(), e);
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    fn key(deploy_addr: &str, hostname: &str) -> String {
        format!("{}|{}", deploy_addr, hostname)
    }

    /// Upsert: bump `last_seen` to now, set `first_seen` only on insert.
    pub fn touch(&mut self, deploy_addr: &str, hostname: &str) {
        let now = Utc::now();
        self.entries
            .entry(Self::key(deploy_addr, hostname))
            .and_modify(|e| e.last_seen = now)
            .or_insert(ClientEntry { first_seen: now, last_seen: now });
    }

    /// Remove all entries with the given `deploy_addr` regardless of hostname.
    /// Returns the count removed. Forget-by-addr matches the menu UX where
    /// the path encodes only the addr; in the rare case of a single addr
    /// reachable from multiple hostnames the user gets an "all rows for
    /// that addr" delete in one click.
    pub fn forget_by_addr(&mut self, deploy_addr: &str) -> usize {
        let prefix = format!("{}|", deploy_addr);
        let before = self.entries.len();
        self.entries.retain(|k, _| !k.starts_with(&prefix));
        before - self.entries.len()
    }

    /// `(deploy_addr, hostname)` pairs sorted by addr then hostname.
    pub fn list(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .entries
            .keys()
            .filter_map(|k| k.split_once('|').map(|(a, h)| (a.to_string(), h.to_string())))
            .collect();
        out.sort();
        out
    }

    /// Drop entries whose `last_seen` is older than `max_age`. Returns the
    /// number removed.
    pub fn prune(&mut self, max_age: Duration) -> usize {
        let cutoff = Utc::now() - max_age;
        let before = self.entries.len();
        self.entries.retain(|_, e| e.last_seen >= cutoff);
        before - self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_inserts_and_updates() {
        let mut h = ClientsHistory::default();
        h.touch("alice@box1", "box1");
        assert_eq!(h.entries.len(), 1);
        let first_seen = h.entries.values().next().unwrap().first_seen;

        std::thread::sleep(std::time::Duration::from_millis(10));
        h.touch("alice@box1", "box1");
        assert_eq!(h.entries.len(), 1, "same key should not duplicate");
        let entry = h.entries.values().next().unwrap();
        assert_eq!(entry.first_seen, first_seen, "first_seen should be stable");
        assert!(entry.last_seen > first_seen, "last_seen should advance");
    }

    #[test]
    fn touch_distinguishes_addr_and_host() {
        let mut h = ClientsHistory::default();
        h.touch("alice@box1", "box1");
        h.touch("bob@box1", "box1");
        h.touch("alice@box1", "box2");
        assert_eq!(h.entries.len(), 3);
    }

    #[test]
    fn forget_by_addr_removes_all_hostnames() {
        let mut h = ClientsHistory::default();
        h.touch("alice@box1", "box1");
        h.touch("alice@box1", "box1.local");
        h.touch("bob@box1", "box1");

        let removed = h.forget_by_addr("alice@box1");
        assert_eq!(removed, 2);
        assert_eq!(h.entries.len(), 1);
        assert!(h.entries.contains_key("bob@box1|box1"));
    }

    #[test]
    fn list_is_sorted() {
        let mut h = ClientsHistory::default();
        h.touch("zoe@box", "box");
        h.touch("alice@box", "box");
        h.touch("alice@box", "alpha");
        let listed = h.list();
        assert_eq!(listed, vec![
            ("alice@box".to_string(), "alpha".to_string()),
            ("alice@box".to_string(), "box".to_string()),
            ("zoe@box".to_string(), "box".to_string()),
        ]);
    }

    #[test]
    fn prune_drops_stale_entries() {
        let mut h = ClientsHistory::default();
        let now = Utc::now();
        h.entries.insert("old|host".into(), ClientEntry {
            first_seen: now - Duration::days(120),
            last_seen: now - Duration::days(100),
        });
        h.entries.insert("fresh|host".into(), ClientEntry {
            first_seen: now - Duration::days(5),
            last_seen: now - Duration::days(1),
        });

        let removed = h.prune(Duration::days(90));
        assert_eq!(removed, 1);
        assert_eq!(h.entries.len(), 1);
        assert!(h.entries.contains_key("fresh|host"));
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clients.json");

        let mut h = ClientsHistory::default();
        h.touch("alice@box1", "box1");
        h.save(&path).unwrap();

        let loaded = ClientsHistory::load(&path);
        assert_eq!(loaded.entries.len(), 1);
        assert!(loaded.entries.contains_key("alice@box1|box1"));
    }

    #[test]
    fn load_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.json");
        let h = ClientsHistory::load(&path);
        assert!(h.entries.is_empty());
    }
}
