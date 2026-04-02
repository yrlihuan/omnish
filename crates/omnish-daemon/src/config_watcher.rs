use crate::file_watcher::FileWatcher;
use omnish_common::config::DaemonConfig;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::watch;

#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug)]
pub enum ConfigSection {
    Tools,
    Sandbox,
    Context,
    Llm,
    Tasks,
    Plugins,
}

pub struct ConfigWatcher {
    config_path: PathBuf,
    current: RwLock<DaemonConfig>,
    senders: HashMap<ConfigSection, watch::Sender<Arc<DaemonConfig>>>,
}

impl ConfigSection {
    /// Map a daemon.toml top-level key to its ConfigSection.
    /// Returns None for keys that don't map (e.g. listen_addr).
    #[cfg(test)]
    pub fn from_toml_key(key: &str) -> Option<ConfigSection> {
        match key {
            "llm" => Some(ConfigSection::Llm),
            "context" => Some(ConfigSection::Context),
            "tasks" => Some(ConfigSection::Tasks),
            "plugins" => Some(ConfigSection::Plugins),
            "tools" => Some(ConfigSection::Tools),
            "sandbox" => Some(ConfigSection::Sandbox),
            "proxy" => Some(ConfigSection::Llm), // proxy affects LLM backends
            _ => None,
        }
    }
}

impl ConfigWatcher {
    /// Sections that have diff + notify implemented in reload().
    /// The compile-time guard test in config_schema.rs asserts that all schema
    /// paths map to a section in this list.
    pub const WATCHED_SECTIONS: &[ConfigSection] = &[
        ConfigSection::Sandbox,
        ConfigSection::Llm,
        ConfigSection::Plugins,
        ConfigSection::Tasks,
        // Add sections here as their diff + subscriber is implemented:
        // ConfigSection::Context,
        // ConfigSection::Tools,
    ];

    /// Create a new ConfigWatcher. Registers a file watch on config_path
    /// via the shared FileWatcher and spawns a reload task.
    pub fn new(
        config_path: PathBuf,
        initial: DaemonConfig,
        file_watcher: &FileWatcher,
    ) -> Arc<Self> {
        let file_rx = file_watcher.watch(config_path.clone());
        let initial_arc = Arc::new(initial.clone());

        let mut senders = HashMap::new();
        for section in [
            ConfigSection::Tools,
            ConfigSection::Sandbox,
            ConfigSection::Context,
            ConfigSection::Llm,
            ConfigSection::Tasks,
            ConfigSection::Plugins,
        ] {
            let (tx, _) = watch::channel(Arc::clone(&initial_arc));
            senders.insert(section, tx);
        }

        let watcher = Arc::new(Self {
            config_path,
            current: RwLock::new(initial),
            senders,
        });

        // Spawn reload task
        let cw = Arc::clone(&watcher);
        tokio::spawn(async move {
            let mut rx = file_rx;
            while rx.changed().await.is_ok() {
                if let Err(e) = cw.reload() {
                    tracing::warn!("config reload failed: {}", e);
                }
            }
        });

        watcher
    }

    /// Subscribe to changes in a specific config section.
    pub fn subscribe(&self, section: ConfigSection) -> watch::Receiver<Arc<DaemonConfig>> {
        self.senders[&section].subscribe()
    }

    /// Re-read daemon.toml, diff sections, notify changed ones.
    /// File I/O and TOML parsing happen before acquiring the write lock.
    pub fn reload(&self) -> anyhow::Result<()> {
        // Read and parse outside the lock
        let content = std::fs::read_to_string(&self.config_path)?;
        let new_config: DaemonConfig = toml::from_str(&content)?;

        // Lock briefly for diff + swap
        let mut current = self.current.write().unwrap();
        let new_arc = Arc::new(new_config.clone());

        // Diff each section and notify if changed
        for section in Self::WATCHED_SECTIONS {
            let changed = match section {
                ConfigSection::Sandbox => current.sandbox != new_config.sandbox,
                ConfigSection::Llm => current.llm != new_config.llm
                    || current.proxy != new_config.proxy,
                ConfigSection::Plugins => current.plugins != new_config.plugins,
                ConfigSection::Tasks => current.tasks != new_config.tasks,
                // Future: add diff for other sections here
                _ => false,
            };
            if changed {
                if let Some(tx) = self.senders.get(section) {
                    let _ = tx.send(Arc::clone(&new_arc));
                    tracing::info!("config section changed: {:?}", section);
                }
            }
        }

        *current = new_config;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_section_hash_eq() {
        let mut map: HashMap<ConfigSection, i32> = HashMap::new();
        map.insert(ConfigSection::Sandbox, 1);
        map.insert(ConfigSection::Tools, 2);
        assert_eq!(map[&ConfigSection::Sandbox], 1);
        assert_eq!(map[&ConfigSection::Tools], 2);
    }

    #[test]
    fn test_reload_detects_sandbox_change() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("daemon.toml");

        // Write initial config
        std::fs::write(&config_path, "").unwrap();
        let initial = DaemonConfig::default();

        // Can't use ConfigWatcher::new (needs tokio runtime), test reload directly
        let initial_arc = Arc::new(initial.clone());
        let mut senders = HashMap::new();
        let (tx, rx) = watch::channel(Arc::clone(&initial_arc));
        senders.insert(ConfigSection::Sandbox, tx);
        for section in [
            ConfigSection::Tools,
            ConfigSection::Context,
            ConfigSection::Llm,
            ConfigSection::Tasks,
            ConfigSection::Plugins,
        ] {
            let (tx, _) = watch::channel(Arc::clone(&initial_arc));
            senders.insert(section, tx);
        }

        let cw = ConfigWatcher {
            config_path: config_path.clone(),
            current: RwLock::new(initial),
            senders,
        };

        // Write config with sandbox rules
        std::fs::write(&config_path, r#"
[sandbox.plugins.bash]
permit_rules = ["command starts_with glab"]
"#).unwrap();

        cw.reload().unwrap();

        // Receiver should have been notified
        assert!(rx.has_changed().unwrap());
        let config = rx.borrow();
        assert_eq!(config.sandbox.plugins["bash"].permit_rules.len(), 1);
    }

    #[test]
    fn test_reload_no_change_no_notify() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("daemon.toml");
        std::fs::write(&config_path, "").unwrap();
        let initial = DaemonConfig::default();

        let initial_arc = Arc::new(initial.clone());
        let mut senders = HashMap::new();
        let (tx, mut rx) = watch::channel(Arc::clone(&initial_arc));
        senders.insert(ConfigSection::Sandbox, tx);
        for section in [
            ConfigSection::Tools,
            ConfigSection::Context,
            ConfigSection::Llm,
            ConfigSection::Tasks,
            ConfigSection::Plugins,
        ] {
            let (tx, _) = watch::channel(Arc::clone(&initial_arc));
            senders.insert(section, tx);
        }

        let cw = ConfigWatcher {
            config_path: config_path.clone(),
            current: RwLock::new(initial),
            senders,
        };

        // Mark current value as seen
        rx.borrow_and_update();

        // Reload same empty config — no sandbox change
        cw.reload().unwrap();

        // Should NOT have changed
        assert!(!rx.has_changed().unwrap());
    }

    #[test]
    fn test_reload_invalid_toml_keeps_current() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("daemon.toml");
        std::fs::write(&config_path, "").unwrap();
        let initial = DaemonConfig::default();

        let initial_arc = Arc::new(initial.clone());
        let mut senders = HashMap::new();
        for section in [
            ConfigSection::Sandbox,
            ConfigSection::Tools,
            ConfigSection::Context,
            ConfigSection::Llm,
            ConfigSection::Tasks,
            ConfigSection::Plugins,
        ] {
            let (tx, _) = watch::channel(Arc::clone(&initial_arc));
            senders.insert(section, tx);
        }

        let cw = ConfigWatcher {
            config_path: config_path.clone(),
            current: RwLock::new(initial),
            senders,
        };

        // Write invalid TOML
        std::fs::write(&config_path, "[invalid toml {{{{").unwrap();

        // reload should return error
        assert!(cw.reload().is_err());

        // Current config should be unchanged (default)
        let current = cw.current.read().unwrap();
        assert!(current.sandbox.plugins.is_empty());
    }
}
