# Scheduled Task Refactor Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace ad-hoc `tokio::spawn` periodic tasks with `tokio-cron-scheduler`, adding a `TaskManager` for centralized registration and runtime management.

**Architecture:** Add `tokio-cron-scheduler` to the daemon. Create a `TaskManager` that wraps `JobScheduler` and tracks job metadata. Refactor `daily_notes.rs` to export a `create_*_job()` function. Extract session eviction into its own module. Expose runtime management via `__cmd:tasks` commands.

**Tech Stack:** `tokio-cron-scheduler 0.13`, existing omnish daemon infrastructure.

---

### Task 1: Add `tokio-cron-scheduler` dependency

**Files:**
- Modify: `crates/omnish-daemon/Cargo.toml`

**Step 1: Add dependency**

Add to `[dependencies]`:

```toml
tokio-cron-scheduler = "0.13"
uuid = { workspace = true }
```

(`uuid` is needed because `tokio-cron-scheduler` returns `Uuid` job IDs.)

**Step 2: Verify it compiles**

Run: `cargo check -p omnish-daemon`
Expected: success

**Step 3: Commit**

```bash
git add crates/omnish-daemon/Cargo.toml Cargo.lock
git commit -m "chore: add tokio-cron-scheduler dependency to omnish-daemon"
```

---

### Task 2: Create `TaskManager`

**Files:**
- Create: `crates/omnish-daemon/src/task_mgr.rs`
- Modify: `crates/omnish-daemon/src/lib.rs`

**Step 1: Implement TaskManager**

Create `crates/omnish-daemon/src/task_mgr.rs`:

```rust
use anyhow::Result;
use std::collections::HashMap;
use tokio_cron_scheduler::{Job, JobScheduler};
use uuid::Uuid;

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
        entries.sort_by_key(|(name, _)| name.clone());
        for (name, entry) in entries {
            let status = if entry.enabled { "enabled" } else { "disabled" };
            lines.push(format!("  {} [{}] ({})", name, entry.cron, status));
        }
        lines.join("\n")
    }
}
```

**Step 2: Export in lib.rs**

Add to `crates/omnish-daemon/src/lib.rs`:

```rust
pub mod task_mgr;
```

**Step 3: Verify it compiles**

Run: `cargo check -p omnish-daemon`
Expected: success

**Step 4: Commit**

```bash
git add crates/omnish-daemon/src/task_mgr.rs crates/omnish-daemon/src/lib.rs
git commit -m "feat: add TaskManager for centralized scheduled task management"
```

---

### Task 3: Extract session eviction into its own module

**Files:**
- Create: `crates/omnish-daemon/src/eviction.rs`
- Modify: `crates/omnish-daemon/src/lib.rs`

**Step 1: Create eviction.rs**

Create `crates/omnish-daemon/src/eviction.rs`:

```rust
use crate::session_mgr::SessionManager;
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio_cron_scheduler::Job;

pub fn create_eviction_job(
    mgr: Arc<SessionManager>,
    max_inactive: Duration,
) -> Result<Job> {
    Ok(Job::new_async("0 0 * * * *", move |_uuid, _lock| {
        let mgr = mgr.clone();
        Box::pin(async move {
            mgr.evict_inactive(max_inactive).await;
        })
    })?)
}
```

**Step 2: Export in lib.rs**

Add to `crates/omnish-daemon/src/lib.rs`:

```rust
pub mod eviction;
```

**Step 3: Verify it compiles**

Run: `cargo check -p omnish-daemon`
Expected: success

**Step 4: Commit**

```bash
git add crates/omnish-daemon/src/eviction.rs crates/omnish-daemon/src/lib.rs
git commit -m "feat: extract session eviction into create_eviction_job"
```

---

### Task 4: Refactor `daily_notes.rs` to use `tokio-cron-scheduler`

**Files:**
- Modify: `crates/omnish-daemon/src/daily_notes.rs`

**Step 1: Replace `spawn_daily_notes_task` with `create_daily_notes_job`**

Replace the `spawn_daily_notes_task` and `duration_until_next` functions with:

```rust
use tokio_cron_scheduler::Job;

pub fn create_daily_notes_job(
    mgr: Arc<SessionManager>,
    llm_backend: Option<Arc<dyn LlmBackend>>,
    notes_dir: PathBuf,
    schedule_hour: u8,
) -> anyhow::Result<Job> {
    let cron = format!("0 0 {} * * *", schedule_hour);
    Ok(Job::new_async(cron, move |_uuid, _lock| {
        let mgr = mgr.clone();
        let llm = llm_backend.clone();
        let dir = notes_dir.clone();
        Box::pin(async move {
            if let Err(e) = generate_daily_note(&mgr, llm.as_deref(), &dir).await {
                tracing::warn!("daily notes generation failed: {}", e);
            }
        })
    })?)
}
```

Remove:
- `spawn_daily_notes_task()` function (lines 9-31)
- `duration_until_next()` function (lines 34-46)

Keep:
- `generate_daily_note()` and all tests (update tests that reference removed functions)

**Step 2: Update tests**

Remove `test_duration_until_next_future_today` and `test_duration_until_next_wraps_to_tomorrow` tests since `duration_until_next` is removed. The remaining tests (`test_generate_daily_note_*`) test `generate_daily_note` directly and don't need changes.

**Step 3: Verify tests pass**

Run: `cargo test -p omnish-daemon -- daily_notes`
Expected: PASS (3 remaining tests)

**Step 4: Commit**

```bash
git add crates/omnish-daemon/src/daily_notes.rs
git commit -m "refactor: replace spawn_daily_notes_task with create_daily_notes_job"
```

---

### Task 5: Wire up `TaskManager` in `main.rs`

**Files:**
- Modify: `crates/omnish-daemon/src/main.rs`

**Step 1: Replace ad-hoc task spawning with TaskManager**

Replace lines 63-89 (both task spawn blocks) with:

```rust
    // Set up scheduled task manager
    let mut task_mgr = omnish_daemon::task_mgr::TaskManager::new().await?;

    // Register session eviction job (hourly)
    {
        let max_inactive = std::time::Duration::from_secs(evict_hours * 3600);
        let job = omnish_daemon::eviction::create_eviction_job(
            Arc::clone(&session_mgr),
            max_inactive,
        )?;
        task_mgr.register("eviction", "0 0 * * * *", job).await?;
    }

    // Register daily notes job if enabled
    if daily_notes_config.enabled {
        let notes_dir = omnish_dir().join("notes");
        let cron = format!("0 0 {} * * *", daily_notes_config.schedule_hour);
        let job = omnish_daemon::daily_notes::create_daily_notes_job(
            Arc::clone(&session_mgr),
            llm_backend.clone(),
            notes_dir,
            daily_notes_config.schedule_hour,
        )?;
        task_mgr.register("daily_notes", &cron, job).await?;
        tracing::info!(
            "daily notes enabled (schedule_hour={})",
            daily_notes_config.schedule_hour
        );
    }

    task_mgr.start().await?;
    let task_mgr = Arc::new(tokio::sync::Mutex::new(task_mgr));
```

Also update `DaemonServer::new` call and import to pass `task_mgr`. Remove old import `use omnish_daemon::daily_notes::spawn_daily_notes_task;`.

**Step 2: Verify it compiles**

Run: `cargo check -p omnish-daemon`
Expected: success

**Step 3: Commit**

```bash
git add crates/omnish-daemon/src/main.rs
git commit -m "refactor: wire TaskManager in daemon main, replace ad-hoc task spawning"
```

---

### Task 6: Add `__cmd:tasks` handler to server

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

**Step 1: Add `task_mgr` to `DaemonServer`**

Update `DaemonServer` to accept and store `Arc<tokio::sync::Mutex<TaskManager>>`:

```rust
use omnish_daemon::task_mgr::TaskManager;
use tokio::sync::Mutex;

pub struct DaemonServer {
    session_mgr: Arc<SessionManager>,
    llm_backend: Option<Arc<dyn LlmBackend>>,
    task_mgr: Arc<Mutex<TaskManager>>,
}

impl DaemonServer {
    pub fn new(
        session_mgr: Arc<SessionManager>,
        llm_backend: Option<Arc<dyn LlmBackend>>,
        task_mgr: Arc<Mutex<TaskManager>>,
    ) -> Self {
        Self { session_mgr, llm_backend, task_mgr }
    }
```

Pass `task_mgr` into the `serve` closure and `handle_message`.

**Step 2: Add `tasks` command handler**

In `handle_builtin_command`, add a new match arm:

```rust
sub if sub == "tasks" || sub.starts_with("tasks ") => {
    handle_tasks_command(sub, task_mgr).await
}
```

Add the handler function:

```rust
async fn handle_tasks_command(
    sub: &str,
    task_mgr: &Mutex<TaskManager>,
) -> String {
    let parts: Vec<&str> = sub.split_whitespace().collect();
    match parts.as_slice() {
        ["tasks"] => {
            let mgr = task_mgr.lock().await;
            mgr.format_list()
        }
        ["tasks", "disable", name] => {
            let mut mgr = task_mgr.lock().await;
            match mgr.disable(name) .await {
                Ok(()) => format!("Disabled task '{}'", name),
                Err(e) => format!("Error: {}", e),
            }
        }
        _ => "Usage: tasks [disable <name>]".to_string(),
    }
}
```

**Step 3: Update main.rs `DaemonServer::new` call**

Pass `task_mgr` to `DaemonServer::new()`.

**Step 4: Verify it compiles**

Run: `cargo check -p omnish-daemon`
Expected: success

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/main.rs
git commit -m "feat: add __cmd:tasks handler for runtime task management"
```

---

### Task 7: Full build and test

**Step 1: Run all tests**

Run: `cargo test --workspace`
Expected: all pass

**Step 2: Manual smoke test**

1. `cargo build --workspace`
2. Start daemon: `cargo run -p omnish-daemon`
3. Start client: `cargo run -p omnish-client`
4. In client: type `//tasks` to see scheduled tasks list
5. Verify output shows `eviction` and optionally `daily_notes`

**Step 3: Commit if any fixes needed**
