# Completion Sampling Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Sample "near miss" completion interactions (ignored suggestions similar to what the user actually typed) with full LLM context for offline prompt iteration.

**Architecture:** Buffer the most recent completion's context/prompt/suggestions per session. When the next command arrives, compute edit distance similarity. If criteria met (ignored + similarity > 0.3 + global rate limit), write a rich JSONL record via async writer thread.

**Tech Stack:** Rust, serde_json, tokio::sync::Mutex, std::sync::mpsc, chrono

---

### Task 1: Create `omnish-store/src/sample.rs` — types and edit distance

**Files:**
- Create: `crates/omnish-store/src/sample.rs`
- Modify: `crates/omnish-store/src/lib.rs`

**Step 1: Write the edit distance test**

Add to `crates/omnish-store/src/sample.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein("git status", "git status"), 0);
    }

    #[test]
    fn test_levenshtein_one_edit() {
        assert_eq!(levenshtein("git status", "git statu"), 1);
    }

    #[test]
    fn test_levenshtein_empty() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", ""), 0);
    }

    #[test]
    fn test_similarity_identical() {
        let s = similarity("git status", "git status");
        assert!((s - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_similarity_near_miss() {
        let s = similarity("git status --short", "git status -s");
        // edit distance = 7, max len = 18, similarity = 1 - 7/18 ≈ 0.61
        assert!(s > 0.3 && s < 1.0);
    }

    #[test]
    fn test_similarity_completely_different() {
        let s = similarity("ls -la", "docker compose up");
        assert!(s < 0.3);
    }

    #[test]
    fn test_similarity_empty() {
        assert!((similarity("", "") - 1.0).abs() < f64::EPSILON);
        assert!((similarity("abc", "")).abs() < f64::EPSILON);
    }
}
```

**Step 2: Implement types and edit distance**

Write `crates/omnish-store/src/sample.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

/// A pending sample buffered in the daemon session, waiting for the next command.
#[derive(Debug, Clone)]
pub struct PendingSample {
    pub session_id: String,
    pub context: String,
    pub prompt: String,
    pub suggestions: Vec<String>,
    pub input: String,
    pub cwd: Option<String>,
    pub latency_ms: u64,
    pub accepted: bool,
}

/// A completed sample record written to JSONL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionSample {
    pub recorded_at: String,
    pub session_id: String,
    pub context: String,
    pub prompt: String,
    pub suggestions: Vec<String>,
    pub input: String,
    pub accepted: bool,
    pub next_command: Option<String>,
    pub similarity: Option<f64>,
    pub cwd: Option<String>,
    pub latency_ms: u64,
}

/// Levenshtein edit distance between two strings.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    if m == 0 { return n; }
    if n == 0 { return m; }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Similarity ratio (0.0 = completely different, 1.0 = identical).
pub fn similarity(a: &str, b: &str) -> f64 {
    let max_len = a.len().max(b.len());
    if max_len == 0 { return 1.0; }
    1.0 - (levenshtein(a, b) as f64 / max_len as f64)
}
```

**Step 3: Export module**

In `crates/omnish-store/src/lib.rs`, add:

```rust
pub mod sample;
```

**Step 4: Run tests**

Run: `cargo test -p omnish-store -- sample`
Expected: All 7 tests pass.

**Step 5: Commit**

```bash
git add crates/omnish-store/src/sample.rs crates/omnish-store/src/lib.rs
git commit -m "feat(store): add completion sample types and edit distance (issue #101)"
```

---

### Task 2: Add JSONL sample writer thread

**Files:**
- Modify: `crates/omnish-store/src/sample.rs`

**Step 1: Write the writer test**

Append to the `tests` module in `sample.rs`:

```rust
    #[test]
    fn test_sample_writer() {
        let dir = tempfile::TempDir::new().unwrap();
        let tx = spawn_sample_writer(dir.path().to_path_buf());

        let sample = CompletionSample {
            recorded_at: "2026-03-03T12:00:00".to_string(),
            session_id: "test".to_string(),
            context: "$ ls\nfile.txt".to_string(),
            prompt: "You are a completion engine...".to_string(),
            suggestions: vec!["git status".to_string()],
            input: "git st".to_string(),
            accepted: false,
            next_command: Some("git status -s".to_string()),
            similarity: Some(0.61),
            cwd: Some("/tmp".to_string()),
            latency_ms: 150,
        };
        tx.send(sample).unwrap();
        thread::sleep(std::time::Duration::from_millis(100));
        drop(tx);
        thread::sleep(std::time::Duration::from_millis(100));

        let entries = std::fs::read_dir(dir.path()).unwrap();
        let jsonl_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|ext| ext == "jsonl").unwrap_or(false))
            .collect();
        assert_eq!(jsonl_files.len(), 1);

        let content = std::fs::read_to_string(jsonl_files[0].path()).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: CompletionSample = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.session_id, "test");
        assert_eq!(parsed.next_command.as_deref(), Some("git status -s"));
    }
```

**Step 2: Implement the writer**

Add `spawn_sample_writer` to `sample.rs` (same pattern as `completion::spawn_writer_thread` but writes JSONL instead of CSV):

```rust
/// Spawn a writer thread that writes CompletionSample records to daily-rotated JSONL files.
pub fn spawn_sample_writer(samples_dir: PathBuf) -> mpsc::Sender<CompletionSample> {
    let (tx, rx): (mpsc::Sender<CompletionSample>, mpsc::Receiver<CompletionSample>) =
        mpsc::channel();

    thread::spawn(move || {
        std::fs::create_dir_all(&samples_dir).ok();
        let mut current_date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mut writer: Option<Box<dyn std::io::Write + Send>> = None;

        loop {
            match rx.try_recv() {
                Ok(sample) => {
                    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                    if today != current_date {
                        current_date = today;
                        writer = None;
                    }
                    if writer.is_none() {
                        let path = samples_dir.join(format!("{}.jsonl", current_date));
                        match std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                        {
                            Ok(file) => writer = Some(Box::new(file)),
                            Err(e) => {
                                tracing::error!("Failed to open samples file: {}", e);
                                continue;
                            }
                        }
                    }
                    if let Some(ref mut w) = writer {
                        if let Ok(json) = serde_json::to_string(&sample) {
                            let _ = writeln!(w, "{}", json);
                            let _ = w.flush();
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
    });

    tx
}
```

**Step 3: Run tests**

Run: `cargo test -p omnish-store -- sample`
Expected: All 8 tests pass.

**Step 4: Commit**

```bash
git add crates/omnish-store/src/sample.rs
git commit -m "feat(store): add JSONL sample writer thread (issue #101)"
```

---

### Task 3: Add `pending_sample` to Session and writer to SessionManager

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs`

**Step 1: Add imports and fields**

At top of `session_mgr.rs`, add to imports:
```rust
use omnish_store::sample::{CompletionSample, PendingSample};
```

Add to `Session` struct (line 53-59):
```rust
struct Session {
    dir: PathBuf,
    meta: RwLock<SessionMeta>,
    commands: RwLock<Vec<CommandRecord>>,
    stream_writer: Mutex<StreamWriterState>,
    last_update: Mutex<Option<u64>>,
    pending_sample: Mutex<Option<PendingSample>>,  // NEW
}
```

Add to `SessionManager` struct (line 61-74):
```rust
pub struct SessionManager {
    base_dir: PathBuf,
    sessions: RwLock<HashMap<String, Arc<Session>>>,
    context_config: ContextConfig,
    completion_writer: mpsc::Sender<CompletionRecord>,
    session_writer: mpsc::Sender<SessionUpdateRecord>,
    history_frozen_until: RwLock<Option<u64>>,
    last_completion_context: RwLock<String>,
    sample_writer: mpsc::Sender<CompletionSample>,        // NEW
    last_sample_time: Mutex<Option<Instant>>,              // NEW — global rate limit
}
```

**Step 2: Update `SessionManager::new` (line 123-139)**

Add sample writer initialization:
```rust
let samples_dir = omnish_dir.join("logs").join("samples");
let sample_writer = omnish_store::sample::spawn_sample_writer(samples_dir);
```

Add to the `Self { ... }` block:
```rust
sample_writer,
last_sample_time: Mutex::new(None),
```

**Step 3: Update `register` (line ~270)**

In the `sessions.insert(...)` call, add to Session construction:
```rust
pending_sample: Mutex::new(None),
```

**Step 4: Update session loading (line ~165-207)**

In the `load` closure where Session is constructed (inside `load_existing`), add:
```rust
pending_sample: Mutex::new(None),
```

**Step 5: Build check**

Run: `cargo build -p omnish-daemon`
Expected: Compiles (no new logic yet, just fields).

**Step 6: Commit**

```bash
git add crates/omnish-daemon/src/session_mgr.rs
git commit -m "feat(daemon): add pending_sample and sample_writer fields (issue #101)"
```

---

### Task 4: Add `store_pending_sample` method to SessionManager

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs`

**Step 1: Add method**

Add this method to `impl SessionManager`:

```rust
/// Store a pending completion sample for a session.
/// Called from handle_completion_request after getting LLM suggestions.
pub async fn store_pending_sample(&self, sample: PendingSample) {
    let session = {
        let sessions = self.sessions.read().await;
        sessions.get(&sample.session_id).cloned()
    };
    if let Some(session) = session {
        let mut pending = session.pending_sample.lock().await;
        *pending = Some(sample);
    }
}
```

**Step 2: Add method to update accepted flag**

```rust
/// Update the pending sample's accepted flag when CompletionSummary arrives.
pub async fn update_pending_sample_accepted(&self, session_id: &str, accepted: bool) {
    let session = {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned()
    };
    if let Some(session) = session {
        let mut pending = session.pending_sample.lock().await;
        if let Some(ref mut sample) = *pending {
            sample.accepted = accepted;
        }
    }
}
```

**Step 3: Build check**

Run: `cargo build -p omnish-daemon`
Expected: Compiles.

**Step 4: Commit**

```bash
git add crates/omnish-daemon/src/session_mgr.rs
git commit -m "feat(daemon): add store/update pending sample methods (issue #101)"
```

---

### Task 5: Add sampling logic in `receive_command` and `end_session`

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs`

**Step 1: Add constants**

Near the top of the file, add:

```rust
/// Minimum edit distance similarity to consider a completion a "near miss".
const SAMPLE_SIMILARITY_THRESHOLD: f64 = 0.3;
/// Global rate limit: at most one sample per this many seconds.
const SAMPLE_RATE_LIMIT_SECS: u64 = 300; // 5 minutes
```

**Step 2: Add helper to convert PendingSample → CompletionSample**

```rust
fn pending_to_sample(
    pending: PendingSample,
    next_command: Option<&str>,
    similarity: Option<f64>,
) -> CompletionSample {
    let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    CompletionSample {
        recorded_at: now,
        session_id: pending.session_id,
        context: pending.context,
        prompt: pending.prompt,
        suggestions: pending.suggestions,
        input: pending.input,
        accepted: pending.accepted,
        next_command: next_command.map(|s| s.to_string()),
        similarity,
        cwd: pending.cwd,
        latency_ms: pending.latency_ms,
    }
}
```

**Step 3: Add sampling logic to `receive_command`**

In `receive_command`, after the existing `commands.push(record)` / `CommandRecord::save_all(...)` block (around line 356-359), add:

```rust
            // Check pending sample for completion sampling
            let pending = {
                let mut p = session.pending_sample.lock().await;
                p.take()
            };
            if let Some(pending) = pending {
                let next_cmd = record.command_line.as_deref().unwrap_or("");
                if !pending.accepted && !next_cmd.is_empty() {
                    // Find best similarity across all suggestions
                    let best_sim = pending
                        .suggestions
                        .iter()
                        .map(|s| omnish_store::sample::similarity(s, next_cmd))
                        .fold(0.0_f64, f64::max);

                    if best_sim > SAMPLE_SIMILARITY_THRESHOLD {
                        // Check global rate limit
                        let should_sample = {
                            let mut last = self.last_sample_time.lock().await;
                            let now = Instant::now();
                            let ok = last.map_or(true, |t| {
                                now.duration_since(t).as_secs() >= SAMPLE_RATE_LIMIT_SECS
                            });
                            if ok {
                                *last = Some(now);
                            }
                            ok
                        };
                        if should_sample {
                            let sample = pending_to_sample(pending, Some(next_cmd), Some(best_sim));
                            tracing::info!(
                                "Sampling completion near-miss: sim={:.2}, suggestion={:?}, actual={:?}",
                                best_sim,
                                sample.suggestions.first(),
                                next_cmd
                            );
                            let _ = self.sample_writer.send(sample);
                        }
                    }
                }
            }
```

Note: `record` is used after being pushed to `commands`, so we need to capture `command_line` before the push. Adjust: extract `let next_cmd_line = record.command_line.clone();` before the commands block, then use it after.

**Step 4: Add flush in `end_session`**

In `end_session`, after `CommandRecord::save_all(...)`, add:

```rust
            // Flush any pending sample without next_command
            let pending = {
                let mut p = session.pending_sample.lock().await;
                p.take()
            };
            if let Some(pending) = pending {
                let sample = pending_to_sample(pending, None, None);
                let _ = self.sample_writer.send(sample);
            }
```

**Step 5: Build check**

Run: `cargo build -p omnish-daemon`
Expected: Compiles.

**Step 6: Commit**

```bash
git add crates/omnish-daemon/src/session_mgr.rs
git commit -m "feat(daemon): add completion sampling logic in receive_command/end_session (issue #101)"
```

---

### Task 6: Store PendingSample from `handle_completion_request` and update from CompletionSummary

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

**Step 1: Capture context/prompt/suggestions in handle_completion_request**

In `handle_completion_request` (line 462-548), after `parse_completion_suggestions` succeeds and before returning, store the pending sample. The function returns `Result<Vec<CompletionSuggestion>>` so we need to restructure slightly:

After `let response = result?;` (line 546) and `parse_completion_suggestions(...)` (line 547), before returning:

```rust
    let response = result?;
    let suggestions = parse_completion_suggestions(&response.content)?;

    // Store pending sample for completion sampling (issue #101)
    let suggestion_texts: Vec<String> = suggestions.iter().map(|s| s.text.clone()).collect();
    mgr.store_pending_sample(omnish_store::sample::PendingSample {
        session_id: req.session_id.clone(),
        context,
        prompt,
        suggestions: suggestion_texts,
        input: req.input.clone(),
        cwd: req.cwd.clone(),
        latency_ms: duration.as_millis() as u64,
        accepted: false,
    }).await;

    Ok(suggestions)
```

Note: `context` and `prompt` are local variables already in scope from lines 473 and 493-494.

**Step 2: Update accepted flag from CompletionSummary handler**

In the `Message::CompletionSummary` handler (line 172), before the existing `receive_completion` call, add:

```rust
        Message::CompletionSummary(summary) => {
            // Update pending sample's accepted flag
            mgr.update_pending_sample_accepted(&summary.session_id, summary.accepted).await;
            if let Err(e) = mgr.receive_completion(summary.clone()).await {
```

**Step 3: Build and test**

Run: `cargo build -p omnish-daemon`
Expected: Compiles.

Run: `cargo test -p omnish-daemon`
Expected: Existing tests pass.

**Step 4: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "feat(daemon): capture pending sample in completion request handler (issue #101)"
```

---

### Task 7: Full integration build and test

**Files:** None (verification only)

**Step 1: Build all**

Run: `cargo build`
Expected: Clean build.

**Step 2: Run all tests**

Run: `cargo test`
Expected: All tests pass.

**Step 3: Final commit (if any fixups needed)**

Push and close issue:

```bash
git push
glab issue note 101 -m "Implemented completion sampling..."
glab issue close 101
```
