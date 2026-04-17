# Unified Task System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify all scheduled tasks behind a common trait + config interface, with hot-reload support via ConfigWatcher.

**Architecture:** Each task implements `ScheduledTask` trait (name/schedule/enabled/create_job). All task configs live under `[tasks.<name>]` in daemon.toml with at least `enabled`. ConfigWatcher watches the Tasks section; on change, TaskManager removes all jobs and re-registers from new config. A `DaemonContext` struct holds daemon-level shared state (omnish_dir, restart_signal, update_cache). `ServerOpts` drops redundant proxy/no_proxy fields (read from daemon_config instead). `TaskContext` combines managers + DaemonContext + ServerOpts for task creation.

**Tech Stack:** Rust, tokio-cron-scheduler, tokio watch channels

---

### Task 1: Update config structs (omnish-common)

**Files:**
- Modify: `crates/omnish-common/src/config.rs:155-243`

- [ ] **Step 1: Add PartialEq to all task config structs and add `enabled` field where missing**

Replace the task config section (lines 155-243) with:

```rust
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct EvictionConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_session_evict_hours", deserialize_with = "string_or_int::deserialize")]
    pub session_evict_hours: u64,
}

impl Default for EvictionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            session_evict_hours: default_session_evict_hours(),
        }
    }
}

fn default_session_evict_hours() -> u64 {
    48
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct HourlySummaryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct DailyNotesConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DiskCleanupConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for DiskCleanupConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct AutoUpdateConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_auto_update_schedule")]
    pub schedule: String,
    #[serde(default)]
    pub check_url: Option<String>,
}

impl Default for AutoUpdateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            schedule: default_auto_update_schedule(),
            check_url: None,
        }
    }
}

fn default_auto_update_schedule() -> String {
    "0 0 4 * * *".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ThreadSummaryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for ThreadSummaryConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Tasks config
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct TasksConfig {
    #[serde(default)]
    pub eviction: EvictionConfig,
    #[serde(default)]
    pub hourly_summary: HourlySummaryConfig,
    #[serde(default)]
    pub daily_notes: DailyNotesConfig,
    #[serde(default)]
    pub disk_cleanup: DiskCleanupConfig,
    #[serde(default)]
    pub auto_update: AutoUpdateConfig,
    #[serde(default)]
    pub thread_summary: ThreadSummaryConfig,
}
```

- [ ] **Step 2: Remove `default_disk_cleanup_schedule` function**

It is no longer referenced. Search and delete if present.

- [ ] **Step 3: Build to verify**

Run: `cargo build --release 2>&1 | tail -5`
Expected: compile errors in main.rs (references to old config fields) - that's OK, we fix those in later tasks.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-common/src/config.rs
git commit -m "refactor: unify task config structs with enabled field and PartialEq"
```

---

### Task 2: Remove proxy/no_proxy from ServerOpts

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs:126-132` (ServerOpts struct)
- Modify: `crates/omnish-daemon/src/server.rs:1525` (execute_daemon_plugin call)
- Modify: `crates/omnish-daemon/src/main.rs:390-396` (ServerOpts construction)

- [ ] **Step 1: Remove proxy/no_proxy fields from ServerOpts**

In `server.rs`, change:

```rust
pub struct ServerOpts {
    pub proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub sandbox_rules: SandboxRules,
    pub config_path: std::path::PathBuf,
    pub daemon_config: std::sync::Arc<std::sync::RwLock<omnish_common::config::DaemonConfig>>,
}
```

to:

```rust
pub struct ServerOpts {
    pub sandbox_rules: SandboxRules,
    pub config_path: std::path::PathBuf,
    pub daemon_config: std::sync::Arc<std::sync::RwLock<omnish_common::config::DaemonConfig>>,
}
```

- [ ] **Step 2: Update all `opts.proxy` / `opts.no_proxy` usages to read from daemon_config**

In `server.rs` line ~1525, change:

```rust
execute_daemon_plugin(&exe, &tc.name, &merged_input, opts.proxy.as_deref(), opts.no_proxy.as_deref()).await
```

to:

```rust
let dc = opts.daemon_config.read().unwrap();
let proxy = dc.proxy.clone();
let no_proxy = dc.no_proxy.clone();
drop(dc);
execute_daemon_plugin(&exe, &tc.name, &merged_input, proxy.as_deref(), no_proxy.as_deref()).await
```

(There are two call sites - search for `opts.proxy` and update both.)

- [ ] **Step 3: Update ServerOpts construction in main.rs**

Change:

```rust
let server_opts = Arc::new(server::ServerOpts {
    proxy: config.proxy,
    no_proxy: config.no_proxy,
    sandbox_rules: Arc::clone(&server_sandbox_rules),
    config_path: config_path.clone(),
    daemon_config: Arc::clone(&daemon_config_arc),
});
```

to:

```rust
let server_opts = Arc::new(server::ServerOpts {
    sandbox_rules: Arc::clone(&server_sandbox_rules),
    config_path: config_path.clone(),
    daemon_config: Arc::clone(&daemon_config_arc),
});
```

- [ ] **Step 4: Build to verify**

Run: `cargo build --release 2>&1 | tail -5`
Expected: success (or expected errors from task config changes, not from proxy removal)

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/main.rs
git commit -m "refactor: remove proxy/no_proxy from ServerOpts, read from daemon_config"
```

---

### Task 3: Add ScheduledTask trait, DaemonContext, TaskContext, and reload to TaskManager

**Files:**
- Modify: `crates/omnish-daemon/src/task_mgr.rs`

- [ ] **Step 1: Rewrite task_mgr.rs with trait, context structs, and reload method**

```rust
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_cron_scheduler::{Job, JobScheduler};
use uuid::Uuid;

use crate::conversation_mgr::ConversationManager;
use crate::session_mgr::SessionManager;
use crate::update_cache::UpdateCache;
use omnish_llm::factory::SharedLlmBackend;

/// Daemon-level shared infrastructure (immutable after startup).
pub struct DaemonContext {
    pub omnish_dir: PathBuf,
    pub restart_signal: Arc<Notify>,
    pub update_cache: Arc<UpdateCache>,
}

/// Everything a ScheduledTask needs to create its Job.
pub struct TaskContext {
    pub session_mgr: Arc<SessionManager>,
    pub conv_mgr: Arc<ConversationManager>,
    pub llm_backend: SharedLlmBackend,
    pub daemon: Arc<DaemonContext>,
    pub opts: Arc<crate::server::ServerOpts>,
}

/// Unified interface for all scheduled tasks.
pub trait ScheduledTask: Send + Sync {
    fn name(&self) -> &'static str;
    fn schedule(&self) -> &str;
    fn enabled(&self) -> bool;
    fn create_job(&self, ctx: &TaskContext) -> Result<Job>;
}

struct TaskEntry {
    uuid: Uuid,
    cron: String,
    enabled: bool,
}

pub struct TaskManager {
    scheduler: JobScheduler,
    tasks: HashMap<String, TaskEntry>,
}

impl TaskManager {
    pub async fn new() -> Result<Self> {
        let scheduler = JobScheduler::new().await?;
        Ok(Self {
            scheduler,
            tasks: HashMap::new(),
        })
    }

    pub async fn register(&mut self, name: &str, cron: &str, job: Job) -> Result<()> {
        let uuid = self.scheduler.add(job).await?;
        self.tasks.insert(name.to_string(), TaskEntry {
            uuid,
            cron: cron.to_string(),
            enabled: true,
        });
        tracing::info!("registered task '{}' with schedule '{}'", name, cron);
        Ok(())
    }

    pub async fn start(&self) -> Result<()> {
        self.scheduler.start().await?;
        Ok(())
    }

    /// Remove all existing jobs and re-register from the given task list.
    pub async fn reload(
        &mut self,
        tasks: &[Box<dyn ScheduledTask>],
        ctx: &TaskContext,
    ) -> Result<()> {
        // Remove all current jobs
        for (name, entry) in self.tasks.drain() {
            if entry.enabled {
                if let Err(e) = self.scheduler.remove(&entry.uuid).await {
                    tracing::warn!("failed to remove task '{}': {}", name, e);
                }
            }
        }
        // Re-register enabled tasks
        for task in tasks {
            if task.enabled() {
                match task.create_job(ctx) {
                    Ok(job) => {
                        self.register(task.name(), task.schedule(), job).await?;
                    }
                    Err(e) => {
                        tracing::warn!("failed to create job for '{}': {}", task.name(), e);
                    }
                }
            } else {
                tracing::debug!("task '{}' is disabled, skipping", task.name());
            }
        }
        tracing::info!("task reload complete: {} tasks registered", self.tasks.len());
        Ok(())
    }

    pub fn list(&self) -> Vec<(String, String, bool)> {
        self.tasks
            .iter()
            .map(|(name, entry)| (name.clone(), entry.cron.clone(), entry.enabled))
            .collect()
    }

    pub async fn disable(&mut self, name: &str) -> Result<()> {
        let entry = self.tasks.get_mut(name)
            .ok_or_else(|| anyhow::anyhow!("task '{}' not found", name))?;
        if !entry.enabled {
            return Ok(());
        }
        self.scheduler.remove(&entry.uuid).await?;
        entry.enabled = false;
        tracing::info!("disabled task '{}'", name);
        Ok(())
    }

    pub fn format_list(&self) -> String {
        if self.tasks.is_empty() {
            return "No scheduled tasks.".to_string();
        }
        let mut lines = vec!["Scheduled tasks:".to_string()];
        let mut entries: Vec<_> = self.tasks.iter().collect();
        entries.sort_by_key(|(name, _)| (*name).clone());
        for (name, entry) in entries {
            let status = if entry.enabled { "enabled" } else { "disabled" };
            lines.push(format!("  {} [{}] ({})", name, entry.cron, status));
        }
        lines.join("\n")
    }
}
```

- [ ] **Step 2: Build to verify**

Run: `cargo build --release -p omnish-daemon 2>&1 | tail -10`
Expected: may have errors from server.rs import (ServerOpts is used in task_mgr.rs now). Fix circular import if needed - ServerOpts is defined in server.rs which is in the binary crate, not the library crate. If so, move ServerOpts definition or use a forward reference. See Task 3 Step 3.

- [ ] **Step 3: Handle module visibility**

`task_mgr.rs` is in the `omnish-daemon` library crate (`crates/omnish-daemon/src/lib.rs`). `server.rs` is in the binary crate (`crates/omnish-daemon/src/main.rs` includes it via `mod server`). `TaskContext` references `crate::server::ServerOpts` which won't compile from the lib crate.

**Solution:** Move `ServerOpts` into the library crate. The simplest approach: define it in `task_mgr.rs` alongside `DaemonContext` and `TaskContext`, since they're all "shared daemon types". Then `server.rs` imports it from `omnish_daemon::task_mgr::ServerOpts`.

Update the `TaskContext` struct to use `ServerOpts` directly (no `crate::server::` prefix).

In `server.rs`, change:
```rust
use omnish_daemon::task_mgr::ServerOpts;
```

And remove the old `ServerOpts` definition from `server.rs`.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/task_mgr.rs
git commit -m "feat: add ScheduledTask trait, DaemonContext, TaskContext, and reload to TaskManager"
```

---

### Task 4: Implement ScheduledTask for each task module

**Files:**
- Modify: `crates/omnish-daemon/src/eviction.rs`
- Modify: `crates/omnish-daemon/src/hourly_summary.rs`
- Modify: `crates/omnish-daemon/src/daily_notes.rs`
- Modify: `crates/omnish-daemon/src/cleanup.rs`
- Modify: `crates/omnish-daemon/src/auto_update.rs`
- Modify: `crates/omnish-daemon/src/thread_summary.rs`

- [ ] **Step 1: Implement ScheduledTask for eviction**

In `eviction.rs`, replace the entire file (keeping the test):

```rust
use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio_cron_scheduler::Job;

pub struct EvictionTask(pub omnish_common::config::EvictionConfig);

impl ScheduledTask for EvictionTask {
    fn name(&self) -> &'static str { "eviction" }
    fn schedule(&self) -> &str { "0 0 * * * *" }
    fn enabled(&self) -> bool { self.0.enabled }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let mgr = Arc::clone(&ctx.session_mgr);
        let max_inactive = Duration::from_secs(self.0.session_evict_hours * 3600);
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let mgr = mgr.clone();
            Box::pin(async move {
                tracing::debug!("task [eviction] started");
                let evicted = mgr.evict_inactive(max_inactive).await;
                if evicted > 0 {
                    tracing::info!("task [eviction] evicted {} inactive sessions", evicted);
                }
                tracing::debug!("task [eviction] finished");
            })
        })?)
    }
}
```

- [ ] **Step 2: Implement ScheduledTask for hourly_summary**

In `hourly_summary.rs`, replace `create_hourly_summary_job` with:

```rust
pub struct HourlySummaryTask(pub omnish_common::config::HourlySummaryConfig);

impl ScheduledTask for HourlySummaryTask {
    fn name(&self) -> &'static str { "hourly_summary" }
    fn schedule(&self) -> &str { "0 0 */4 * * *" }
    fn enabled(&self) -> bool { self.0.enabled }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let mgr = Arc::clone(&ctx.session_mgr);
        let conv_mgr = Arc::clone(&ctx.conv_mgr);
        let llm_holder = ctx.llm_backend.clone();
        let notes_dir = ctx.daemon.omnish_dir.join("notes");
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let mgr = mgr.clone();
            let conv_mgr = conv_mgr.clone();
            let llm = llm_holder.read().unwrap().get_backend(UseCase::Analysis);
            let dir = notes_dir.clone();
            Box::pin(async move {
                tracing::debug!("task [hourly_summary] started");
                if let Err(e) = generate_hourly_summary(&mgr, &conv_mgr, Some(llm.as_ref()), &dir).await {
                    tracing::warn!("task [hourly_summary] failed: {}", e);
                }
                tracing::debug!("task [hourly_summary] finished");
            })
        })?)
    }
}
```

Keep all existing functions (`build_hourly_context`, `generate_hourly_summary`) and tests. Remove the old `create_hourly_summary_job` function. Add `use crate::task_mgr::ScheduledTask;` and `use anyhow::Result;` to imports.

- [ ] **Step 3: Implement ScheduledTask for daily_notes**

In `daily_notes.rs`, replace `create_daily_notes_job` with:

```rust
pub struct DailyNotesTask(pub omnish_common::config::DailyNotesConfig);

impl ScheduledTask for DailyNotesTask {
    fn name(&self) -> &'static str { "daily_notes" }
    fn schedule(&self) -> &str { "0 10 0 * * *" }
    fn enabled(&self) -> bool { self.0.enabled }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let mgr = Arc::clone(&ctx.session_mgr);
        let conv_mgr = Arc::clone(&ctx.conv_mgr);
        let llm_holder = ctx.llm_backend.clone();
        let notes_dir = ctx.daemon.omnish_dir.join("notes");
        Ok(Job::new_async_tz(self.schedule(), Local, move |_uuid, _lock| {
            let mgr = mgr.clone();
            let conv_mgr = conv_mgr.clone();
            let llm = llm_holder.read().unwrap().get_backend(UseCase::Analysis);
            let dir = notes_dir.clone();
            Box::pin(async move {
                tracing::debug!("task [daily_notes] started");
                if let Err(e) = generate_daily_note(&mgr, &conv_mgr, Some(llm.as_ref()), &dir).await {
                    tracing::warn!("task [daily_notes] failed: {}", e);
                }
                tracing::debug!("task [daily_notes] finished");
            })
        })?)
    }
}
```

- [ ] **Step 4: Implement ScheduledTask for cleanup**

In `cleanup.rs`, replace `create_disk_cleanup_job` with:

```rust
use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio_cron_scheduler::Job;

pub struct DiskCleanupTask(pub omnish_common::config::DiskCleanupConfig);

impl ScheduledTask for DiskCleanupTask {
    fn name(&self) -> &'static str { "disk_cleanup" }
    fn schedule(&self) -> &str { "0 0 */6 * * *" }
    fn enabled(&self) -> bool { self.0.enabled }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let mgr = Arc::clone(&ctx.session_mgr);
        let max_age = Duration::from_secs(48 * 3600);
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let mgr = mgr.clone();
            Box::pin(async move {
                tracing::debug!("task [disk_cleanup] started");
                let cleaned = mgr.cleanup_expired_dirs(max_age).await;
                if cleaned > 0 {
                    tracing::info!("task [disk_cleanup] cleaned {} expired session directories", cleaned);
                }
                tracing::debug!("task [disk_cleanup] finished");
            })
        })?)
    }
}
```

- [ ] **Step 5: Implement ScheduledTask for auto_update**

In `auto_update.rs`, replace `create_auto_update_job` with:

```rust
use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use std::sync::Arc;
use tokio_cron_scheduler::Job;

pub struct AutoUpdateTask(pub omnish_common::config::AutoUpdateConfig);

impl ScheduledTask for AutoUpdateTask {
    fn name(&self) -> &'static str { "auto_update" }
    fn schedule(&self) -> &str { &self.0.schedule }
    fn enabled(&self) -> bool { self.0.enabled }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let check_url = self.0.check_url.clone();
        let restart_signal = Arc::clone(&ctx.daemon.restart_signal);
        let update_cache = Arc::clone(&ctx.daemon.update_cache);
        let daemon_config = Arc::clone(&ctx.opts.daemon_config);
        Ok(Job::new_async_tz(self.schedule(), chrono::Local, move |_uuid, _lock| {
            let check_url = check_url.clone();
            let restart_signal = restart_signal.clone();
            let update_cache = update_cache.clone();
            let daemon_config = daemon_config.clone();
            Box::pin(async move {
                let (proxy, no_proxy) = {
                    let dc = daemon_config.read().unwrap();
                    (dc.proxy.clone(), dc.no_proxy.clone())
                };
                // ... (keep the entire existing async body from the current
                // create_auto_update_job, replacing references to the captured
                // proxy/no_proxy variables with the ones read from daemon_config above)
            })
        })?)
    }
}
```

The async body is the same as the current `create_auto_update_job` closure body (lines 25-121 of current auto_update.rs). The only change: `proxy` and `no_proxy` are now read from `daemon_config` at the start of each run instead of captured at creation time.

- [ ] **Step 6: Implement ScheduledTask for thread_summary**

In `thread_summary.rs`, replace `create_thread_summary_job` with:

```rust
pub struct ThreadSummaryTask(pub omnish_common::config::ThreadSummaryConfig);

impl ScheduledTask for ThreadSummaryTask {
    fn name(&self) -> &'static str { "thread_summary" }
    fn schedule(&self) -> &str { "0 * * * * *" }
    fn enabled(&self) -> bool { self.0.enabled }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let conv_mgr = Arc::clone(&ctx.conv_mgr);
        let llm_holder = ctx.llm_backend.clone();
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let conv_mgr = conv_mgr.clone();
            let llm = llm_holder.read().unwrap().get_backend(UseCase::Chat);
            Box::pin(async move {
                tracing::debug!("task [thread_summary] started");
                if let Err(e) = generate_thread_summaries(&conv_mgr, Some(llm.as_ref())).await {
                    tracing::warn!("task [thread_summary] failed: {}", e);
                }
                tracing::debug!("task [thread_summary] finished");
            })
        })?)
    }
}
```

- [ ] **Step 7: Add factory function**

In `task_mgr.rs`, add at the bottom (before the closing of the module):

```rust
/// Create all scheduled tasks from config.
pub fn create_all_tasks(config: &omnish_common::config::TasksConfig) -> Vec<Box<dyn ScheduledTask>> {
    vec![
        Box::new(crate::eviction::EvictionTask(config.eviction.clone())),
        Box::new(crate::hourly_summary::HourlySummaryTask(config.hourly_summary.clone())),
        Box::new(crate::daily_notes::DailyNotesTask(config.daily_notes.clone())),
        Box::new(crate::cleanup::DiskCleanupTask(config.disk_cleanup.clone())),
        Box::new(crate::auto_update::AutoUpdateTask(config.auto_update.clone())),
        Box::new(crate::thread_summary::ThreadSummaryTask(config.thread_summary.clone())),
    ]
}
```

- [ ] **Step 8: Fix test compilation in task modules**

Update tests in `cleanup.rs` and other modules to use the new struct-based API. For example in cleanup.rs:

```rust
#[test]
fn test_create_disk_cleanup_job() {
    // ... setup ...
    let task = DiskCleanupTask(omnish_common::config::DiskCleanupConfig::default());
    assert!(task.enabled());
    assert_eq!(task.schedule(), "0 0 */6 * * *");
}
```

- [ ] **Step 9: Build to verify**

Run: `cargo build --release 2>&1 | tail -10`
Expected: errors only in main.rs (old task registration code). All library code should compile.

- [ ] **Step 10: Commit**

```bash
git add crates/omnish-daemon/src/eviction.rs crates/omnish-daemon/src/hourly_summary.rs \
  crates/omnish-daemon/src/daily_notes.rs crates/omnish-daemon/src/cleanup.rs \
  crates/omnish-daemon/src/auto_update.rs crates/omnish-daemon/src/thread_summary.rs \
  crates/omnish-daemon/src/task_mgr.rs
git commit -m "feat: implement ScheduledTask trait for all task modules"
```

---

### Task 5: Rewrite main.rs task registration with unified loop

**Files:**
- Modify: `crates/omnish-daemon/src/main.rs:144-251`

- [ ] **Step 1: Replace task registration block**

Replace lines 144-251 (from `let evict_hours` through `task_mgr.start()`) with:

```rust
    let session_mgr = Arc::new(SessionManager::new(omnish_dir.clone(), config.context.clone()));
    match session_mgr.load_existing().await {
        Ok(count) if count > 0 => tracing::info!("loaded {} existing session(s)", count),
        Ok(_) => {}
        Err(e) => tracing::warn!("failed to load existing sessions: {}", e),
    }

    let conv_mgr = Arc::new(ConversationManager::new(omnish_dir.join("threads")));

    // Restart signal: notified when auto-update installs a new binary
    let restart_signal = Arc::new(tokio::sync::Notify::new());

    // Update cache: stores downloaded packages for distribution to clients
    let update_cache = Arc::new(omnish_daemon::update_cache::UpdateCache::new(&omnish_dir));

    // Periodic scan of updates directory (every 60s) to refresh cached versions
    {
        let uc = Arc::clone(&update_cache);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                uc.scan_updates();
            }
        });
    }

    let daemon_ctx = Arc::new(omnish_daemon::task_mgr::DaemonContext {
        omnish_dir: omnish_dir.clone(),
        restart_signal: Arc::clone(&restart_signal),
        update_cache: Arc::clone(&update_cache),
    });
```

(The ServerOpts and TaskContext are created after config_watcher is set up - see Step 2.)

- [ ] **Step 2: Create TaskContext and register tasks after ServerOpts is built**

After `server_opts` is created (around current line 390), add the task registration:

```rust
    // Build TaskContext and register all tasks
    let task_ctx = omnish_daemon::task_mgr::TaskContext {
        session_mgr: Arc::clone(&session_mgr),
        conv_mgr: Arc::clone(&conv_mgr),
        llm_backend: llm_backend.clone(),
        daemon: Arc::clone(&daemon_ctx),
        opts: Arc::clone(&server_opts),
    };

    let mut task_mgr = omnish_daemon::task_mgr::TaskManager::new().await?;
    let all_tasks = omnish_daemon::task_mgr::create_all_tasks(&config.tasks);
    for task in &all_tasks {
        if task.enabled() {
            let job = task.create_job(&task_ctx)?;
            task_mgr.register(task.name(), task.schedule(), job).await?;
        }
    }
    task_mgr.start().await?;
    let task_mgr = Arc::new(tokio::sync::Mutex::new(task_mgr));
```

- [ ] **Step 3: Remove old imports**

Remove unused imports from main.rs:
- `create_daily_notes_job`
- `create_hourly_summary_job`

- [ ] **Step 4: Update DaemonServer::new call**

Replace the `update_cache` parameter with `daemon_ctx`:

The DaemonServer constructor needs updating to accept `Arc<DaemonContext>` instead of `Arc<UpdateCache>`. In `server.rs`, change the `update_cache` field to `daemon_ctx: Arc<task_mgr::DaemonContext>` and access `update_cache` via `self.daemon_ctx.update_cache`. Update all `self.update_cache` references.

In main.rs, update:
```rust
let server = DaemonServer::new(session_mgr, llm_backend, task_mgr, conv_mgr, plugin_mgr.clone(),
    tool_registry.clone(), server_opts, formatter_mgr, Arc::clone(&daemon_ctx));
```

- [ ] **Step 5: Build and verify**

Run: `cargo build --release 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-daemon/src/main.rs crates/omnish-daemon/src/server.rs
git commit -m "refactor: unified task registration loop with ScheduledTask trait"
```

---

### Task 6: Add Tasks section to ConfigWatcher hot-reload

**Files:**
- Modify: `crates/omnish-daemon/src/config_watcher.rs:46-54,115-132`
- Modify: `crates/omnish-daemon/src/main.rs` (add Tasks subscriber)

- [ ] **Step 1: Enable Tasks in WATCHED_SECTIONS**

In `config_watcher.rs`, change:

```rust
pub const WATCHED_SECTIONS: &[ConfigSection] = &[
    ConfigSection::Sandbox,
    ConfigSection::Llm,
    ConfigSection::Plugins,
];
```

to:

```rust
pub const WATCHED_SECTIONS: &[ConfigSection] = &[
    ConfigSection::Sandbox,
    ConfigSection::Llm,
    ConfigSection::Plugins,
    ConfigSection::Tasks,
];
```

- [ ] **Step 2: Add Tasks diff in reload()**

In the `reload()` method's match block, add the Tasks arm:

```rust
ConfigSection::Tasks => current.tasks != new_config.tasks,
```

- [ ] **Step 3: Add Tasks subscriber in main.rs**

After the existing hot-reload blocks (after plugins reload), add:

```rust
    // Hot-reload tasks on config change
    {
        let tasks_rx = config_watcher.subscribe(config_watcher::ConfigSection::Tasks);
        let tm = Arc::clone(&task_mgr);
        let task_ctx = task_ctx; // move into closure
        tokio::spawn(async move {
            let mut rx = tasks_rx;
            while rx.changed().await.is_ok() {
                let config = rx.borrow_and_update().clone();
                let all_tasks = omnish_daemon::task_mgr::create_all_tasks(&config.tasks);
                let mut mgr = tm.lock().await;
                if let Err(e) = mgr.reload(&all_tasks, &task_ctx).await {
                    tracing::warn!("task reload failed: {}", e);
                }
            }
        });
    }
```

- [ ] **Step 4: Also subscribe Tasks for daemon_config_arc sync**

In the existing `daemon_config_arc` sync block, add a Tasks receiver alongside the existing llm/sandbox/plugins receivers:

```rust
let tasks_rx = config_watcher.subscribe(config_watcher::ConfigSection::Tasks);
// ... inside the tokio::select! loop:
Ok(()) = tasks.changed() => {
    let config = tasks.borrow_and_update().clone();
    *dca.write().unwrap() = (*config).clone();
}
```

- [ ] **Step 5: Build and verify**

Run: `cargo build --release 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-daemon/src/config_watcher.rs crates/omnish-daemon/src/main.rs
git commit -m "feat: hot-reload tasks config via ConfigWatcher"
```

---

### Task 7: Update install.sh template

**Files:**
- Modify: `install.sh:750-764`

- [ ] **Step 1: Update the daemon.toml template**

Replace the tasks section in install.sh:

```bash
[tasks.eviction]
# enabled = true
# session_evict_hours = 48

[tasks.hourly_summary]
# enabled = true

[tasks.daily_notes]
# enabled = true

[tasks.disk_cleanup]
# enabled = true

[tasks.auto_update]
enabled = ${AUTO_UPDATE_ENABLED}
# schedule = "0 0 4 * * *"
${CHECK_URL_LINE}

[tasks.thread_summary]
# enabled = true
```

- [ ] **Step 2: Commit**

```bash
git add install.sh
git commit -m "refactor: update install.sh task config to match new unified schema"
```

---

### Task 8: Final build, test, and verify

- [ ] **Step 1: Full build**

Run: `cargo build --release 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 2: Run tests**

Run: `cargo test --release 2>&1 | tail -20`
Expected: all tests pass. Fix any test failures from the refactoring.

- [ ] **Step 3: Verify existing daemon.toml compatibility**

Ensure that a daemon.toml with the old `[tasks.disk_cleanup] schedule = "..."` still deserializes without error (the `schedule` field is just ignored since `DiskCleanupConfig` no longer has it - serde's default behavior with `#[serde(default)]` on the struct will skip unknown fields). If serde strict mode is on, add `#[serde(deny_unknown_fields)]` is NOT used, so unknown fields are silently ignored. Verify this.

- [ ] **Step 4: Final commit if any fixups needed**

```bash
git add -A
git commit -m "fix: address test and compatibility issues from unified task system"
```
