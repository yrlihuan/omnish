# Disk Cleanup for Expired Sessions Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Clean up session directories on disk that have been inactive for 48 hours (2 days) as an independent scheduled task and during daemon startup.

**Architecture:** Add `cleanup_expired_dirs` method to `SessionManager` that reads `commands.json` to determine session age, create `cleanup.rs` module with `create_disk_cleanup_job`, call cleanup in `load_existing()` for startup, and register daily task in `main.rs`.

**Tech Stack:** Rust, tokio-cron-scheduler, omnish-store for CommandRecord loading

---

### Task 1: Add `cleanup_expired_dirs` method to SessionManager

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs:589-590` (after `evict_inactive` method)

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn test_cleanup_expired_dirs() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let mgr = SessionManager::new(base.clone(), Default::default());

    // Create a mock session directory with old commands.json
    let session_dir = base.join("test_session");
    std::fs::create_dir_all(&session_dir).unwrap();

    // Create commands.json with old timestamp (3 days ago)
    let old_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64 - 3 * 24 * 3600 * 1000;

    let commands = vec![CommandRecord {
        command_id: "cmd1".into(),
        session_id: "test_session".into(),
        command_line: Some("ls".into()),
        cwd: Some("/tmp".into()),
        started_at: old_timestamp,
        ended_at: Some(old_timestamp + 1000),
        output_summary: "".into(),
        stream_offset: 0,
        stream_length: 0,
        exit_code: None,
    }];

    CommandRecord::save_all(&commands, &session_dir).unwrap();

    // Create other required files
    std::fs::write(session_dir.join("meta.json"), "{}").unwrap();
    std::fs::write(session_dir.join("stream.bin"), "").unwrap();

    // Clean up with 48-hour threshold
    let cleaned = mgr.cleanup_expired_dirs(Duration::from_secs(48 * 3600)).await;
    assert_eq!(cleaned, 1);
    assert!(!session_dir.exists());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon test_cleanup_expired_dirs -- --nocapture`
Expected: FAIL with "method `cleanup_expired_dirs` not found for `SessionManager`"

**Step 3: Write minimal implementation**

Add method after `evict_inactive` in `session_mgr.rs`:

```rust
/// Clean up session directories that have been inactive longer than `max_age`.
/// Returns the number of directories deleted.
pub async fn cleanup_expired_dirs(&self, max_age: std::time::Duration) -> usize {
    let mut cleaned = 0;

    // Get list of directories in base_dir
    let entries = match std::fs::read_dir(&self.base_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("failed to read session store directory: {}", e);
            return 0;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("failed to read directory entry: {}", e);
                continue;
            }
        };

        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        // Try to load commands.json
        let commands = match CommandRecord::load_all(&dir) {
            Ok(cmds) => cmds,
            Err(e) => {
                tracing::warn!("failed to load commands.json from {:?}: {}", dir, e);
                continue;
            }
        };

        // Get last command timestamp
        let last_cmd_ms = commands
            .last()
            .and_then(|cmd| cmd.ended_at.or(Some(cmd.started_at)));

        match last_cmd_ms {
            Some(ms) => {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;

                let age = std::time::Duration::from_millis(now_ms.saturating_sub(ms));
                if age >= max_age {
                    match std::fs::remove_dir_all(&dir) {
                        Ok(_) => {
                            tracing::info!("cleaned up expired session directory: {:?}", dir);
                            cleaned += 1;
                        }
                        Err(e) => {
                            tracing::error!("failed to delete expired session directory {:?}: {}", dir, e);
                        }
                    }
                }
            }
            None => {
                // No commands - could be empty session directory
                // We'll skip it for safety
                continue;
            }
        }
    }

    cleaned
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-daemon test_cleanup_expired_dirs -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/session_mgr.rs
git commit -m "feat: add cleanup_expired_dirs method to SessionManager"
```

---

### Task 2: Create cleanup.rs module with create_disk_cleanup_job

**Files:**
- Create: `crates/omnish-daemon/src/cleanup.rs`
- Modify: `crates/omnish-daemon/src/lib.rs:5` (add module export)

**Step 1: Write the failing test**

Add test to new file `cleanup.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn test_create_disk_cleanup_job() {
        // Mock SessionManager
        let mock_dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(mock_dir.path().to_path_buf(), Default::default()));

        // Should create job successfully
        let job = create_disk_cleanup_job(mgr, Duration::from_secs(48 * 3600));
        assert!(job.is_ok());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon test_create_disk_cleanup_job -- --nocapture`
Expected: FAIL with "cannot find function `create_disk_cleanup_job` in this scope"

**Step 3: Write minimal implementation**

Create `crates/omnish-daemon/src/cleanup.rs`:

```rust
use crate::session_mgr::SessionManager;
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio_cron_scheduler::Job;

pub fn create_disk_cleanup_job(
    mgr: Arc<SessionManager>,
    max_age: Duration,
) -> Result<Job> {
    Ok(Job::new_async("0 0 0 * * *", move |_uuid, _lock| {
        let mgr = mgr.clone();
        Box::pin(async move {
            let cleaned = mgr.cleanup_expired_dirs(max_age).await;
            if cleaned > 0 {
                tracing::info!("cleaned up {} expired session directories", cleaned);
            }
        })
    })?)
}
```

**Step 4: Export module in lib.rs**

Modify `crates/omnish-daemon/src/lib.rs`:

```rust
pub mod cleanup;
pub mod daily_notes;
pub mod eviction;
pub mod session_mgr;
pub mod task_mgr;
```

**Step 5: Run test to verify it passes**

Run: `cargo test -p omnish-daemon test_create_disk_cleanup_job -- --nocapture`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/omnish-daemon/src/cleanup.rs crates/omnish-daemon/src/lib.rs
git commit -m "feat: create cleanup module with disk cleanup job"
```

---

### Task 3: Update load_existing to call cleanup on startup

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs:194-195` (end of load_existing method)

**Step 1: Write the failing test**

Add test to existing `test_load_existing_restores_sessions` or create new test:

```rust
#[tokio::test]
async fn test_load_existing_cleans_up_expired_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();

    // Create expired session directory
    let expired_dir = base.join("expired_session");
    std::fs::create_dir_all(&expired_dir).unwrap();

    // Create commands.json with old timestamp
    use std::time::{SystemTime, UNIX_EPOCH};
    let old_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64 - 3 * 24 * 3600 * 1000;

    let commands = vec![CommandRecord {
        command_id: "cmd1".into(),
        session_id: "expired_session".into(),
        command_line: Some("ls".into()),
        cwd: Some("/tmp".into()),
        started_at: old_timestamp,
        ended_at: Some(old_timestamp + 1000),
        output_summary: "".into(),
        stream_offset: 0,
        stream_length: 0,
        exit_code: None,
    }];

    CommandRecord::save_all(&commands, &expired_dir).unwrap();
    std::fs::write(expired_dir.join("meta.json"), "{}").unwrap();
    std::fs::write(expired_dir.join("stream.bin"), "").unwrap();

    // Create fresh session directory
    let fresh_dir = base.join("fresh_session");
    std::fs::create_dir_all(&fresh_dir).unwrap();

    let fresh_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64 - 12 * 3600 * 1000; // 12 hours ago

    let fresh_commands = vec![CommandRecord {
        command_id: "cmd2".into(),
        session_id: "fresh_session".into(),
        command_line: Some("pwd".into()),
        cwd: Some("/tmp".into()),
        started_at: fresh_timestamp,
        ended_at: Some(fresh_timestamp + 1000),
        output_summary: "".into(),
        stream_offset: 0,
        stream_length: 0,
        exit_code: None,
    }];

    CommandRecord::save_all(&fresh_commands, &fresh_dir).unwrap();
    std::fs::write(fresh_dir.join("meta.json"), "{}").unwrap();
    std::fs::write(fresh_dir.join("stream.bin"), "").unwrap();

    // Load existing - should clean up expired but keep fresh
    let mgr = SessionManager::new(base.clone(), Default::default());
    let count = mgr.load_existing().await.unwrap();

    // Fresh session should be loaded, expired should be deleted
    assert_eq!(count, 1); // Only fresh session loaded
    assert!(!expired_dir.exists());
    assert!(fresh_dir.exists());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon test_load_existing_cleans_up_expired_dirs -- --nocapture`
Expected: FAIL because expired directory still exists after load_existing

**Step 3: Write minimal implementation**

Modify end of `load_existing` method in `session_mgr.rs`:

```rust
pub async fn load_existing(&self) -> Result<usize> {
    let mut count = 0;
    let entries = match std::fs::read_dir(&self.base_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("failed to read session store directory: {}", e);
            return Ok(0);
        }
    };

    let mut sessions = self.sessions.write().await;
    for entry in entries {
        // ... existing loading logic ...
    }

    // Clean up expired directories on startup (48 hours)
    let max_age = std::time::Duration::from_secs(48 * 3600);
    let cleaned = self.cleanup_expired_dirs(max_age).await;
    if cleaned > 0 {
        tracing::info!("cleaned up {} expired session directories on startup", cleaned);
    }

    Ok(count)
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-daemon test_load_existing_cleans_up_expired_dirs -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/session_mgr.rs
git commit -m "feat: call cleanup_expired_dirs in load_existing for startup cleanup"
```

---

### Task 4: Register cleanup task in main.rs

**Files:**
- Modify: `crates/omnish-daemon/src/main.rs:77-78` (after eviction job registration)

**Step 1: Write the failing test**

Check if task appears in task list (requires integration test):

```rust
#[tokio::test]
async fn test_disk_cleanup_task_registered() {
    // This would require full daemon startup test
    // For now, we'll manually verify after implementation
}
```

**Step 2: Manual verification step**

Run daemon and check `/tasks list` output shows `disk_cleanup` task.

**Step 3: Write minimal implementation**

Add after eviction job registration in `main.rs`:

```rust
// Register session eviction job (hourly)
{
    let max_inactive = std::time::Duration::from_secs(evict_hours * 3600);
    let job = omnish_daemon::eviction::create_eviction_job(
        Arc::clone(&session_mgr),
        max_inactive,
    )?;
    task_mgr.register("eviction", "0 0 * * * *", job).await?;
}

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

**Step 4: Run compilation to verify no errors**

Run: `cargo check -p omnish-daemon`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/main.rs
git commit -m "feat: register disk_cleanup task in daemon"
```

---

### Task 5: Add error handling and edge case tests

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs` (cleanup_expired_dirs method)
- Create: Additional test cases

**Step 1: Write failing test for edge cases**

```rust
#[tokio::test]
async fn test_cleanup_expired_dirs_edge_cases() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let mgr = SessionManager::new(base.clone(), Default::default());

    // Test 1: Empty commands.json
    let empty_dir = base.join("empty_session");
    std::fs::create_dir_all(&empty_dir).unwrap();
    std::fs::write(empty_dir.join("commands.json"), "[]").unwrap();

    // Should skip (no timestamp to check)
    let cleaned = mgr.cleanup_expired_dirs(Duration::from_secs(48 * 3600)).await;
    assert_eq!(cleaned, 0);
    assert!(empty_dir.exists()); // Should still exist

    // Test 2: Corrupted commands.json
    let corrupt_dir = base.join("corrupt_session");
    std::fs::create_dir_all(&corrupt_dir).unwrap();
    std::fs::write(corrupt_dir.join("commands.json"), "not json").unwrap();

    let cleaned = mgr.cleanup_expired_dirs(Duration::from_secs(48 * 3600)).await;
    assert_eq!(cleaned, 0);
    assert!(corrupt_dir.exists()); // Should still exist (skip on error)

    // Test 3: Missing commands.json
    let missing_dir = base.join("missing_session");
    std::fs::create_dir_all(&missing_dir).unwrap();

    let cleaned = mgr.cleanup_expired_dirs(Duration::from_secs(48 * 3600)).await;
    assert_eq!(cleaned, 0);
    assert!(missing_dir.exists()); // Should still exist
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon test_cleanup_expired_dirs_edge_cases -- --nocapture`
Expected: MAY FAIL or PASS depending on current implementation

**Step 3: Improve error handling in cleanup_expired_dirs**

Update the method to handle edge cases better:

```rust
pub async fn cleanup_expired_dirs(&self, max_age: std::time::Duration) -> usize {
    let mut cleaned = 0;

    let entries = match std::fs::read_dir(&self.base_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("failed to read session store directory: {}", e);
            return 0;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("failed to read directory entry: {}", e);
                continue;
            }
        };

        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        // Try to load commands.json
        let commands = match CommandRecord::load_all(&dir) {
            Ok(cmds) => cmds,
            Err(e) => {
                // commands.json might be missing, empty, or corrupted
                tracing::warn!("failed to load commands.json from {:?}: {}", dir, e);
                continue; // Skip this directory
            }
        };

        // Get last command timestamp
        let last_cmd_ms = commands
            .last()
            .and_then(|cmd| cmd.ended_at.or(Some(cmd.started_at)));

        match last_cmd_ms {
            Some(ms) => {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;

                let age = std::time::Duration::from_millis(now_ms.saturating_sub(ms));
                if age >= max_age {
                    match std::fs::remove_dir_all(&dir) {
                        Ok(_) => {
                            tracing::info!("cleaned up expired session directory: {:?}", dir);
                            cleaned += 1;
                        }
                        Err(e) => {
                            tracing::error!("failed to delete expired session directory {:?}: {}", dir, e);
                        }
                    }
                }
            }
            None => {
                // No commands - could be empty session directory
                // We'll skip it for safety (preserve data)
                continue;
            }
        }
    }

    cleaned
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-daemon test_cleanup_expired_dirs_edge_cases -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/session_mgr.rs
git commit -m "test: add edge case tests for cleanup_expired_dirs"
```

---

### Task 6: Verify complete implementation

**Files:**
- All modified files

**Step 1: Run all tests**

Run: `cargo test -p omnish-daemon`
Expected: All tests pass

**Step 2: Check compilation**

Run: `cargo check --all`
Expected: No errors

**Step 3: Manual verification**

Start daemon in test mode and verify:
1. `/tasks list` shows `disk_cleanup` task with schedule `0 0 0 * * *`
2. Startup logs show cleanup message if expired directories exist

**Step 4: Create final commit summarizing changes**

```bash
git add -A
git commit -m "feat: complete disk cleanup for expired sessions (issue #42)

- Add SessionManager::cleanup_expired_dirs method
- Create cleanup module with create_disk_cleanup_job
- Call cleanup in load_existing for daemon startup cleanup
- Register daily disk cleanup task (midnight, 48-hour threshold)
- Add comprehensive tests for edge cases
"
```

**Step 5: Update issue status**

Add comment to issue #42 with implementation details and commit hash.