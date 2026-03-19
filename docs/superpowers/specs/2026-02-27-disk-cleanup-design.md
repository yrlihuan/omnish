# Disk Cleanup for Expired Sessions Design

**Issue:** #42 — 在eviction任务中，同时清理掉磁盘上过期的sessions信息

## Overview

Currently, the eviction task only removes inactive sessions from memory but leaves their data on disk (stream.bin, commands.json, meta.json files). This leads to unbounded disk growth over time. This design adds disk cleanup functionality that:

1. Deletes entire session directories that have been inactive for 48 hours (2 days)
2. Runs as an independent scheduled task (daily)
3. Also executes once during daemon startup
4. Uses the last command timestamp from commands.json to determine session age

## Components

### 1. SessionManager Cleanup Method

Add a new method to `SessionManager`:

```rust
/// Clean up session directories that have been inactive longer than `max_age`.
/// Returns the number of directories deleted.
pub async fn cleanup_expired_dirs(&self, max_age: std::time::Duration) -> usize
```

**Algorithm:**
1. Iterate over all subdirectories in `self.base_dir`
2. For each session directory:
   - Load `commands.json` using `CommandRecord::load_all(&dir)`
   - Find the most recent command's `ended_at` or `started_at` timestamp
   - Convert timestamp to `Instant` and compare with current time
   - If older than `max_age`, delete the entire directory with `std::fs::remove_dir_all(&dir)`
3. Skip directories with errors (log warning and continue)
4. Return count of successfully deleted directories

### 2. Cleanup Module

Create new module `crates/omnish-daemon/src/cleanup.rs`:

```rust
pub fn create_disk_cleanup_job(
    mgr: Arc<SessionManager>,
    max_age: std::time::Duration,
) -> Result<Job>
```

Similar to existing `eviction::create_eviction_job`, creates a tokio-cron-scheduler Job that calls `mgr.cleanup_expired_dirs(max_age)`.

### 3. Daemon Startup Cleanup

Modify `SessionManager::load_existing()` to include initial cleanup:

```rust
pub async fn load_existing(&self) -> Result<usize> {
    // ... existing loading logic ...

    // Clean up expired directories on startup
    let max_age = std::time::Duration::from_secs(48 * 3600); // 48 hours
    let cleaned = self.cleanup_expired_dirs(max_age).await;
    if cleaned > 0 {
        tracing::info!("cleaned up {} expired session directories on startup", cleaned);
    }

    Ok(count)
}
```

### 4. Task Registration

Register cleanup job in `main.rs` with daily schedule:

```rust
// Register disk cleanup job (daily at midnight)
{
    let max_age = std::time::Duration::from_secs(48 * 3600);
    let job = omnish_daemon::cleanup::create_disk_cleanup_job(
        Arc::clone(&session_mgr),
        max_age,
    )?;
    task_mgr.register("disk_cleanup", "0 0 0 * * *", job).await?;
}
```

## Dependencies

- No new external dependencies
- Uses existing `omnish-store::command::CommandRecord` for loading commands.json
- Uses `tokio-cron-scheduler` (already in use)

## Data Flow

```
Daemon Startup                  Daily Scheduled Task
────────────────────────────────────────────────────
SessionManager::load_existing()  ┌─→ tokio-cron-scheduler
            │                    │        │
            ├─→ cleanup_expired_dirs(48h) │
            │                    │        │
            ├─→ Delete old dirs  │        │
            │                    │        │
            └─→ Log result       │        │
                                 │        │
                                 │  create_disk_cleanup_job()
                                 │        │
                                 │  cleanup_expired_dirs(48h)
                                 │        │
                                 └────────┘ Delete old dirs
                                            Log result
```

## Error Handling

- **Individual directory errors:** Log warning with `tracing::warn!()` and continue processing other directories
- **JSON parsing errors:** If `commands.json` is corrupted, log warning and skip the directory (preserve data rather than risk deleting active session)
- **Directory deletion errors:** Log error with `tracing::error!()` but continue processing
- **Missing commands.json:** Treat as empty command list (use directory creation time as fallback)

## Testing

### Unit Tests
1. **Mock session directories:** Create temporary directories with mock `commands.json` files
2. **Age calculation:** Test timestamp parsing and age comparison logic
3. **Error scenarios:** Test handling of corrupted JSON, missing files, permission errors

### Integration Tests
1. **Task registration:** Verify cleanup job is correctly registered in `TaskManager`
2. **Schedule verification:** Confirm daily schedule (`0 0 0 * * *`)
3. **Startup cleanup:** Test that cleanup runs during daemon startup

### Manual Testing
1. Create test session directories with varying last command times
2. Verify that directories older than 48 hours are deleted
3. Verify that active directories (within 48 hours) are preserved

## Configuration

- **Time threshold:** Hardcoded to 48 hours (2 days) based on issue requirements
- **Schedule:** Daily at midnight (`0 0 0 * * *`)
- **No user-configurable parameters** in initial implementation (can be added later if needed)

## Alternatives Considered

1. **Integrate with eviction task:** Rejected because disk cleanup has different frequency (daily vs hourly) and criteria (48h vs 1h)
2. **Use directory name timestamp:** Rejected because session directories don't get renamed on reconnection, making directory creation time inaccurate
3. **Partial cleanup (stream.bin only):** Rejected because issue specifies cleaning up entire session information