# Config Hot-Reload Design

**Issue:** #380
**Date:** 2026-03-20
**Status:** Draft

## Problem

When `daemon.toml` is modified, the daemon must be restarted for changes to take effect. This is inconvenient — users expect config changes (like sandbox permit rules) to apply without restart.

Additionally, the existing file watcher in `plugin.rs` (for `tool.override.json`) is self-contained and not reusable. A shared mechanism would serve both use cases.

## Design

### Architecture

```
FileWatcher (shared, generic)
├── watches daemon.toml → ConfigWatcher
│   └── diffs SandboxConfig → sandbox_rules subscriber (recompiles PermitRule)
│   └── (future: Tools, Context, Llm, Tasks subscribers)
└── watches ~/.omnish/plugins/ → PluginManager (migrated from self-contained watcher)
```

### Module 1: FileWatcher (`file_watcher.rs`)

Generic file/directory watcher extracted from the current `plugin.rs` inotify/polling code.

```rust
pub struct FileWatcher {
    inner: Mutex<WatcherInner>,
    // Linux: Inotify fd (created in new(), shared across watches)
    // non-Linux: poll state
}

struct WatcherInner {
    watches: Vec<(PathBuf, watch::Sender<()>)>,
    // non-Linux: HashMap<PathBuf, SystemTime> for mtime tracking
}

impl FileWatcher {
    pub fn new() -> Self;

    /// Register a path to watch. Can be called at any time, including
    /// after run() has started. Returns a Receiver that fires on change.
    pub fn watch(&self, path: PathBuf) -> watch::Receiver<()>;

    /// Start the event loop. Takes &self — the watcher remains usable
    /// for dynamic watch registration by other components.
    pub async fn run(&self);
}
```

**Platform behavior:**
- **Linux:** Single `nix::sys::inotify::Inotify` fd, non-blocking, wrapped in `tokio::io::unix::AsyncFd`. Watches for `IN_CLOSE_WRITE`, `IN_CREATE`, `IN_MOVED_TO`.
- **Non-Linux:** Polls file mtime every 5 seconds via `tokio::time::sleep`.

**Key properties:**
- Single event loop serves all watched paths (one inotify fd, one poll loop).
- Each watched path gets its own `watch::Sender<()>` / `Receiver<()>` pair.
- For directory watches, any file change within the directory fires the notification.
- `watch` channel coalesces rapid changes naturally — multiple `send()` calls before `changed().await` returns result in a single notification. This is intentional debouncing (e.g. vim's write-temp-then-rename pattern).
- `watch()` can be called at any time — before or after `run()`. Internal `Mutex` protects the watch list. Each module registers its own watches via `Arc<FileWatcher>` dependency injection.

### Module 2: ConfigWatcher (`config_watcher.rs`)

Watches `daemon.toml`, re-parses on change, diffs config sections, notifies only changed sections.

```rust
#[derive(Hash, Eq, PartialEq)]
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
    /// Create with initial config. Registers file watch on config_path
    /// via the shared FileWatcher.
    pub fn new(config_path: PathBuf, initial: DaemonConfig, file_watcher: Arc<FileWatcher>) -> Self;

    /// Subscribe to a specific config section. Returns a Receiver
    /// that fires only when that section changes.
    pub fn subscribe(&self, section: ConfigSection) -> watch::Receiver<Arc<DaemonConfig>>;

    /// Re-read daemon.toml, diff each section, notify changed sections.
    /// Called by the file watcher event loop.
    /// File I/O and TOML parsing happen BEFORE acquiring the write lock
    /// to avoid blocking the tokio worker thread.
    pub fn reload(&self) -> anyhow::Result<()>;
}
```

**`reload()` implementation order:**
1. Read file and parse TOML (no lock held — this is blocking I/O but infrequent).
2. Acquire write lock on `current`.
3. Diff each section via `PartialEq`, notify changed sections.
4. Swap in new config, release lock.

**Diffing:** Each `ConfigSection` maps to a field on `DaemonConfig`. On reload, compare old vs new via `PartialEq`. Only call `sender.send()` for sections that differ.

**Section mapping:**
| ConfigSection | Config field | PartialEq type |
|---|---|---|
| Sandbox | `config.sandbox` | `SandboxConfig` |
| Tools | `config.tools` | `HashMap<String, HashMap<String, Value>>` (already PartialEq) |
| Context | `config.context` | `ContextConfig` (future) |
| Llm | `config.llm` | `LlmConfig` (future) |
| Tasks | `config.tasks` | `TasksConfig` (future) |
| Plugins | `config.plugins` | `PluginsConfig` (future) |

**Error handling:** If file read or TOML parsing fails (including file deletion), log a warning and keep the current config. Do not notify subscribers.

**Note:** `ConfigSection::Tools` and other future sections are defined in the enum but have no subscribers in this iteration. Only `Sandbox` is wired up.

### Module 3: Subscriber — Sandbox Rules

The only subscriber wired up in this iteration.

**In `server.rs`:**
- Change `sandbox_rules` field from `Arc<HashMap<...>>` to `Arc<RwLock<HashMap<...>>>`.
- Tool execution reads via `sandbox_rules.read()`.

**In `main.rs`:**
- Subscribe to `ConfigSection::Sandbox`.
- Spawn a task that loops on `rx.changed()`:
  1. Read new config from receiver.
  2. Call `sandbox_rules::compile_config(&config.sandbox)`.
  3. Swap into `Arc<RwLock<...>>`.
  4. Log: `"sandbox rules reloaded: N rules for M tools"`.

### Migration: PluginManager

**Remove from `plugin.rs`:**
- The `watch_overrides()` method (both Linux and non-Linux implementations).
- All inotify/polling code.

**Replace with:**
- `PluginManager` receives `Arc<FileWatcher>` and registers its own watch on the plugins directory internally.
- A spawned task loops on `rx.changed()` and calls `self.reload_overrides()` (existing method, unchanged).

### Wiring in `main.rs`

```rust
// Create shared file watcher and start event loop
let file_watcher = Arc::new(FileWatcher::new());
let fw = Arc::clone(&file_watcher);
tokio::spawn(async move { fw.run().await });

// ConfigWatcher: registers its own watch on daemon.toml internally
let config_watcher = Arc::new(ConfigWatcher::new(
    config_path, config.clone(), Arc::clone(&file_watcher),
));
// ConfigWatcher spawns its own reload task internally

// Sandbox rules subscriber
let sandbox_rx = config_watcher.subscribe(ConfigSection::Sandbox);
let sr = Arc::clone(&sandbox_rules);
tokio::spawn(async move {
    let mut rx = sandbox_rx;
    while rx.changed().await.is_ok() {
        let config = rx.borrow_and_update().clone(); // clone Arc, release borrow
        let new_rules = sandbox_rules::compile_config(&config.sandbox);
        *sr.write().unwrap() = new_rules;
        tracing::info!("sandbox rules reloaded");
    }
});

// PluginManager: receives Arc<FileWatcher>, registers its own watch internally
let plugin_mgr = PluginManager::new(plugins_dir, Arc::clone(&file_watcher));
```

### Config Type Changes

**`omnish-common/src/config.rs`:**

Add `Clone` derive to (needed for `Arc<DaemonConfig>` in watch channel):
- `DaemonConfig`
- `LlmConfig`, `LlmBackendConfig`, `LangfuseConfig`
- `ShellConfig`
- (all sub-types already have clonable fields)

Add `PartialEq` derive to:
- `SandboxConfig`
- `SandboxPluginConfig`

Other config types get `PartialEq` when their subscribers are added (future work).

### `server.rs` Changes

Change `SandboxRules` type alias:
```rust
// Before:
type SandboxRules = Arc<HashMap<String, Vec<crate::sandbox_rules::PermitRule>>>;

// After:
type SandboxRules = Arc<std::sync::RwLock<HashMap<String, Vec<crate::sandbox_rules::PermitRule>>>>;
```

At the check_bypass call site, acquire read lock:
```rust
let rules = sandbox_rules.read().unwrap();
let matched_rule = crate::sandbox_rules::check_bypass(
    rules.get(&tc.name).map(|v| v.as_slice()).unwrap_or(&[]),
    &tc.input,
);
```

## Scope

**In scope:**
- Shared `FileWatcher` (inotify + polling fallback)
- `ConfigWatcher` with section-based pub-sub
- Sandbox rules subscriber (recompile on change)
- Plugin manager migration to shared `FileWatcher`

**Out of scope (future):**
- Tools, Context, LLM, Tasks, Plugins config subscribers
- SIGHUP signal handling
- Config validation beyond TOML parsing
