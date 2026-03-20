# Config Hot-Reload Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Hot-reload `daemon.toml` config changes without daemon restart, starting with sandbox permit rules.

**Architecture:** A shared `FileWatcher` (inotify on Linux, polling on other platforms) notifies a `ConfigWatcher` when `daemon.toml` changes. `ConfigWatcher` diffs config sections via `PartialEq` and notifies subscribers via `tokio::sync::watch` channels. `PluginManager` is migrated from its self-contained watcher to use the shared `FileWatcher`.

**Tech Stack:** Rust, tokio, nix (inotify), tokio::sync::watch channels

**Spec:** `docs/superpowers/specs/2026-03-20-config-hot-reload-design.md`

---

### Task 1: Add `Clone` and `PartialEq` derives to config types

**Files:**
- Modify: `crates/omnish-common/src/config.rs`

- [ ] **Step 1: Add `Clone` to `DaemonConfig`**

In `crates/omnish-common/src/config.rs:238`, change:
```rust
#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
```
to:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct DaemonConfig {
```

- [ ] **Step 2: Add `Clone` to `LlmConfig`**

At line 335, change:
```rust
#[derive(Debug, Deserialize)]
pub struct LlmConfig {
```
to:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct LlmConfig {
```

- [ ] **Step 3: Add `Clone` to `LlmBackendConfig`**

At line 389, change:
```rust
#[derive(Debug, Deserialize)]
pub struct LlmBackendConfig {
```
to:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct LlmBackendConfig {
```

- [ ] **Step 4: Add `Clone` to `LangfuseConfig`**

At line 372, change:
```rust
#[derive(Debug, Deserialize)]
pub struct LangfuseConfig {
```
to:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct LangfuseConfig {
```

- [ ] **Step 5: Add `Clone` to `ShellConfig`**

At line 288, change:
```rust
#[derive(Debug, Deserialize)]
pub struct ShellConfig {
```
to:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct ShellConfig {
```

- [ ] **Step 6: Add `Clone` to `ClientConfig`**

At line 28, change:
```rust
#[derive(Debug, Deserialize)]
pub struct ClientConfig {
```
to:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct ClientConfig {
```

- [ ] **Step 7: Add `PartialEq` to `SandboxConfig` and `SandboxPluginConfig`**

At line 218, change:
```rust
#[derive(Debug, Deserialize, Clone, Default)]
pub struct SandboxConfig {
```
to:
```rust
#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
pub struct SandboxConfig {
```

At line 226, change:
```rust
#[derive(Debug, Deserialize, Clone, Default)]
pub struct SandboxPluginConfig {
```
to:
```rust
#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
pub struct SandboxPluginConfig {
```

- [ ] **Step 8: Verify it compiles**

Run: `cargo build -p omnish-common 2>&1 | tail -5`
Expected: successful build

- [ ] **Step 9: Commit**

```bash
git add crates/omnish-common/src/config.rs
git commit -m "refactor: add Clone/PartialEq derives to config types for hot-reload"
```

---

### Task 2: Create `FileWatcher` module

**Files:**
- Create: `crates/omnish-daemon/src/file_watcher.rs`
- Modify: `crates/omnish-daemon/src/main.rs` (add `mod file_watcher;`)

- [ ] **Step 1: Write tests for FileWatcher**

Create `crates/omnish-daemon/src/file_watcher.rs` with tests:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::sync::watch;

pub struct FileWatcher {
    inner: Mutex<WatcherInner>,
    #[cfg(target_os = "linux")]
    inotify: nix::sys::inotify::Inotify,
}

struct WatcherInner {
    watches: Vec<(PathBuf, watch::Sender<()>)>,
    #[cfg(not(target_os = "linux"))]
    mtimes: HashMap<PathBuf, std::time::SystemTime>,
}

impl FileWatcher {
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        let inotify = {
            use nix::sys::inotify::{InitFlags, Inotify};
            Inotify::init(InitFlags::IN_NONBLOCK)
                .expect("failed to init inotify")
        };

        Self {
            inner: Mutex::new(WatcherInner {
                watches: Vec::new(),
                #[cfg(not(target_os = "linux"))]
                mtimes: HashMap::new(),
            }),
            #[cfg(target_os = "linux")]
            inotify,
        }
    }

    /// Register a path to watch. Can be called at any time.
    /// Returns a Receiver that fires on change.
    ///
    /// For files: watches the **parent directory** via inotify and filters by
    /// filename. This survives editor save-and-rename patterns (vim, sed -i)
    /// that create a new inode. For directories: watches the directory directly.
    pub fn watch(&self, path: PathBuf) -> watch::Receiver<()> {
        let (tx, rx) = watch::channel(());

        let mut inner = self.inner.lock().unwrap();

        #[cfg(target_os = "linux")]
        {
            use nix::sys::inotify::AddWatchFlags;
            let flags = AddWatchFlags::IN_CREATE
                | AddWatchFlags::IN_CLOSE_WRITE
                | AddWatchFlags::IN_MOVED_TO;
            // For files, watch parent directory (survives editor rename patterns).
            // For directories, watch the directory itself.
            let watch_target = if path.is_dir() {
                path.clone()
            } else {
                path.parent().unwrap_or(Path::new("/")).to_path_buf()
            };
            if let Err(e) = self.inotify.add_watch(&watch_target, flags) {
                tracing::warn!("failed to watch {}: {}", watch_target.display(), e);
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(mtime) = meta.modified() {
                    inner.mtimes.insert(path.clone(), mtime);
                }
            }
        }

        inner.watches.push((path, tx));
        rx
    }

    /// Start the event loop. Takes &self so the watcher remains usable
    /// for dynamic watch registration.
    #[cfg(target_os = "linux")]
    pub async fn run(&self) {
        use std::os::fd::AsFd;
        use tokio::io::unix::AsyncFd;
        use tokio::io::Interest;

        let async_fd = match AsyncFd::with_interest(
            self.inotify.as_fd().try_clone_to_owned().unwrap(),
            Interest::READABLE,
        ) {
            Ok(fd) => fd,
            Err(e) => {
                tracing::warn!("failed to create AsyncFd for inotify: {}", e);
                return;
            }
        };

        tracing::info!("file watcher started (inotify)");

        loop {
            let mut guard = match async_fd.readable().await {
                Ok(g) => g,
                Err(e) => {
                    tracing::warn!("inotify readable error: {}", e);
                    break;
                }
            };

            // Collect changed filenames from inotify events
            let mut changed_names: Vec<String> = Vec::new();
            loop {
                match self.inotify.read_events() {
                    Ok(events) => {
                        if events.is_empty() {
                            break;
                        }
                        for event in &events {
                            if let Some(name) = &event.name {
                                changed_names.push(name.to_string_lossy().to_string());
                            } else {
                                // Directory-level event (no name) — treat as changed
                                changed_names.push(String::new());
                            }
                        }
                    }
                    Err(nix::errno::Errno::EAGAIN) => break,
                    Err(e) => {
                        tracing::warn!("inotify read error: {}", e);
                        break;
                    }
                }
            }

            guard.clear_ready();

            // Lock once, match event filenames against registered watches
            if !changed_names.is_empty() {
                let inner = self.inner.lock().unwrap();
                for (watched_path, sender) in &inner.watches {
                    let should_notify = if watched_path.is_dir() {
                        // Directory watch: any event in the dir triggers
                        changed_names.iter().any(|_| true)
                    } else {
                        // File watch: match by filename (parent dir is watched)
                        let file_name = watched_path.file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default();
                        changed_names.iter().any(|n| n == &file_name)
                    };
                    if should_notify {
                        let _ = sender.send(());
                    }
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn run(&self) {
        tracing::info!("file watcher started (polling, 5s interval)");

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            // Collect changes first, then apply — avoids borrow conflict
            let inner = self.inner.lock().unwrap();
            let mut updates: Vec<(PathBuf, Option<std::time::SystemTime>)> = Vec::new();
            let mut to_notify: Vec<usize> = Vec::new();

            for (i, (path, _)) in inner.watches.iter().enumerate() {
                let current_mtime = std::fs::metadata(path)
                    .ok()
                    .and_then(|m| m.modified().ok());
                let prev_mtime = inner.mtimes.get(path).copied();

                let changed = match (current_mtime, prev_mtime) {
                    (Some(cur), Some(prev)) => cur != prev,
                    (Some(_), None) => true,   // file appeared
                    (None, Some(_)) => true,   // file disappeared
                    (None, None) => false,
                };

                if changed {
                    updates.push((path.clone(), current_mtime));
                    to_notify.push(i);
                }
            }
            drop(inner); // release immutable borrow

            if !updates.is_empty() {
                let mut inner = self.inner.lock().unwrap();
                for (path, mtime) in updates {
                    if let Some(t) = mtime {
                        inner.mtimes.insert(path, t);
                    } else {
                        inner.mtimes.remove(&path);
                    }
                }
                for i in to_notify {
                    let _ = inner.watches[i].1.send(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watch_returns_receiver() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.toml");
        std::fs::write(&path, "").unwrap();
        let fw = FileWatcher::new();
        let rx = fw.watch(path);
        assert!(!rx.has_changed().unwrap_or(true));
    }

    #[test]
    fn test_multiple_watches() {
        let tmp = tempfile::tempdir().unwrap();
        let fw = FileWatcher::new();
        let _rx1 = fw.watch(tmp.path().join("a.toml"));
        let _rx2 = fw.watch(tmp.path().join("b.toml"));
        let inner = fw.inner.lock().unwrap();
        assert_eq!(inner.watches.len(), 2);
    }
}
```

- [ ] **Step 2: Add `mod file_watcher;` to main.rs**

In `crates/omnish-daemon/src/main.rs`, after the existing `mod` declarations (line 1-2), add:
```rust
mod file_watcher;
```

- [ ] **Step 3: Run tests to verify**

Run: `cargo test -p omnish-daemon file_watcher 2>&1 | tail -10`
Expected: 2 tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/file_watcher.rs crates/omnish-daemon/src/main.rs
git commit -m "feat: add shared FileWatcher module (inotify + polling)"
```

---

### Task 3: Create `ConfigWatcher` module

**Files:**
- Create: `crates/omnish-daemon/src/config_watcher.rs`
- Modify: `crates/omnish-daemon/src/main.rs` (add `mod config_watcher;`)

- [ ] **Step 1: Write ConfigWatcher with tests**

Create `crates/omnish-daemon/src/config_watcher.rs`:

```rust
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

impl ConfigWatcher {
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
        if current.sandbox != new_config.sandbox {
            if let Some(tx) = self.senders.get(&ConfigSection::Sandbox) {
                let _ = tx.send(Arc::clone(&new_arc));
                tracing::info!("config section changed: Sandbox");
            }
        }

        // Future: diff Tools, Context, Llm, Tasks, Plugins sections here
        // (requires PartialEq on those types)

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

        let fw = FileWatcher::new();
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
```

- [ ] **Step 2: Add `mod config_watcher;` and `toml` dependency**

In `crates/omnish-daemon/src/main.rs`, add after existing mod declarations:
```rust
mod config_watcher;
```

Move `toml = "0.8"` from `[dev-dependencies]` to `[dependencies]` in `crates/omnish-daemon/Cargo.toml` (needed at runtime by `config_watcher.rs`). Remove the line from `[dev-dependencies]` and add to `[dependencies]`:

```toml
toml = "0.8"
```

- [ ] **Step 3: Run tests to verify**

Run: `cargo test -p omnish-daemon config_watcher 2>&1 | tail -10`
Expected: 3 tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/config_watcher.rs crates/omnish-daemon/src/main.rs crates/omnish-daemon/Cargo.toml Cargo.lock
git commit -m "feat: add ConfigWatcher with section-based pub-sub"
```

---

### Task 4: Change `SandboxRules` to `Arc<RwLock<...>>` in server.rs

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

- [ ] **Step 1: Change the type alias**

In `crates/omnish-daemon/src/server.rs:111`, change:
```rust
type SandboxRules = Arc<HashMap<String, Vec<crate::sandbox_rules::PermitRule>>>;
```
to:
```rust
type SandboxRules = Arc<std::sync::RwLock<HashMap<String, Vec<crate::sandbox_rules::PermitRule>>>>;
```

- [ ] **Step 2: Update `DaemonServer::new` to wrap in RwLock**

At line 244, change:
```rust
            sandbox_rules: Arc::new(sandbox_rules),
```
to:
```rust
            sandbox_rules: Arc::new(std::sync::RwLock::new(sandbox_rules)),
```

- [ ] **Step 3: Acquire read lock at the check_bypass call site**

At line 1125-1127, change:
```rust
                            let matched_rule = crate::sandbox_rules::check_bypass(
                                sandbox_rules.get(&tc.name).map(|v| v.as_slice()).unwrap_or(&[]),
                                &tc.input,
                            );
```
to:
```rust
                            let matched_rule = {
                                let rules = sandbox_rules.read().unwrap();
                                crate::sandbox_rules::check_bypass(
                                    rules.get(&tc.name).map(|v| v.as_slice()).unwrap_or(&[]),
                                    &tc.input,
                                ).map(|s| s.to_string())
                            };
```

Note: we need `.map(|s| s.to_string())` to avoid holding the read lock guard across the subsequent log and message construction. The `matched_rule` becomes `Option<String>` instead of `Option<&str>`.

- [ ] **Step 4: Update the log line and `sandboxed` field to use `Option<String>`**

At lines 1129-1135, change:
```rust
                            if let Some(rule) = matched_rule {
                                tracing::warn!(
                                    "sandbox bypass: tool={}, rule='{}', input={}",
                                    tc.name, rule,
                                    serde_json::to_string(&tc.input).unwrap_or_default(),
                                );
                            }
```
to:
```rust
                            if let Some(ref rule) = matched_rule {
                                tracing::warn!(
                                    "sandbox bypass: tool={}, rule='{}', input={}",
                                    tc.name, rule,
                                    serde_json::to_string(&tc.input).unwrap_or_default(),
                                );
                            }
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo build -p omnish-daemon 2>&1 | tail -10`
Expected: successful build

- [ ] **Step 6: Run all daemon tests**

Run: `cargo test -p omnish-daemon 2>&1 | tail -10`
Expected: all tests pass

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "refactor: wrap SandboxRules in RwLock for hot-reload"
```

---

### Task 5: Wire up FileWatcher, ConfigWatcher, and sandbox subscriber in main.rs

**Files:**
- Modify: `crates/omnish-daemon/src/main.rs`
- Modify: `crates/omnish-daemon/src/server.rs`

- [ ] **Step 1: Change `DaemonServer::new` to accept pre-wrapped `SandboxRules`**

The subscriber and the server must share the same `Arc<RwLock<...>>`. Change `DaemonServer::new` in `server.rs` to accept the pre-wrapped type.

Change parameter (line 232) from:
```rust
        sandbox_rules: HashMap<String, Vec<crate::sandbox_rules::PermitRule>>,
```
to:
```rust
        sandbox_rules: SandboxRules,
```

Change constructor body (line 244) from:
```rust
            sandbox_rules: Arc::new(std::sync::RwLock::new(sandbox_rules)),
```
to:
```rust
            sandbox_rules,
```

- [ ] **Step 2: Update sandbox_rules creation in main.rs**

In `main.rs`, change the sandbox_rules creation (line 268) from:
```rust
    let sandbox_rules = sandbox_rules::compile_config(&config.sandbox);
```
to:
```rust
    let sandbox_rules = sandbox_rules::compile_config(&config.sandbox);
    let server_sandbox_rules: Arc<std::sync::RwLock<_>> = Arc::new(std::sync::RwLock::new(sandbox_rules));
```

And update the `DaemonServer::new` call (line 269) to pass `Arc::clone(&server_sandbox_rules)` instead of `sandbox_rules`.

- [ ] **Step 3: Create FileWatcher and start event loop**

In `main.rs`, after plugin_mgr creation (line 256) and before the watch_overrides spawn (line 259), add:

```rust
    // Shared file watcher for config and plugin hot-reload
    let file_watcher = Arc::new(file_watcher::FileWatcher::new());
    let fw = Arc::clone(&file_watcher);
    tokio::spawn(async move { fw.run().await });
```

- [ ] **Step 4: Create ConfigWatcher**

After the file_watcher creation, add:

```rust
    // Config watcher: monitors daemon.toml, notifies subscribers on section changes
    let config_path = std::env::var("OMNISH_DAEMON_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| omnish_dir.join("daemon.toml"));
    let config_watcher = config_watcher::ConfigWatcher::new(
        config_path,
        config.clone(),
        &file_watcher,
    );
```

- [ ] **Step 5: Wire up sandbox rules subscriber**

After ConfigWatcher creation, add:

```rust
    // Hot-reload sandbox rules on config change
    {
        let sandbox_rx = config_watcher.subscribe(config_watcher::ConfigSection::Sandbox);
        let sr = Arc::clone(&server_sandbox_rules);
        tokio::spawn(async move {
            let mut rx = sandbox_rx;
            while rx.changed().await.is_ok() {
                let config = rx.borrow_and_update().clone();
                let new_rules = sandbox_rules::compile_config(&config.sandbox);
                let rule_count: usize = new_rules.values().map(|v| v.len()).sum();
                let tool_count = new_rules.len();
                *sr.write().unwrap() = new_rules;
                tracing::info!("sandbox rules reloaded: {} rules for {} tools", rule_count, tool_count);
            }
        });
    }
```

- [ ] **Step 6: Add `use std::path::PathBuf;` if not already imported in main.rs**

Check if `PathBuf` is imported. If not, add it.

- [ ] **Step 7: Verify it compiles**

Run: `cargo build -p omnish-daemon 2>&1 | tail -10`
Expected: successful build

- [ ] **Step 8: Run all tests**

Run: `cargo test -p omnish-daemon 2>&1 | tail -10`
Expected: all tests pass

- [ ] **Step 9: Commit**

```bash
git add crates/omnish-daemon/src/main.rs crates/omnish-daemon/src/server.rs
git commit -m "feat: wire config hot-reload with sandbox rules subscriber (#380)"
```

---

### Task 6: Migrate PluginManager to use shared FileWatcher

**Files:**
- Modify: `crates/omnish-daemon/src/plugin.rs`
- Modify: `crates/omnish-daemon/src/main.rs`

- [ ] **Step 1: Add `watch_with` method to PluginManager**

In `plugin.rs`, add a new method that uses the shared FileWatcher instead of self-contained inotify:

```rust
    /// Start watching plugin overrides using a shared FileWatcher.
    pub async fn watch_with(self: &Arc<Self>, file_watcher: &crate::file_watcher::FileWatcher) {
        let mut rx = file_watcher.watch(self.plugins_dir.clone());
        tracing::info!("watching plugin overrides via shared file watcher: {}", self.plugins_dir.display());
        while rx.changed().await.is_ok() {
            tracing::info!("tool.override.json changed, reloading...");
            self.reload_overrides();
        }
    }
```

- [ ] **Step 2: Remove old `watch_overrides` methods**

Delete both `#[cfg(target_os = "linux")]` and `#[cfg(not(target_os = "linux"))]` `watch_overrides` methods (lines 377-532).

- [ ] **Step 3: Update main.rs to use `watch_with`**

In `main.rs`, replace:
```rust
    let plugin_mgr_watcher = Arc::clone(&plugin_mgr);
    tokio::spawn(async move { plugin_mgr_watcher.watch_overrides().await });
```
with:
```rust
    let plugin_mgr_watcher = Arc::clone(&plugin_mgr);
    let fw_for_plugins = Arc::clone(&file_watcher);
    tokio::spawn(async move { plugin_mgr_watcher.watch_with(&fw_for_plugins).await });
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build -p omnish-daemon 2>&1 | tail -10`
Expected: successful build

- [ ] **Step 5: Run all tests**

Run: `cargo test -p omnish-daemon 2>&1 | tail -10`
Expected: all tests pass

- [ ] **Step 6: Full workspace build and test**

Run: `cargo build --workspace 2>&1 | tail -5 && cargo test --workspace 2>&1 | tail -15`
Expected: all builds and tests pass

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs crates/omnish-daemon/src/main.rs
git commit -m "refactor: migrate PluginManager to shared FileWatcher (#380)"
```

---

### Task 7: Manual smoke test

- [ ] **Step 1: Build the daemon**

Run: `cargo build -p omnish-daemon`

- [ ] **Step 2: Start the daemon and edit daemon.toml**

Start the daemon, edit `~/.omnish/daemon.toml` to add/remove a sandbox permit rule, and check the daemon logs for `"sandbox rules reloaded"` and `"config section changed: Sandbox"` messages.

- [ ] **Step 3: Verify plugin override reload still works**

Edit a `tool.override.json` file and verify the daemon picks up the change via the shared FileWatcher.
