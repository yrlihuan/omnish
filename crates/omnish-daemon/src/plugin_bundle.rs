//! Issue #588: daemon-side cache of the plugin bundle produced by
//! `omnish_common::plugin_bundle`.
//!
//! Two layers of locking:
//!   - `state: RwLock<Bundle>` gives readers (`snapshot`, `checksum`) an
//!     atomic view of the `(bytes, checksum)` pair.
//!   - `rebuild_lock: tokio::sync::Mutex<()>` serializes concurrent
//!     `rebuild` calls so N clients polling simultaneously (and the
//!     scheduled `PluginBundleTask`) never trigger N concurrent tars;
//!     at most one rebuild runs, the rest wait on the mutex and then
//!     read the fresh cache.
//!
//! `PluginBundleTask` refreshes the cache every 5 minutes as a baseline.
//! Server handlers refresh it on demand when a client's checksum
//! disagrees with the cache (the cache can be up to 5 minutes stale, so
//! if a user edited `plugins/` on the daemon host right after the last
//! scheduled rebuild, the check handler catches it before responding
//! with a stale announcement).

use omnish_common::plugin_bundle::{build_bundle, Bundle};
use std::path::PathBuf;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub use omnish_common::plugin_bundle::Bundle as SharedBundle;

/// How recent a prior rebuild must be to let a waiting caller skip its
/// own rebuild and reuse the fresh cache. 500 ms comfortably coalesces a
/// burst of concurrent client polls without collapsing a legitimate
/// rebuild that follows a scheduled one 5 min later.
const REBUILD_COALESCE_MS: u64 = 500;

pub struct PluginBundler {
    plugins_dir: PathBuf,
    state: RwLock<Bundle>,
    rebuild_lock: tokio::sync::Mutex<()>,
    last_rebuild_ms: AtomicU64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl PluginBundler {
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self {
            plugins_dir,
            state: RwLock::new(Bundle::default()),
            rebuild_lock: tokio::sync::Mutex::new(()),
            last_rebuild_ms: AtomicU64::new(0),
        }
    }

    /// Rebuild the cache from `plugins_dir`. Serialized via `rebuild_lock`
    /// so concurrent callers (scheduled task + client-triggered refresh)
    /// never duplicate the tar+gzip work: at most one rebuild runs at a
    /// time. A second caller that enters the critical section within
    /// `REBUILD_COALESCE_MS` of the previous rebuild short-circuits and
    /// returns the fresh cache - in that window the on-disk state is
    /// effectively the same state the first caller just packaged.
    ///
    /// The tar+gzip itself runs on a blocking thread so the tokio
    /// executor isn't stalled. Returns the fresh checksum on success;
    /// on failure the previous snapshot is preserved and its checksum
    /// is returned.
    pub async fn rebuild(&self) -> String {
        let _guard = self.rebuild_lock.lock().await;
        let last = self.last_rebuild_ms.load(Ordering::Relaxed);
        let now = now_ms();
        if last > 0 && now.saturating_sub(last) < REBUILD_COALESCE_MS {
            // Someone else just did this; reuse their result.
            return self.state.read().unwrap().checksum.clone();
        }
        let plugins_dir = self.plugins_dir.clone();
        let result = tokio::task::spawn_blocking(move || build_bundle(&plugins_dir)).await;
        match result {
            Ok(Ok(bundle)) => {
                let checksum = bundle.checksum.clone();
                *self.state.write().unwrap() = bundle;
                self.last_rebuild_ms.store(now_ms(), Ordering::Relaxed);
                checksum
            }
            Ok(Err(e)) => {
                tracing::warn!("plugin_bundle: rebuild failed: {}", e);
                self.state.read().unwrap().checksum.clone()
            }
            Err(e) => {
                tracing::warn!("plugin_bundle: rebuild join error: {}", e);
                self.state.read().unwrap().checksum.clone()
            }
        }
    }

    /// Cloned snapshot for streaming to a client.
    pub fn snapshot(&self) -> Bundle {
        self.state.read().unwrap().clone()
    }

    /// Current checksum without cloning the bytes. Fast-path for
    /// `PluginSyncCheck` handling.
    pub fn checksum(&self) -> String {
        self.state.read().unwrap().checksum.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(path: &std::path::Path, content: &[u8]) {
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).unwrap(); }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content).unwrap();
    }

    #[tokio::test]
    async fn rebuild_and_snapshot_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().to_path_buf();
        write_file(&plugins.join("a/tool.json"), b"{\"x\":1}");

        let bundler = PluginBundler::new(plugins);
        let c = bundler.rebuild().await;
        assert!(!c.is_empty());
        let snap = bundler.snapshot();
        assert_eq!(snap.checksum, c);
        assert!(!snap.bytes.is_empty());
    }

    #[tokio::test]
    async fn missing_dir_rebuild_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let bundler = PluginBundler::new(dir.path().join("does_not_exist"));
        assert_eq!(bundler.rebuild().await, "");
        assert!(bundler.snapshot().bytes.is_empty());
    }

    #[tokio::test]
    async fn rebuild_refreshes_after_content_change() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().to_path_buf();
        write_file(&plugins.join("a/tool.json"), b"v1");

        let bundler = PluginBundler::new(plugins.clone());
        let c1 = bundler.rebuild().await;
        assert!(!c1.is_empty());

        // Wait past the coalescing window so the next rebuild really runs.
        tokio::time::sleep(std::time::Duration::from_millis(REBUILD_COALESCE_MS + 50)).await;

        // Modify after cache is warm.
        write_file(&plugins.join("a/tool.json"), b"v2");
        let c2 = bundler.rebuild().await;
        assert_ne!(c1, c2, "rebuild must pick up post-cache changes");
        assert_eq!(bundler.checksum(), c2);
    }

    /// Issue #588: back-to-back rebuild calls within the coalescing window
    /// must not run a second tar. The second caller observes the result
    /// of the first even though the underlying `plugins/` changed - this
    /// is the intentional dedup behaviour for bursty client polls.
    #[tokio::test]
    async fn rebuild_coalesces_within_window() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().to_path_buf();
        write_file(&plugins.join("a/tool.json"), b"v1");

        let bundler = PluginBundler::new(plugins.clone());
        let c1 = bundler.rebuild().await;
        assert!(!c1.is_empty());

        // Modify, then call rebuild *without* sleeping past the window:
        // the change is real but the coalescer trusts the recent result.
        write_file(&plugins.join("a/tool.json"), b"v2");
        let c2 = bundler.rebuild().await;
        assert_eq!(c1, c2, "coalesced rebuild must reuse previous result");
    }
}
