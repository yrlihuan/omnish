# Daemon Scheduled Task Refactor Design

**Issue:** #32 — Refactor daemon scheduled tasks, standardize task interface to support multiple task types

## Overview

Replace ad-hoc `tokio::spawn` patterns for periodic tasks with `tokio-cron-scheduler`. Introduce a `TaskManager` that wraps `JobScheduler` for centralized registration, lifecycle management, and runtime control.

## Current State

Two periodic tasks spawned ad-hoc in `main.rs`:

1. **Session eviction** — inline `tokio::spawn` with `tokio::time::interval(1 hour)`, calls `mgr.evict_inactive()`
2. **Daily notes** — `spawn_daily_notes_task()` with manual `duration_until_next()` + 24h sleep loop

No shared interface, no centralized registry, no runtime management.

## Design

### 1. Core: `tokio-cron-scheduler` Integration

Add `tokio-cron-scheduler` dependency to `omnish-daemon`. Use `JobScheduler` as the scheduling engine.

**Schedule mapping:**
- Session eviction: `"0 0 * * * *"` (hourly at :00)
- Daily notes: `"0 0 {hour} * * *"` (daily at configured hour)

### 2. Task Wrapper Pattern

Each task module exports a `create_*_job()` function returning a `Job`:

```rust
// daily_notes.rs
pub fn create_daily_notes_job(
    mgr: Arc<SessionManager>,
    llm: Option<Arc<dyn LlmBackend>>,
    notes_dir: PathBuf,
    schedule_hour: u8,
) -> Result<Job> {
    let cron = format!("0 0 {} * * *", schedule_hour);
    Job::new_async(cron, move |_uuid, _lock| {
        let mgr = mgr.clone();
        let llm = llm.clone();
        let dir = notes_dir.clone();
        Box::pin(async move {
            if let Err(e) = generate_daily_note(&mgr, llm.as_deref(), &dir).await {
                tracing::warn!("daily notes failed: {}", e);
            }
        })
    })
}
```

Task logic (`generate_daily_note`, `evict_inactive`) remains unchanged. Only the scheduling wrapper changes.

### 3. TaskManager

```rust
pub struct TaskManager {
    scheduler: JobScheduler,
    jobs: HashMap<String, TaskEntry>,
}

struct TaskEntry {
    uuid: Uuid,
    cron: String,
    enabled: bool,
}
```

**Methods:**
- `new()` — create scheduler
- `register(name, job, cron)` — add job to scheduler, track metadata
- `start()` — start scheduler
- `list()` — return task names, schedules, enabled status
- `disable(name)` — remove job from scheduler, mark disabled
- `enable(name)` — re-create and add job, mark enabled

### 4. Runtime Management

Exposed via existing `__cmd:` request path in `server.rs`:
- `__cmd:tasks` — list all registered tasks
- `__cmd:tasks enable <name>` — re-enable a disabled task
- `__cmd:tasks disable <name>` — disable a running task

### 5. Module Changes

- `daily_notes.rs` — replace `spawn_daily_notes_task()` with `create_daily_notes_job()`; remove `duration_until_next()` (cron handles scheduling); keep `generate_daily_note()` and helpers
- New: `eviction.rs` — extract session eviction into `create_eviction_job()`
- New: `task_mgr.rs` — `TaskManager` struct
- `main.rs` — create `TaskManager`, register jobs, pass to `DaemonServer`
- `server.rs` — add `__cmd:tasks` handler, store `TaskManager` reference
- `lib.rs` — export new modules

### 6. Dependencies

Add to `omnish-daemon/Cargo.toml`:
```toml
tokio-cron-scheduler = "0.13"
```
