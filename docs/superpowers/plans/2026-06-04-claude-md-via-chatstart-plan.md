# CLAUDE.md via ChatStart Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `<cwd>/CLAUDE.md` reading from daemon to client; ship content as a new field on `ChatStart`; daemon caches per thread and injects into the system prompt.

**Architecture:** Client reads + truncates + wraps `CLAUDE.md` into a `<project_instructions>` block at every chat-entry point. Block travels in `ChatStart.project_instructions`. Daemon stores it in a `Mutex<HashMap<thread_id, String>>` on `ConversationManager`, retrieves on each `ChatMessage`, and clears on `ChatEnd`. Existing daemon-side filesystem read is removed; `MIN_COMPATIBLE_VERSION` bumps to 24 so mismatched peers fail at auth instead of silently dropping CLAUDE.md.

**Tech Stack:** Rust, bincode 1.x, serde, tokio, omnish workspace (crates/omnish-protocol, crates/omnish-client, crates/omnish-daemon).

**Spec:** `docs/superpowers/specs/2026-06-04-claude-md-via-chatstart-design.md`
**Issue:** #637

## File Structure

**Create:**
- `crates/omnish-client/src/project_instructions.rs` - pure `load_for_cwd(cwd)` reader + truncator + wrapper

**Modify:**
- `crates/omnish-protocol/src/message.rs` - bump version constants, add field to `ChatStart`, add round-trip test
- `crates/omnish-client/src/main.rs` - register new module
- `crates/omnish-client/src/chat_session.rs` - call `load_for_cwd` at three `ChatStart` construction sites (lines 1611, 2661, 2683)
- `crates/omnish-daemon/src/conversation_mgr.rs` - add `project_instructions` field + three methods to `ConversationManager` + unit tests
- `crates/omnish-daemon/src/server.rs` - `handle_chat_start` calls `set_project_instructions` on success; `handle_chat_message` replaces filesystem read with cache lookup; `ChatEnd` arm calls `clear_project_instructions`; remove `load_project_instructions` and `MAX_PROJECT_INSTRUCTIONS_BYTES`

---

## Task 1: Protocol field + version bump + round-trip test

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs`

- [ ] **Step 1: Write the failing round-trip test**

Insert this test just after the existing `chat_ready_with_history_round_trips` test (find by `grep -n chat_ready_with_history_round_trips crates/omnish-protocol/src/message.rs`):

```rust
    /// Regression test: ChatStart with populated project_instructions must
    /// survive a bincode round-trip. Guards against accidental removal of
    /// the field or a wire-format mismatch when MIN_COMPATIBLE_VERSION is bumped.
    #[test]
    fn chat_start_with_project_instructions_round_trips() {
        let body = "<project_instructions>\nSource: /tmp/x/CLAUDE.md\n\nhello world\n</project_instructions>".to_string();
        let frame = Frame {
            request_id: 7,
            payload: Message::ChatStart(ChatStart {
                request_id: "abc".to_string(),
                session_id: "sess".to_string(),
                new_thread: true,
                thread_id: None,
                project_instructions: Some(body.clone()),
            }),
        };
        let bytes = frame.serialize().expect("serialize");
        let (decoded, _len) = Frame::from_bytes(&bytes).expect("deserialize");
        match decoded.payload {
            Message::ChatStart(cs) => assert_eq!(cs.project_instructions, Some(body)),
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    /// Same as above but with `project_instructions = None`. The Option tag
    /// must still be emitted, and a None must round-trip back to None.
    #[test]
    fn chat_start_without_project_instructions_round_trips() {
        let frame = Frame {
            request_id: 7,
            payload: Message::ChatStart(ChatStart {
                request_id: "abc".to_string(),
                session_id: "sess".to_string(),
                new_thread: true,
                thread_id: None,
                project_instructions: None,
            }),
        };
        let bytes = frame.serialize().expect("serialize");
        let (decoded, _len) = Frame::from_bytes(&bytes).expect("deserialize");
        match decoded.payload {
            Message::ChatStart(cs) => assert!(cs.project_instructions.is_none()),
            other => panic!("unexpected variant: {:?}", other),
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p omnish-protocol --release chat_start_with_project_instructions_round_trips`
Expected: FAIL — compile error, `struct ChatStart has no field named project_instructions`.

- [ ] **Step 3: Add the field to `ChatStart`**

Find `pub struct ChatStart {` in `crates/omnish-protocol/src/message.rs` (search for `pub struct ChatStart`). Add the new field at the END of the struct (positional bincode requires append-only):

```rust
pub struct ChatStart {
    pub request_id: String,
    pub session_id: String,
    pub new_thread: bool,
    /// If set, resume this specific thread instead of creating a new one.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Pre-formatted `<project_instructions>` block read from
    /// `<shell_cwd>/CLAUDE.md` on the client. `None` when absent or
    /// unreadable. Already truncated and wrapped; daemon appends as-is.
    #[serde(default)]
    pub project_instructions: Option<String>,
}
```

- [ ] **Step 4: Bump the version constants**

In the same file, find:

```rust
pub const PROTOCOL_VERSION: u32 = 23;
```

and:

```rust
pub const MIN_COMPATIBLE_VERSION: u32 = 23;
```

Change both to 24:

```rust
pub const PROTOCOL_VERSION: u32 = 24;
```
```rust
pub const MIN_COMPATIBLE_VERSION: u32 = 24;
```

- [ ] **Step 5: Run the protocol tests to verify they pass**

Run: `cargo test -p omnish-protocol --release`
Expected: PASS. All existing tests (including `variant_indices_are_stable`) plus the two new ones.

- [ ] **Step 6: Verify the rest of the workspace still compiles**

Run: `cargo build --release --workspace`
Expected: PASS. The new `project_instructions` field has a default, and call sites that construct `ChatStart` literally (e.g. in `chat_session.rs`) will fail to compile because Rust requires all fields. **This is intentional — Task 3 fixes those call sites.** If the workspace build fails only at those call sites, that is the expected halfway state; do not fix them yet, move on to Task 2 first (Task 2 is independent of these call sites).

If unrelated compile errors appear, stop and inspect.

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-protocol/src/message.rs
git commit -m "feat(protocol): add ChatStart.project_instructions; bump versions to 24 (#637)"
```

---

## Task 2: Client `project_instructions` module

**Files:**
- Create: `crates/omnish-client/src/project_instructions.rs`
- Modify: `crates/omnish-client/src/main.rs`

- [ ] **Step 1: Create the new module file with failing tests**

Write `crates/omnish-client/src/project_instructions.rs`:

```rust
use std::io::ErrorKind;
use std::path::Path;

const MAX_BYTES: usize = 128 * 1024;

/// Read `<cwd>/CLAUDE.md`, truncate at a char boundary if necessary, and
/// wrap in a `<project_instructions>` block. Returns `None` when `cwd` is
/// empty, the file is absent, or it is unreadable.
pub fn load_for_cwd(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let path = Path::new(cwd).join("CLAUDE.md");
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::NotFound => return None,
        Err(e) => {
            crate::event_log::push(format!(
                "project_instructions: read failed at {}: {}",
                path.display(),
                e
            ));
            return None;
        }
    };
    let (body, truncated) = if content.len() > MAX_BYTES {
        let mut end = MAX_BYTES;
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        (&content[..end], true)
    } else {
        (content.as_str(), false)
    };
    let tail = if truncated {
        "\n[... truncated: file exceeded 128KB ...]\n"
    } else {
        "\n"
    };
    Some(format!(
        "<project_instructions>\nSource: {}\n\n{}{}</project_instructions>",
        path.display(),
        body,
        tail
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn returns_none_for_empty_cwd() {
        assert!(load_for_cwd("").is_none());
    }

    #[test]
    fn returns_none_when_file_missing() {
        let dir = tempdir().unwrap();
        assert!(load_for_cwd(dir.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn wraps_present_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "rule one\nrule two").unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(out.starts_with("<project_instructions>\nSource: "));
        assert!(out.contains("rule one\nrule two"));
        assert!(out.ends_with("</project_instructions>"));
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn truncates_oversized_file_at_char_boundary() {
        let dir = tempdir().unwrap();
        // Place a 3-byte CJK char straddling MAX_BYTES so the naive cut
        // would land mid-character; the boundary-finder must step back.
        let mut content = "x".repeat(MAX_BYTES - 1);
        content.push('中'); // bytes [MAX_BYTES-1 .. MAX_BYTES+2)
        content.push_str(&"y".repeat(MAX_BYTES));
        fs::write(dir.path().join("CLAUDE.md"), &content).unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        assert!(out.contains("[... truncated: file exceeded 128KB ...]"));
        // Clean cut: the straddling CJK char must not survive.
        assert!(!out.contains('中'));
    }

    #[test]
    fn includes_absolute_source_path() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "hello").unwrap();
        let out = load_for_cwd(dir.path().to_str().unwrap()).unwrap();
        let expected_path = dir.path().join("CLAUDE.md");
        assert!(out.contains(&format!("Source: {}", expected_path.display())));
    }

    #[test]
    fn returns_none_on_invalid_utf8() {
        let dir = tempdir().unwrap();
        // Bytes that are not valid UTF-8 (lone 0xFF continuation byte).
        fs::write(dir.path().join("CLAUDE.md"), [0xFFu8, 0xFE, 0xFD]).unwrap();
        assert!(load_for_cwd(dir.path().to_str().unwrap()).is_none());
    }
}
```

- [ ] **Step 2: Register the module in `main.rs`**

In `crates/omnish-client/src/main.rs`, find the block of `mod ...;` declarations near the top (around line 2-22). Add this line in alphabetical position relative to neighbors (between `pending_notices` and `ghost_complete` is fine):

```rust
mod project_instructions;
```

- [ ] **Step 3: Verify `tempfile` is an available test dep**

Run: `grep -A1 '\[dev-dependencies\]' crates/omnish-client/Cargo.toml`

If `tempfile` is not listed under `[dev-dependencies]`, add it:

```toml
[dev-dependencies]
tempfile = "3"
```

Otherwise leave the file alone.

- [ ] **Step 4: Run the new tests to verify they pass**

Run: `cargo test -p omnish-client --release project_instructions`
Expected: 6 tests passing — `returns_none_for_empty_cwd`, `returns_none_when_file_missing`, `wraps_present_file`, `truncates_oversized_file_at_char_boundary`, `includes_absolute_source_path`, `returns_none_on_invalid_utf8`.

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/src/project_instructions.rs crates/omnish-client/src/main.rs crates/omnish-client/Cargo.toml
git commit -m "feat(client): add project_instructions module to read CLAUDE.md (#637)"
```

---

## Task 3: Wire `load_for_cwd` into the three `ChatStart` send sites

**Files:**
- Modify: `crates/omnish-client/src/chat_session.rs:1611-1616`, `chat_session.rs:2661-2666`, `chat_session.rs:2683-2688`

- [ ] **Step 1: Site 1 — initial chat entry (~line 1611)**

Find this exact block in `crates/omnish-client/src/chat_session.rs`:

```rust
            if self.current_thread_id.is_none() {
                let req_id = Uuid::new_v4().to_string()[..8].to_string();
                let start_msg = Message::ChatStart(ChatStart {
                    request_id: req_id.clone(),
                    session_id: session_id.to_string(),
                    new_thread: true,
                    thread_id: None,
                });
```

Replace with:

```rust
            if self.current_thread_id.is_none() {
                let req_id = Uuid::new_v4().to_string()[..8].to_string();
                let project_instructions = self
                    .shell_cwd
                    .as_deref()
                    .and_then(crate::project_instructions::load_for_cwd);
                let start_msg = Message::ChatStart(ChatStart {
                    request_id: req_id.clone(),
                    session_id: session_id.to_string(),
                    new_thread: true,
                    thread_id: None,
                    project_instructions,
                });
```

- [ ] **Step 2: Site 2 — resume by tid (~line 2661)**

Find:

```rust
    async fn handle_resume_tid(&mut self, tid: &str, session_id: &str, rpc: &RpcClient) -> bool {
        crate::event_log::push(format!("resume_tid: sending ChatStart thread={}", tid));
        let rid = Uuid::new_v4().to_string()[..8].to_string();
        let start_msg = Message::ChatStart(ChatStart {
            request_id: rid.clone(),
            session_id: session_id.to_string(),
            new_thread: false,
            thread_id: Some(tid.to_string()),
        });
```

Replace with:

```rust
    async fn handle_resume_tid(&mut self, tid: &str, session_id: &str, rpc: &RpcClient) -> bool {
        crate::event_log::push(format!("resume_tid: sending ChatStart thread={}", tid));
        let rid = Uuid::new_v4().to_string()[..8].to_string();
        let project_instructions = self
            .shell_cwd
            .as_deref()
            .and_then(crate::project_instructions::load_for_cwd);
        let start_msg = Message::ChatStart(ChatStart {
            request_id: rid.clone(),
            session_id: session_id.to_string(),
            new_thread: false,
            thread_id: Some(tid.to_string()),
            project_instructions,
        });
```

- [ ] **Step 3: Site 3 — alternate tid after thread_locked (~line 2683)**

Find:

```rust
                    if let Some(alt_tid) = self.show_resume_picker(session_id, rpc, false).await {
                        // Resume the selected thread (locked items are disabled in picker,
                        // so this should not hit thread_locked again)
                        let rid2 = Uuid::new_v4().to_string()[..8].to_string();
                        let start2 = Message::ChatStart(ChatStart {
                            request_id: rid2.clone(),
                            session_id: session_id.to_string(),
                            new_thread: false,
                            thread_id: Some(alt_tid),
                        });
```

Replace with:

```rust
                    if let Some(alt_tid) = self.show_resume_picker(session_id, rpc, false).await {
                        // Resume the selected thread (locked items are disabled in picker,
                        // so this should not hit thread_locked again)
                        let rid2 = Uuid::new_v4().to_string()[..8].to_string();
                        let project_instructions = self
                            .shell_cwd
                            .as_deref()
                            .and_then(crate::project_instructions::load_for_cwd);
                        let start2 = Message::ChatStart(ChatStart {
                            request_id: rid2.clone(),
                            session_id: session_id.to_string(),
                            new_thread: false,
                            thread_id: Some(alt_tid),
                            project_instructions,
                        });
```

- [ ] **Step 4: Verify the client builds**

Run: `cargo build --release -p omnish-client`
Expected: PASS.

- [ ] **Step 5: Verify the daemon still builds**

Run: `cargo build --release -p omnish-daemon`
Expected: PASS. (Daemon still uses old `load_project_instructions` from session_attrs — it ignores the new `project_instructions` field on ChatStart. Tasks 4-5 fix the daemon.)

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-client/src/chat_session.rs
git commit -m "feat(client): attach CLAUDE.md to all three ChatStart sites (#637)"
```

---

## Task 4: `ConversationManager` `project_instructions` cache + tests

**Files:**
- Modify: `crates/omnish-daemon/src/conversation_mgr.rs`

- [ ] **Step 1: Write failing tests at the bottom of `conversation_mgr.rs`**

Find the existing `#[cfg(test)] mod tests` block (run `grep -n "#\[cfg(test)\]" crates/omnish-daemon/src/conversation_mgr.rs | head`). Add these tests inside that mod (at the end, before its closing `}`):

```rust
    #[test]
    fn project_instructions_set_get_clear() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());

        assert!(mgr.get_project_instructions("t1").is_none());

        mgr.set_project_instructions("t1", Some("BLOCK_A".to_string()));
        assert_eq!(mgr.get_project_instructions("t1").as_deref(), Some("BLOCK_A"));

        mgr.set_project_instructions("t1", Some("BLOCK_B".to_string()));
        assert_eq!(mgr.get_project_instructions("t1").as_deref(), Some("BLOCK_B"));

        mgr.clear_project_instructions("t1");
        assert!(mgr.get_project_instructions("t1").is_none());
    }

    #[test]
    fn project_instructions_set_none_removes_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());

        mgr.set_project_instructions("t1", Some("BLOCK".to_string()));
        assert!(mgr.get_project_instructions("t1").is_some());

        mgr.set_project_instructions("t1", None);
        assert!(mgr.get_project_instructions("t1").is_none());
    }
```

If the existing test module doesn't already use `tempfile`, verify it is a dev-dep on the daemon crate:

Run: `grep -A2 '\[dev-dependencies\]' crates/omnish-daemon/Cargo.toml`

If `tempfile` is absent, add it:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p omnish-daemon --release project_instructions_`
Expected: FAIL — compile error, `set_project_instructions` / `get_project_instructions` / `clear_project_instructions` are not methods on `ConversationManager`.

- [ ] **Step 3: Add the field to `ConversationManager`**

Find `pub struct ConversationManager {` (line ~62 in `conversation_mgr.rs`). Add a new field:

```rust
pub struct ConversationManager {
    threads_dir: PathBuf,
    /// In-memory store: thread_id → raw JSON messages.
    threads: Mutex<HashMap<String, Vec<serde_json::Value>>>,
    /// In-memory cache: thread_id → pre-wrapped <project_instructions> block
    /// supplied by the client at chat-entry (ChatStart). Populated by
    /// handle_chat_start, read by handle_chat_message, cleared by ChatEnd.
    /// Not persisted; daemon restart drops entries and they are replenished
    /// the next time the client sends ChatStart.
    project_instructions: Mutex<HashMap<String, String>>,
}
```

- [ ] **Step 4: Initialize the new field in `ConversationManager::new`**

Find `pub fn new(threads_dir: PathBuf) -> Self` (around line 333). Locate the `Self { ... }` literal at the end of the function and add the new field:

```rust
        Self {
            threads_dir,
            threads: Mutex::new(threads),
            project_instructions: Mutex::new(HashMap::new()),
        }
```

(If the existing `Self { ... }` block has different formatting/field order, preserve the order and just append the new field.)

- [ ] **Step 5: Add the three methods to `impl ConversationManager`**

Append these methods inside `impl ConversationManager` (anywhere in the impl block is fine; placing them next to `save_meta`/`load_meta` keeps related thread-keyed helpers together):

```rust
    /// Store the pre-wrapped `<project_instructions>` block for a thread.
    /// `None` removes any prior entry, which is what `handle_chat_start`
    /// should pass when the client reports no CLAUDE.md.
    pub fn set_project_instructions(&self, thread_id: &str, content: Option<String>) {
        let mut map = self.project_instructions.lock().unwrap();
        match content {
            Some(s) => {
                map.insert(thread_id.to_string(), s);
            }
            None => {
                map.remove(thread_id);
            }
        }
    }

    /// Look up the cached `<project_instructions>` block for a thread.
    /// Returns `None` when no entry exists (daemon restart, ChatEnd already
    /// fired, or the client never sent one).
    pub fn get_project_instructions(&self, thread_id: &str) -> Option<String> {
        self.project_instructions.lock().unwrap().get(thread_id).cloned()
    }

    /// Drop the cached entry for a thread. Called from the ChatEnd handler.
    pub fn clear_project_instructions(&self, thread_id: &str) {
        self.project_instructions.lock().unwrap().remove(thread_id);
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p omnish-daemon --release project_instructions_`
Expected: PASS — both `project_instructions_set_get_clear` and `project_instructions_set_none_removes_entry`.

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-daemon/src/conversation_mgr.rs crates/omnish-daemon/Cargo.toml
git commit -m "feat(daemon): add per-thread project_instructions cache (#637)"
```

---

## Task 5: Wire daemon handlers to use the cache; remove dead code

This is the atomic switch from "daemon reads CLAUDE.md from disk" to "daemon reads from the per-thread cache filled by ChatStart".

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

- [ ] **Step 1: Have `handle_chat_start` populate the cache on success**

In `crates/omnish-daemon/src/server.rs`, find the line `let _ = tx.send(ready).await;` at the end of `handle_chat_start` (currently line ~1095). Insert this block on the line BEFORE it:

```rust
    if let Message::ChatReady(ChatReady { thread_id, error: None, .. }) = &ready {
        conv_mgr.set_project_instructions(thread_id, cs.project_instructions.clone());
    }
    let _ = tx.send(ready).await;
```

This skips the failure paths (`thread_locked`, `not_found`) — those return `ChatReady { error: Some(...), ... }` and would falsely associate the content with a thread the user never actually entered.

- [ ] **Step 2: Have `handle_chat_message` read from the cache instead of disk**

Find the block in `handle_chat_message` (currently lines ~1525-1532):

```rust
    let project_instructions = session_attrs
        .get("shell_cwd")
        .and_then(|cwd| load_project_instructions(cwd));

    let full_system_prompt = match project_instructions {
        Some(ref pi) => format!("{}\n\n{}\n\n{}", system_prompt, reminder, pi),
        None => format!("{}\n\n{}", system_prompt, reminder),
    };
```

Replace with:

```rust
    let project_instructions = conv_mgr.get_project_instructions(&cm.thread_id);

    let full_system_prompt = match project_instructions {
        Some(ref pi) => format!("{}\n\n{}\n\n{}", system_prompt, reminder, pi),
        None => format!("{}\n\n{}", system_prompt, reminder),
    };
```

- [ ] **Step 3: Have the `ChatEnd` arm clear the cache**

Find the `Message::ChatEnd(ce) => { ... }` arm (around line 848 in `server.rs`). Just before the existing `let _ = tx.send(Message::Ack).await;` at the end of this arm, insert:

```rust
            ctx.conv_mgr.clear_project_instructions(&ce.thread_id);
```

So the arm now ends with:

```rust
            ctx.conv_mgr.clear_project_instructions(&ce.thread_id);
            let _ = tx.send(Message::Ack).await;
        }
```

- [ ] **Step 4: Delete `load_project_instructions` and its constant**

Find `const MAX_PROJECT_INSTRUCTIONS_BYTES: usize = 128 * 1024;` (around line 1420). Delete this constant and the entire `fn load_project_instructions(cwd: &str) -> Option<String> { ... }` function that follows it (ends around line 1463).

Verify nothing else references either symbol:

```bash
grep -n "load_project_instructions\|MAX_PROJECT_INSTRUCTIONS_BYTES" crates/omnish-daemon/src/server.rs
```

Expected output: empty.

- [ ] **Step 5: Build the daemon to verify everything compiles**

Run: `cargo build --release -p omnish-daemon`
Expected: PASS.

- [ ] **Step 6: Run the full daemon test suite**

Run: `cargo test -p omnish-daemon --release`
Expected: PASS — including the `project_instructions_*` tests added in Task 4 and any pre-existing chat tests.

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "feat(daemon): use per-thread cache for project_instructions; drop disk read (#637)"
```

---

## Task 6: Full release build + workspace test pass

**Files:** none modified (verification only).

- [ ] **Step 1: Full release build**

Run: `cargo build --release --workspace`
Expected: PASS.

- [ ] **Step 2: Full release test pass**

Run: `cargo test --release --workspace`
Expected: PASS.

- [ ] **Step 3: Confirm no leftover references to deleted symbols**

Run:
```bash
grep -rn "load_project_instructions\|MAX_PROJECT_INSTRUCTIONS_BYTES" crates/ docs/
```

Expected: hits only inside `docs/superpowers/specs/2026-06-04-claude-md-via-chatstart-design.md` (where they are referenced as removed items) and `docs/superpowers/plans/2026-06-04-claude-md-via-chatstart-plan.md` (this plan). Zero hits in `crates/`.

- [ ] **Step 4: Confirm protocol version is consistently 24**

Run:
```bash
grep -rn "PROTOCOL_VERSION\|MIN_COMPATIBLE_VERSION" crates/omnish-protocol/src/
```

Expected: both consts show `= 24`.

- [ ] **Step 5: Manual cross-machine smoke test (do not automate; record in PR description)**

Document the following steps in the PR description so the reviewer can re-run them on a real two-host deployment:

```
# Host A (client):
cd /some/project
echo "TEST_SENTINEL_$(uuidgen)" >> CLAUDE.md
omnish    # connects to daemon on host B
# enter chat, ask: "what's in the project_instructions block?"
# expect: LLM repeats the sentinel string verbatim.
```

Note on test coverage: the spec lists handler-level integration tests
(`handle_chat_start`, `handle_chat_message`, `handle_chat_end`). The daemon
codebase does not have a pattern for invoking these handlers directly under
unit-test scaffolding — chat behavior is exercised by `tools/integration_tests`
end-to-end and by manual smoke. We therefore rely on:
- Pure-function tests for the cache (`project_instructions_*` in
  `conversation_mgr.rs`)
- Protocol round-trip tests for the new field
- The manual cross-machine smoke above

Adding mock-RPC handler tests is a separate scope item if the pattern is
ever established.

No commit for this task — it's verification only. If everything passes, the PR is ready to open.
