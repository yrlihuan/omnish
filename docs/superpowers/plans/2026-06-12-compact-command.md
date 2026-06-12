# /compact Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the `/compact` chat command (issue #641) that truncates bulky tool_result blocks in a thread's stored history, with a pre-compact snapshot for later lookup.

**Architecture:** Pure compaction logic lives in a new `compact.rs` module in omnish-daemon. `ConversationManager::compact_thread` wraps it with snapshot + persistence. The daemon exposes it via the existing `__cmd:` builtin command path; the client adds a `/compact` chat-mode handler following the `/thread sandbox` pattern.

**Tech Stack:** Rust, serde_json, existing omnish daemon/client crates.

**Spec:** `docs/superpowers/specs/2026-06-12-compact-design.md`

**Build rule (from CLAUDE.md):** ALWAYS use release build: `cargo build --release`, `cargo test --release`. NEVER use em-dash in any code, comment, string, or doc. Never run omnish-daemon directly; ask the user.

---

## File Structure

- Create: `crates/omnish-daemon/src/compact.rs` - pure compaction functions + constants + `CompactStats` + unit tests
- Modify: `crates/omnish-daemon/src/lib.rs` - register `pub mod compact;`
- Modify: `crates/omnish-daemon/src/conversation_mgr.rs` - add `compact_thread()`, extend `delete_thread()` to remove snapshots
- Modify: `crates/omnish-daemon/src/server.rs` - add `compact:<tid>` branch in `handle_builtin_command` (around line 2778, before the `thread rename` branch)
- Modify: `crates/omnish-client/src/chat_session.rs` - add `/compact` dispatch (after the `/thread rename` block around line 1461) + `handle_compact()` method
- Modify: `crates/omnish-client/src/command.rs` - add `/compact` to `CHAT_ONLY_COMMANDS` and help output
- Modify: `crates/omnish-client/src/i18n/*.json` (8 files) - add `command.help.compact` key

---

### Task 1: Pure compaction logic (`compact.rs`)

**Files:**
- Create: `crates/omnish-daemon/src/compact.rs`
- Modify: `crates/omnish-daemon/src/lib.rs`

- [ ] **Step 1: Register the module**

In `crates/omnish-daemon/src/lib.rs`, add alongside the existing module declarations (next to `pub mod conversation_mgr;`):

```rust
pub mod compact;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/omnish-daemon/src/compact.rs` with constants, types, function stubs, and tests. Stubs first so the test code compiles but fails:

```rust
//! Compaction of bulky tool_result blocks in stored chat history (issue #641).
//!
//! Pure functions only; snapshot + persistence live in ConversationManager.

/// Recent rounds (real user inputs counted from the tail) whose tool results
/// are never compacted.
pub const PROTECTED_ROUNDS: usize = 2;
/// Only compact tool_result texts longer than this many lines.
pub const MIN_LINES: usize = 30;
pub const HEAD_LINES: usize = 10;
pub const TAIL_LINES: usize = 10;
/// Marker embedded in compacted text. Also serves as the idempotency flag:
/// texts containing it are never compacted again.
pub const COMPACT_MARKER: &str = "[compacted:";

#[derive(Debug, Default, Clone, Copy)]
pub struct CompactStats {
    /// Number of tool_result blocks compacted.
    pub blocks: usize,
    /// Total chars of those blocks before compaction.
    pub chars_before: usize,
    /// Total chars of those blocks after compaction.
    pub chars_after: usize,
}

/// Compact bulky tool_result blocks in-place. Returns stats; `blocks == 0`
/// means nothing changed.
pub fn compact_messages(msgs: &mut [serde_json::Value]) -> CompactStats {
    todo!()
}
```

Append the tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn many_lines(n: usize) -> String {
        (1..=n).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n")
    }

    fn user_text(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": text})
    }

    fn assistant_tool_use(id: &str) -> serde_json::Value {
        serde_json::json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": id, "name": "bash", "input": {}}
        ]})
    }

    /// tool_result whose content is a plain string.
    fn user_tool_result_str(id: &str, content: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": content}
        ]})
    }

    /// tool_result whose content is an array of text blocks.
    fn user_tool_result_arr(id: &str, content: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": [
                {"type": "text", "text": content}
            ]}
        ]})
    }

    fn assistant_text(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "assistant", "content": [
            {"type": "text", "text": text}
        ]})
    }

    /// Build a thread with `rounds` rounds; each round is:
    /// user text, assistant tool_use, user tool_result (big), assistant text.
    fn build_thread(rounds: usize, string_content: bool) -> Vec<serde_json::Value> {
        let mut msgs = Vec::new();
        for r in 0..rounds {
            let id = format!("tu_{}", r);
            msgs.push(user_text(&format!("question {}", r)));
            msgs.push(assistant_tool_use(&id));
            let big = many_lines(50);
            msgs.push(if string_content {
                user_tool_result_str(&id, &big)
            } else {
                user_tool_result_arr(&id, &big)
            });
            msgs.push(assistant_text(&format!("answer {}", r)));
        }
        msgs
    }

    fn result_text(msg: &serde_json::Value) -> String {
        let block = &msg["content"][0];
        match &block["content"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(arr) => arr[0]["text"].as_str().unwrap().to_string(),
            _ => panic!("unexpected content"),
        }
    }

    #[test]
    fn compacts_string_content_outside_protected_window() {
        // 3 rounds: round 0 is outside the protected window (last 2 rounds).
        let mut msgs = build_thread(3, true);
        let stats = compact_messages(&mut msgs);
        assert_eq!(stats.blocks, 1);
        assert!(stats.chars_before > stats.chars_after);
        let compacted = result_text(&msgs[2]);
        assert!(compacted.contains(COMPACT_MARKER));
        assert!(compacted.starts_with("line 1\n"));
        assert!(compacted.ends_with("line 50"));
        // Protected rounds untouched
        assert!(!result_text(&msgs[6]).contains(COMPACT_MARKER));
        assert!(!result_text(&msgs[10]).contains(COMPACT_MARKER));
    }

    #[test]
    fn compacts_array_content() {
        let mut msgs = build_thread(3, false);
        let stats = compact_messages(&mut msgs);
        assert_eq!(stats.blocks, 1);
        assert!(result_text(&msgs[2]).contains(COMPACT_MARKER));
    }

    #[test]
    fn protects_recent_rounds_when_thread_is_short() {
        // Only 2 rounds total: everything is inside the protected window.
        let mut msgs = build_thread(2, true);
        let stats = compact_messages(&mut msgs);
        assert_eq!(stats.blocks, 0);
        assert!(!result_text(&msgs[2]).contains(COMPACT_MARKER));
    }

    #[test]
    fn line_threshold_boundary() {
        // 30 lines: not compacted. 31 lines: compacted.
        let id30 = "tu_30";
        let id31 = "tu_31";
        let mut msgs = vec![
            user_text("q0"),
            assistant_tool_use(id30),
            user_tool_result_str(id30, &many_lines(30)),
            assistant_text("a0"),
            user_text("q1"),
            assistant_tool_use(id31),
            user_tool_result_str(id31, &many_lines(31)),
            assistant_text("a1"),
            // Two more rounds so both results above are unprotected
            user_text("q2"),
            assistant_text("a2"),
            user_text("q3"),
            assistant_text("a3"),
        ];
        let stats = compact_messages(&mut msgs);
        assert_eq!(stats.blocks, 1);
        assert!(!result_text(&msgs[2]).contains(COMPACT_MARKER));
        assert!(result_text(&msgs[6]).contains(COMPACT_MARKER));
    }

    #[test]
    fn idempotent_on_second_run() {
        let mut msgs = build_thread(3, true);
        let first = compact_messages(&mut msgs);
        assert_eq!(first.blocks, 1);
        let snapshot = msgs.clone();
        let second = compact_messages(&mut msgs);
        assert_eq!(second.blocks, 0);
        assert_eq!(msgs, snapshot);
    }

    #[test]
    fn ignores_non_tool_result_blocks_and_string_user_messages() {
        let big = many_lines(50);
        let mut msgs = vec![
            // Plain string user message with many lines: not a tool_result
            user_text(&big),
            assistant_text("a0"),
            user_text("q1"),
            assistant_text("a1"),
            user_text("q2"),
            assistant_text("a2"),
        ];
        let stats = compact_messages(&mut msgs);
        assert_eq!(stats.blocks, 0);
        assert_eq!(msgs[0]["content"].as_str().unwrap(), big);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --release -p omnish-daemon compact:: 2>&1 | tail -20`
Expected: panics with `not yet implemented` (todo!) in all tests that reach `compact_messages`.

- [ ] **Step 4: Implement the logic**

Replace the `compact_messages` stub with:

```rust
/// True for a real user input message (has text content), as opposed to a
/// tool_result-only user message. Mirrors ConversationManager::is_user_input.
fn is_real_user_input(msg: &serde_json::Value) -> bool {
    if msg["role"].as_str() != Some("user") {
        return false;
    }
    match &msg["content"] {
        serde_json::Value::String(_) => true,
        serde_json::Value::Array(arr) => {
            arr.iter().any(|b| b["type"].as_str() == Some("text"))
        }
        _ => false,
    }
}

/// Index of the first message in the protected window: everything from the
/// PROTECTED_ROUNDS-th real user input (counted from the tail) onward.
/// Returns 0 when the thread has fewer rounds than the window.
fn protect_boundary(msgs: &[serde_json::Value]) -> usize {
    let mut rounds = 0;
    for (i, m) in msgs.iter().enumerate().rev() {
        if is_real_user_input(m) {
            rounds += 1;
            if rounds == PROTECTED_ROUNDS {
                return i;
            }
        }
    }
    0
}

/// Truncate `text` to head + marker + tail. None when below threshold or
/// already compacted.
fn compact_text(text: &str) -> Option<String> {
    if text.contains(COMPACT_MARKER) {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MIN_LINES {
        return None;
    }
    let head = lines[..HEAD_LINES].join("\n");
    let tail = lines[lines.len() - TAIL_LINES..].join("\n");
    let omitted_lines = lines.len() - HEAD_LINES - TAIL_LINES;
    let omitted_chars = text.len().saturating_sub(head.len() + tail.len());
    Some(format!(
        "{}\n... {} omitted {} lines, {} chars] ...\n{}",
        head, COMPACT_MARKER, omitted_lines, omitted_chars, tail
    ))
}

fn compact_tool_result(block: &mut serde_json::Value, stats: &mut CompactStats) {
    let mut chars_before = 0;
    let mut chars_after = 0;
    let mut changed = false;
    match &mut block["content"] {
        serde_json::Value::String(s) => {
            if let Some(c) = compact_text(s) {
                chars_before += s.len();
                chars_after += c.len();
                *s = c;
                changed = true;
            }
        }
        serde_json::Value::Array(arr) => {
            for inner in arr.iter_mut() {
                if inner["type"].as_str() != Some("text") {
                    continue;
                }
                if let serde_json::Value::String(s) = &mut inner["text"] {
                    if let Some(c) = compact_text(s) {
                        chars_before += s.len();
                        chars_after += c.len();
                        *s = c;
                        changed = true;
                    }
                }
            }
        }
        _ => {}
    }
    if changed {
        stats.blocks += 1;
        stats.chars_before += chars_before;
        stats.chars_after += chars_after;
    }
}

/// Compact bulky tool_result blocks in-place. Returns stats; `blocks == 0`
/// means nothing changed.
pub fn compact_messages(msgs: &mut [serde_json::Value]) -> CompactStats {
    let mut stats = CompactStats::default();
    let boundary = protect_boundary(msgs);
    for msg in &mut msgs[..boundary] {
        if msg["role"].as_str() != Some("user") {
            continue;
        }
        let Some(blocks) = msg["content"].as_array_mut() else {
            continue;
        };
        for block in blocks {
            if block["type"].as_str() == Some("tool_result") {
                compact_tool_result(block, &mut stats);
            }
        }
    }
    stats
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --release -p omnish-daemon compact:: 2>&1 | tail -10`
Expected: all 6 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-daemon/src/compact.rs crates/omnish-daemon/src/lib.rs
git commit -m "feat(daemon): add pure tool_result compaction logic (#641)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: `ConversationManager::compact_thread` with snapshot

**Files:**
- Modify: `crates/omnish-daemon/src/conversation_mgr.rs` (add method near `replace_messages` at ~line 567; extend `delete_thread` at ~line 512; tests at the bottom tests module)

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` in `conversation_mgr.rs` (it already has `user_msg`/`assistant_msg` helpers and uses `tempfile`):

```rust
    fn big_tool_round(id: &str) -> Vec<serde_json::Value> {
        let big = (1..=50).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        vec![
            serde_json::json!({"role": "user", "content": "run it"}),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": id, "name": "bash", "input": {}}
            ]}),
            serde_json::json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": big}
            ]}),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "text", "text": "done"}
            ]}),
        ]
    }

    fn snapshot_files(dir: &std::path::Path, thread_id: &str) -> Vec<std::path::PathBuf> {
        let snap_dir = dir.join("precompact");
        match std::fs::read_dir(&snap_dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(&format!("{}.", thread_id))
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    #[test]
    fn test_compact_thread_writes_snapshot_and_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        // 3 rounds; the first is outside the protected window
        for r in 0..3 {
            mgr.append_messages(&id, &big_tool_round(&format!("tu_{}", r)));
        }
        let original = mgr.load_raw_messages(&id);

        let stats = mgr.compact_thread(&id).unwrap().expect("should compact");
        assert_eq!(stats.blocks, 1);

        // In-memory and on-disk both rewritten
        let after = mgr.load_raw_messages(&id);
        assert_ne!(original, after);
        let compacted_text = after[2]["content"][0]["content"].as_str().unwrap();
        assert!(compacted_text.contains(crate::compact::COMPACT_MARKER));
        let mgr2 = ConversationManager::new(dir.path().to_path_buf());
        assert_eq!(mgr2.load_raw_messages(&id), after);

        // Snapshot exists and contains the original messages
        let snaps = snapshot_files(dir.path(), &id);
        assert_eq!(snaps.len(), 1);
        let snap_content = std::fs::read_to_string(&snaps[0]).unwrap();
        let snap_msgs: Vec<serde_json::Value> = snap_content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(snap_msgs, original);
    }

    #[test]
    fn test_compact_thread_nothing_to_compact() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        mgr.append_messages(&id, &[user_msg("hi"), assistant_msg("hello")]);

        let res = mgr.compact_thread(&id).unwrap();
        assert!(res.is_none());
        // No snapshot written
        assert!(snapshot_files(dir.path(), &id).is_empty());
    }

    #[test]
    fn test_compact_thread_unknown_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        assert!(mgr.compact_thread("nope").is_err());
    }

    #[test]
    fn test_delete_thread_removes_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        for r in 0..3 {
            mgr.append_messages(&id, &big_tool_round(&format!("tu_{}", r)));
        }
        mgr.compact_thread(&id).unwrap().expect("should compact");
        assert_eq!(snapshot_files(dir.path(), &id).len(), 1);

        assert!(mgr.delete_thread(&id));
        assert!(snapshot_files(dir.path(), &id).is_empty());
    }

    #[test]
    fn test_snapshot_not_listed_as_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        for r in 0..3 {
            mgr.append_messages(&id, &big_tool_round(&format!("tu_{}", r)));
        }
        mgr.compact_thread(&id).unwrap().expect("should compact");

        // Reload from disk: only the real thread must be discovered
        let mgr2 = ConversationManager::new(dir.path().to_path_buf());
        let ids = mgr2.list_thread_ids();
        assert_eq!(ids, vec![id]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --release -p omnish-daemon test_compact_thread 2>&1 | tail -10`
Expected: compile error, `compact_thread` not found.

- [ ] **Step 3: Implement `compact_thread` and extend `delete_thread`**

Add after `replace_messages` in `conversation_mgr.rs`:

```rust
    /// Compact bulky tool_result blocks in a thread (issue #641).
    ///
    /// Snapshots the current JSONL to `precompact/{tid}.{unix_ts}.jsonl`
    /// before the destructive rewrite; a failed snapshot aborts the rewrite.
    /// Returns Ok(None) when there is nothing to compact.
    pub fn compact_thread(
        &self,
        thread_id: &str,
    ) -> Result<Option<crate::compact::CompactStats>, String> {
        let mut threads = self.threads.lock().unwrap();
        let msgs = threads
            .get_mut(thread_id)
            .ok_or_else(|| "thread not found".to_string())?;

        let mut candidate = msgs.clone();
        let stats = crate::compact::compact_messages(&mut candidate);
        if stats.blocks == 0 {
            return Ok(None);
        }

        let snap_dir = self.threads_dir.join("precompact");
        std::fs::create_dir_all(&snap_dir)
            .map_err(|e| format!("snapshot dir failed: {}", e))?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let src = self.threads_dir.join(format!("{}.jsonl", thread_id));
        let snap = snap_dir.join(format!("{}.{}.jsonl", thread_id, ts));
        std::fs::copy(&src, &snap).map_err(|e| format!("snapshot failed: {}", e))?;

        *msgs = candidate;
        Self::rewrite_thread_file(&self.threads_dir, thread_id, msgs);
        tracing::info!(
            "compacted thread {}: {} blocks, {} -> {} chars (snapshot: {})",
            thread_id, stats.blocks, stats.chars_before, stats.chars_after,
            snap.display()
        );
        Ok(Some(stats))
    }
```

In `delete_thread`, after the two existing `remove_file` calls, add:

```rust
            // Remove precompact snapshots belonging to this thread
            let snap_dir = self.threads_dir.join("precompact");
            if let Ok(rd) = std::fs::read_dir(&snap_dir) {
                let prefix = format!("{}.", thread_id);
                for entry in rd.filter_map(|e| e.ok()) {
                    if entry.file_name().to_string_lossy().starts_with(&prefix) {
                        std::fs::remove_file(entry.path()).ok();
                    }
                }
            }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --release -p omnish-daemon test_compact_thread test_delete_thread_removes test_snapshot_not_listed 2>&1 | tail -10`

Note: cargo accepts only one filter; run instead:
`cargo test --release -p omnish-daemon conversation_mgr 2>&1 | tail -10`
Expected: all conversation_mgr tests PASS including the 5 new ones.

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/conversation_mgr.rs
git commit -m "feat(daemon): add ConversationManager::compact_thread with precompact snapshot (#641)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Daemon `__cmd:compact` handler

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs` (in `handle_builtin_command`, insert before the `thread rename` branch at ~line 2778)

- [ ] **Step 1: Add the handler branch**

`handle_builtin_command` already has `ctx` (with `pending_loops`, `thread_generations`) and `conv_mgr` in scope. Insert:

```rust
    // Handle /compact - truncate bulky tool_result blocks in a thread (issue #641).
    // Payload form: `compact:<tid>`.
    if let Some(tid) = sub.strip_prefix("compact:") {
        if tid.is_empty() {
            return cmd_display("Error: missing thread_id");
        }
        if !conv_mgr.thread_exists(tid) {
            return cmd_display("Error: thread not found");
        }
        // Refuse while an agent loop is parked waiting for client tool
        // results; compacting now would orphan its in-flight work.
        {
            let loops = ctx.pending_loops.lock().await;
            if loops.values().any(|s| s.cm.thread_id == tid) {
                return cmd_display("Error: thread is waiting for tool results, try again later");
            }
        }
        // Invalidate any in-flight agent loop (e.g. awaiting an LLM response)
        // so it discards its work instead of appending to the rewritten file.
        bump_generation(&ctx.thread_generations, tid).await;
        return match conv_mgr.compact_thread(tid) {
            Ok(Some(stats)) => cmd_display(format!(
                "compacted {} tool result(s): {} -> {} chars",
                stats.blocks, stats.chars_before, stats.chars_after
            )),
            Ok(None) => cmd_display("nothing to compact"),
            Err(e) => cmd_display(format!("Error: {}", e)),
        };
    }
```

Note: check the actual field name of the pending agent-loop map in `HandlerCtx` (it is `pending_loops: Arc<Mutex<HashMap<String, AgentLoopState>>>` at server.rs:221) and the ChatMessage field on `AgentLoopState` (`cm`). Adjust if they differ.

- [ ] **Step 2: Build to verify it compiles**

Run: `cargo build --release -p omnish-daemon 2>&1 | tail -5`
Expected: success, no warnings about this code.

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "feat(daemon): handle __cmd:compact builtin command (#641)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Client `/compact` command

**Files:**
- Modify: `crates/omnish-client/src/chat_session.rs` (dispatch after `/thread rename` block at ~line 1461; new method near `handle_thread_sandbox` at ~line 2240)
- Modify: `crates/omnish-client/src/command.rs` (`CHAT_ONLY_COMMANDS` at ~line 366, `help_command` at ~line 221)
- Modify: `crates/omnish-client/src/i18n/en.json`, `zh.json`, `zh-tw.json`, `ja.json`, `ko.json`, `fr.json`, `es.json`, `ar.json`

- [ ] **Step 1: Add dispatch in the chat loop**

In `chat_session.rs`, after the `/thread rename` block (ends ~line 1461), add:

```rust
            // /compact - truncate bulky tool results in current thread
            if trimmed == "/compact" {
                self.handle_compact(session_id, rpc).await;
                continue;
            }
```

- [ ] **Step 2: Add the handler method**

Add next to `handle_thread_sandbox` (mirror its request/response style exactly, including how `display::render_error` / `display::render_response` and `crate::i18n::t` are invoked there):

```rust
    async fn handle_compact(&mut self, session_id: &str, rpc: &RpcClient) {
        let tid = match self.current_thread_id.as_deref() {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => {
                write_stdout(&display::render_error(
                    crate::i18n::t("error.no_active_thread"),
                ));
                return;
            }
        };

        let rid = Uuid::new_v4().to_string()[..8].to_string();
        let request = Message::Request(Request {
            request_id: rid.clone(),
            session_id: session_id.to_string(),
            query: format!("__cmd:compact:{}", tid),
            scope: RequestScope::AllSessions,
        });
        match rpc.call(request).await {
            Ok(Message::Response(resp)) if resp.request_id == rid => {
                let display_text = if let Some(json) = super::parse_cmd_response(&resp.content) {
                    super::cmd_display_str(&json)
                } else {
                    resp.content
                };
                write_stdout(&display::render_response(&display_text));
            }
            _ => {
                write_stdout(&display::render_error("compact failed".to_string()));
            }
        }
    }
```

Note: `error.no_active_thread` already exists in all i18n files (used by `handle_thread_rename`). Match the exact argument type `display::render_error` expects (check its signature in `display.rs`; pass `&str` or `String` accordingly).

- [ ] **Step 3: Register the command**

In `command.rs`:

1. Add to `CHAT_ONLY_COMMANDS` (line ~366):

```rust
pub const CHAT_ONLY_COMMANDS: &[&str] = &[
    "/resume",
    "/resume all",
    "/model",
    "/compact",
    "/thread sandbox",
    ...
];
```

2. In `help_command`, after the `/model` line (~line 221), add:

```rust
    output.push_str(&format!("  /compact - {}\n", crate::i18n::t("command.help.compact")));
```

- [ ] **Step 4: Add i18n keys**

Add `"command.help.compact"` next to `"command.help.model"` (line ~142) in each of the 8 files:

- `en.json`: `"command.help.compact": "Compact bulky tool results in current thread"`
- `zh.json`: `"command.help.compact": "压缩当前线程中的大体积工具结果"`
- `zh-tw.json`: `"command.help.compact": "壓縮當前線程中的大體積工具結果"`
- `ja.json`: `"command.help.compact": "現在のスレッドの大きなツール結果を圧縮"`
- `ko.json`: `"command.help.compact": "현재 스레드의 큰 도구 결과 압축"`
- `fr.json`: `"command.help.compact": "Compacter les gros résultats d'outils du fil actuel"`
- `es.json`: `"command.help.compact": "Compactar resultados de herramientas voluminosos del hilo actual"`
- `ar.json`: `"command.help.compact": "ضغط نتائج الأدوات الكبيرة في المحادثة الحالية"`

- [ ] **Step 5: Build and run client tests**

Run: `cargo build --release -p omnish-client 2>&1 | tail -5`
Expected: success.

Run: `cargo test --release -p omnish-client 2>&1 | tail -10`
Expected: PASS (i18n completeness tests, if any, must pass with the new key).

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-client/src/chat_session.rs crates/omnish-client/src/command.rs crates/omnish-client/src/i18n/
git commit -m "feat(client): add /compact chat command (#641)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Full verification

- [ ] **Step 1: Full release build and test suite**

Run: `cargo build --release 2>&1 | tail -5`
Expected: success.

Run: `cargo test --release 2>&1 | tail -20`
Expected: all crates PASS.

- [ ] **Step 2: Em-dash check on all changed files**

Run: `git diff master --name-only | xargs grep -l $'—' || echo "no em-dash"`
Expected: `no em-dash` (CLAUDE.md forbids em-dash everywhere).

Note: run against the appropriate base if working on a branch off master.

- [ ] **Step 3: Manual verification (requires user)**

Ask the user to restart omnish-daemon (do NOT run it yourself, per CLAUDE.md) and verify in a chat session:

1. Open a thread with several tool-heavy rounds, run `/compact`, expect a `compacted N tool result(s): X -> Y chars` response
2. Run `/compact` again, expect `nothing to compact`
3. Confirm `~/.omnish/threads/precompact/{tid}.{ts}.jsonl` exists and contains the original history
4. Continue chatting in the thread; verify the LLM still responds correctly with the compacted history
5. `/compact` with no active thread shows the no-active-thread error

- [ ] **Step 4: Close issue #641**

Per CLAUDE.md: push first to get correct commit ids, then close with commit info.

```bash
git push
glab issue note 641 -m "已实现 /compact 命令: <列出相关 commit id 和一句话说明>"
glab issue close 641
```

---

## Out of Scope (per spec)

- Auto-compact on token thresholds
- LLM-based summarization of tool results
- Compacting thinking/assistant text
- Snapshot auto-cleanup (future: hook into disk_cleanup task)
- Configurable thresholds (constants in compact.rs)
