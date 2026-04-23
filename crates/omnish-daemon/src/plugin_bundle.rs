//! Issue #588: daemon-side cache of the plugin bundle produced by
//! `omnish_common::plugin_bundle`. `PluginBundler` holds the most recent
//! `(bytes, checksum)` under `RwLock` so `PluginBundleTask` can refresh
//! it periodically without coordinating with the server handlers that
//! stream the bundle to clients.

use omnish_common::plugin_bundle::{build_bundle, Bundle};
use std::path::PathBuf;
use std::sync::RwLock;

pub use omnish_common::plugin_bundle::Bundle as SharedBundle;

pub struct PluginBundler {
    plugins_dir: PathBuf,
    state: RwLock<Bundle>,
}

impl PluginBundler {
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self {
            plugins_dir,
            state: RwLock::new(Bundle::default()),
        }
    }

    /// Rebuild the in-memory bundle by scanning `plugins_dir`. Returns the
    /// new checksum. On error the previous snapshot is preserved and a
    /// warning is logged.
    pub fn rebuild(&self) -> String {
        match build_bundle(&self.plugins_dir) {
            Ok(bundle) => {
                let checksum = bundle.checksum.clone();
                *self.state.write().unwrap() = bundle;
                checksum
            }
            Err(e) => {
                tracing::warn!("plugin_bundle: rebuild failed: {}", e);
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

    #[test]
    fn rebuild_and_snapshot_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().to_path_buf();
        write_file(&plugins.join("a/tool.json"), b"{\"x\":1}");

        let bundler = PluginBundler::new(plugins);
        let c = bundler.rebuild();
        assert!(!c.is_empty());
        let snap = bundler.snapshot();
        assert_eq!(snap.checksum, c);
        assert!(!snap.bytes.is_empty());
    }

    #[test]
    fn missing_dir_rebuild_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let bundler = PluginBundler::new(dir.path().join("does_not_exist"));
        assert_eq!(bundler.rebuild(), "");
        assert!(bundler.snapshot().bytes.is_empty());
    }
}
