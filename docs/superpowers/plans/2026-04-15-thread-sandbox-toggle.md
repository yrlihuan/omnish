# `/thread sandbox on|off` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `/thread sandbox on|off` so users can disable Landlock/bwrap sandboxing for all `ChatToolCall` dispatched from a specific chat thread, with state persisted per-thread and visible on resume.

**Architecture:** Daemon-authoritative state in `ThreadMeta.sandbox_disabled` (persisted to `<thread>.meta.json`). Daemon forces `ChatToolCall.sandboxed=false` when the flag is set. Client exposes `/thread sandbox [on|off]` command, buffers the preference before a thread exists, applies it right after `ChatReady` for a new thread, and renders a yellow warning on resume when sandbox is off.

**Tech Stack:** Rust workspace (omnish-protocol, omnish-daemon, omnish-client). Bincode-framed RPC messages. Serde JSON for `ThreadMeta` persistence.

**Spec:** `docs/superpowers/specs/2026-04-15-thread-sandbox-toggle-design.md`

**Issue:** #535

---

## File Map

- Modify: `crates/omnish-protocol/src/message.rs` - bump PROTOCOL_VERSION, extend `ChatReady` with `sandbox_disabled`
- Modify: `crates/omnish-daemon/src/conversation_mgr.rs` - add `sandbox_disabled` field to `ThreadMeta`, helper to set and persist
- Modify: `crates/omnish-daemon/src/server.rs` - handle `__cmd:thread sandbox[ on|off]:<tid>`, include sandbox flag in `ChatReady`, force `sandboxed=false` in `ChatToolCall` dispatch, surface in `/thread stats`
- Modify: `crates/omnish-client/src/chat_session.rs` - `/thread sandbox` dispatch, `pending_sandbox_off` buffer, apply after `ChatReady` for new thread, resume warning
- Create: `tools/integration_tests/test_thread_sandbox.sh` - exercises command, buffered path, persistence, runtime bypass
- Modify: `.gitlab-ci.yml` - add the new test to the integration-test and integration-test-zsh jobs
- Modify: `CHANGELOG.md` - entry under the next unreleased version heading

---

## Task 1: Protocol changes (version bump + ChatReady field)

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs`

Context: `PROTOCOL_VERSION` currently 17 (line ~20). `ChatReady` struct at lines 294-322.

- [ ] **Step 1: Bump protocol version**

In `crates/omnish-protocol/src/message.rs`, find:

```rust
pub const PROTOCOL_VERSION: u32 = 17;
```

Replace with:

```rust
pub const PROTOCOL_VERSION: u32 = 18;
```

`MIN_COMPATIBLE_VERSION` stays at 14 - this is an additive optional field.

- [ ] **Step 2: Extend ChatReady**

In the same file, find the `ChatReady` struct. After the last field (`error_display`), add:

```rust
    /// Per-thread sandbox override, mirrored from ThreadMeta.sandbox_disabled.
    /// When Some(true), client should render a resume warning and the daemon
    /// forces ChatToolCall.sandboxed=false for this thread.
    #[serde(default)]
    pub sandbox_disabled: Option<bool>,
```

- [ ] **Step 3: Update every ChatReady construction site in message.rs**

Test builders in the same file construct `ChatReady` literals. For each site (search for `ChatReady {` within `message.rs`), append `sandbox_disabled: None,` as the last field. Sites to fix:

- Around line 728 (in `test_variant_index_stability`)
- Around line 899 (in `test_frame_with_chat_ready`)

Example diff pattern:

```rust
Message::ChatReady(ChatReady {
    request_id: String::new(),
    thread_id: String::new(),
    last_exchange: None,
    earlier_count: 0,
    model_name: None,
    history: None,
    thread_host: None,
    thread_cwd: None,
    thread_summary: None,
    error: None,
    error_display: None,
    sandbox_disabled: None,
}),
```

- [ ] **Step 4: Build just the protocol crate**

Run: `cargo build --release -p omnish-protocol`
Expected: clean build. If any other site constructs `ChatReady` (e.g., `test_frame_with_chat_ready` referenced elsewhere), the compiler will flag it - fix inline.

- [ ] **Step 5: Build the whole workspace**

Run: `cargo build --release`
Expected: clean build. Any `ChatReady` constructor in `omnish-daemon` / `omnish-client` that doesn't compile is addressed in later tasks; if the build fails here, add `sandbox_disabled: None,` at each flagged site just to restore compilation. The logic rewrite in Tasks 3 and 5 will replace these defaults.

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-protocol/src/message.rs \
  crates/omnish-daemon/src/server.rs \
  crates/omnish-client/src/chat_session.rs
git commit -m "protocol: bump to v18, add ChatReady.sandbox_disabled (#535)"
```

(Include the daemon/client files only if you had to add placeholder `sandbox_disabled: None` to restore the build. Those sites will be rewritten in later tasks.)

---

## Task 2: ThreadMeta data model

**Files:**
- Modify: `crates/omnish-daemon/src/conversation_mgr.rs`

Context: `ThreadMeta` struct at lines 13-39. `save_meta` / `load_meta` exist at lines 205 / 213.

- [ ] **Step 1: Write failing serde roundtrip test**

In `crates/omnish-daemon/src/conversation_mgr.rs`, find the existing `#[cfg(test)]` module. Add this test alongside the existing `test_delete_thread_removes_meta` / similar:

```rust
#[test]
fn test_thread_meta_sandbox_disabled_roundtrip() {
    // Default ThreadMeta: sandbox_disabled is None and omitted from JSON.
    let meta = ThreadMeta::default();
    let json = serde_json::to_string(&meta).unwrap();
    assert!(!json.contains("sandbox_disabled"),
        "absent flag must not appear in JSON, got: {}", json);

    // sandbox_disabled=Some(true) roundtrips.
    let meta_off = ThreadMeta { sandbox_disabled: Some(true), ..ThreadMeta::default() };
    let json = serde_json::to_string(&meta_off).unwrap();
    assert!(json.contains("\"sandbox_disabled\":true"),
        "flag must appear when set, got: {}", json);
    let parsed: ThreadMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.sandbox_disabled, Some(true));

    // JSON without the field loads as None (pre-feature threads).
    let legacy = "{}";
    let parsed: ThreadMeta = serde_json::from_str(legacy).unwrap();
    assert_eq!(parsed.sandbox_disabled, None);
}
```

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test --release -p omnish-daemon conversation_mgr::tests::test_thread_meta_sandbox_disabled_roundtrip`
Expected: compile error ("no field `sandbox_disabled`") or runtime assertion failure.

- [ ] **Step 3: Add field to ThreadMeta**

In `crates/omnish-daemon/src/conversation_mgr.rs`, inside `pub struct ThreadMeta { ... }`, append (after `pub system_reminder`):

```rust
    /// Per-thread sandbox override. When Some(true), daemon forces
    /// ChatToolCall.sandboxed=false for this thread, bypassing permit_rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_disabled: Option<bool>,
```

- [ ] **Step 4: Run test, verify it passes**

Run: `cargo test --release -p omnish-daemon conversation_mgr::tests::test_thread_meta_sandbox_disabled_roundtrip`
Expected: PASS.

- [ ] **Step 5: Add setter helper**

Still in `conversation_mgr.rs`, find the existing `impl ConversationManager { ... }` block (the one containing `save_meta` around line 205). Add this method alongside:

```rust
    /// Set per-thread sandbox override and persist. `off=true` → disabled;
    /// `off=false` → clears the override (back to default "on").
    /// Returns the effective state after the update for display.
    pub fn set_sandbox_disabled(&self, thread_id: &str, off: bool) -> bool {
        let mut meta = self.load_meta(thread_id);
        meta.sandbox_disabled = if off { Some(true) } else { None };
        self.save_meta(thread_id, &meta);
        off
    }
```

- [ ] **Step 6: Write failing test for the setter**

Add alongside the roundtrip test:

```rust
#[test]
fn test_set_sandbox_disabled_persists() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = ConversationManager::new(tmp.path().to_path_buf());
    let tid = mgr.create_thread(ThreadMeta::default());

    assert_eq!(mgr.load_meta(&tid).sandbox_disabled, None);

    assert_eq!(mgr.set_sandbox_disabled(&tid, true), true);
    assert_eq!(mgr.load_meta(&tid).sandbox_disabled, Some(true));

    assert_eq!(mgr.set_sandbox_disabled(&tid, false), false);
    assert_eq!(mgr.load_meta(&tid).sandbox_disabled, None);
}
```

If `tempfile` isn't already a dev-dependency of this crate, check the existing tests in the same file - the `test_delete_thread_removes_meta` test likely uses the same pattern. If it uses `std::env::temp_dir()` or similar, mirror that approach instead.

- [ ] **Step 7: Run test, verify it passes**

Run: `cargo test --release -p omnish-daemon conversation_mgr::tests::test_set_sandbox_disabled_persists`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/omnish-daemon/src/conversation_mgr.rs
git commit -m "daemon: add ThreadMeta.sandbox_disabled and setter (#535)"
```

---

## Task 3: Daemon RPC handler for `__cmd:thread sandbox`

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

Context: `handle_builtin_command` starts at line 2163. Existing `__cmd` handlers include `context chat:<tid>`, `conversations del <tid>`, `models [<tid>]` - pattern for tid-in-query is established.

- [ ] **Step 1: Add sandbox handlers**

In `crates/omnish-daemon/src/server.rs`, inside `handle_builtin_command`, before the final `match sub { ... }` block (the one ending with `other => cmd_display(...)`), add:

```rust
    // Handle /thread sandbox - query or set per-thread sandbox override.
    // Queries embed the thread_id as ":<tid>" suffix, matching /context chat.
    if let Some(rest) = sub.strip_prefix("thread sandbox") {
        let rest = rest.trim_start();
        let (action, tid) = if let Some(tid) = rest.strip_prefix(":") {
            ("query", tid)
        } else if let Some(tid) = rest.strip_prefix("on:") {
            ("on", tid)
        } else if let Some(tid) = rest.strip_prefix("off:") {
            ("off", tid)
        } else {
            return cmd_display("Usage: __cmd:thread sandbox[ on|off]:<tid>");
        };
        if tid.is_empty() {
            return cmd_display("Error: missing thread_id");
        }
        let display = match action {
            "on" => {
                conv_mgr.set_sandbox_disabled(tid, false);
                format!("sandbox enabled for thread {}", &tid[..8.min(tid.len())])
            }
            "off" => {
                conv_mgr.set_sandbox_disabled(tid, true);
                format!("sandbox disabled for thread {}", &tid[..8.min(tid.len())])
            }
            _ => {
                let off = conv_mgr.load_meta(tid).sandbox_disabled.unwrap_or(false);
                format!("sandbox: {}", if off { "off" } else { "on" })
            }
        };
        return cmd_display(display);
    }
```

- [ ] **Step 2: Build**

Run: `cargo build --release -p omnish-daemon`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "daemon: handle __cmd:thread sandbox for per-thread override (#535)"
```

---

## Task 4: Enforce sandbox_disabled in ChatToolCall dispatch

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

Context: `ChatToolCall` construction at line 1671; current sandboxed logic: `sandboxed: matched_rule.is_none()` (line 1678). `state.cm.thread_id` is available on the enclosing state.

- [ ] **Step 1: Locate thread metadata once per agent-loop iteration**

In `crates/omnish-daemon/src/server.rs`, find the outer loop that produces `ChatToolCall` messages (around line 1600-1700, the `for tc in ...` loop or equivalent that iterates tool calls from an LLM response). Immediately before the `for` loop body (or at the top of the response-processing block so it runs once per LLM iteration), add:

```rust
let thread_sandbox_off = conv_mgr
    .load_meta(&state.cm.thread_id)
    .sandbox_disabled
    .unwrap_or(false);
```

If you cannot cheaply hoist it out of the per-tool-call scope (e.g., the enclosing scope is not obvious), inline it into the `sandboxed:` expression - meta reads are JSON file reads, not free, but are ~10µs and there are few tool calls per iteration. Hoisting is preferred but not required.

- [ ] **Step 2: Update the sandboxed flag**

Find:

```rust
                                sandboxed: matched_rule.is_none(),
```

Replace with:

```rust
                                sandboxed: matched_rule.is_none() && !thread_sandbox_off,
```

- [ ] **Step 3: Add a debug log when the override takes effect**

Directly before the `tx.send(Message::ChatToolCall(...))` call (so it only fires when a tool call is actually dispatched), add:

```rust
if thread_sandbox_off {
    tracing::warn!(
        "thread sandbox disabled: thread={}, tool={}",
        state.cm.thread_id, tc.name
    );
}
```

Place it alongside the existing `if let Some(ref rule) = matched_rule` warning (around line 1664).

- [ ] **Step 4: Build**

Run: `cargo build --release -p omnish-daemon`
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "daemon: force sandboxed=false when thread has sandbox_disabled (#535)"
```

---

## Task 5: Populate ChatReady.sandbox_disabled from ThreadMeta

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

Context: `ChatReady` is constructed in 6 places (lines 737, 753, 786, 804, 826, 858, 875 per earlier grep). Resume-existing-thread paths are at 786 and 858 - these need the real value. Error / not-found / new-thread paths should stay `None`.

- [ ] **Step 1: Update the two resume paths**

In `crates/omnish-daemon/src/server.rs`, find the `ChatReady` constructor at line ~786 (resume specific thread - the one after `let old_meta = conv_mgr.load_meta(tid);`). After `error_display: None,`, add:

```rust
                        sandbox_disabled: old_meta.sandbox_disabled,
```

Find the `ChatReady` constructor at line ~858 (resume latest thread - also after `let old_meta = conv_mgr.load_meta(&tid);`). Add the same line:

```rust
                            sandbox_disabled: old_meta.sandbox_disabled,
```

- [ ] **Step 2: Update all other ChatReady constructors with None**

For each remaining `ChatReady { ... }` literal in `server.rs` (lines ~737, 753, 804, 826, 875), append `sandbox_disabled: None,` as the last field. These are error / not-found / new-thread paths where there is no override to report.

If you added placeholder `sandbox_disabled: None` during Task 1 Step 5, the Task 5 Step 1 sites need to change from `None` to `old_meta.sandbox_disabled`; the others are already correct.

- [ ] **Step 3: Build**

Run: `cargo build --release -p omnish-daemon`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "daemon: mirror ThreadMeta.sandbox_disabled into ChatReady (#535)"
```

---

## Task 6: Surface sandbox state in `/thread stats`

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

Context: `format_thread_stats` starts around line 2418 (per earlier grep `/// Returns display string with per-thread stats`). It iterates threads and emits a header + stats per thread.

- [ ] **Step 1: Locate the per-thread header construction**

Open `crates/omnish-daemon/src/server.rs` and read `format_thread_stats` (from line 2418). Find the spot where per-thread metadata is rendered into the output - typically after the thread header, alongside lines like `usage: ...` or `summary: ...`.

- [ ] **Step 2: Emit sandbox line when disabled**

In that rendering block, when `meta.sandbox_disabled == Some(true)`, append a line to the output:

```rust
if meta.sandbox_disabled == Some(true) {
    output.push_str("  sandbox: off\n");
}
```

(Match the indentation style - two spaces before `sandbox:` - used by the surrounding lines for this thread. If the function uses a `writeln!` macro pattern instead of `push_str`, mirror that.)

Only emit when off. Sandbox-on is the default; no noise.

- [ ] **Step 3: Build**

Run: `cargo build --release -p omnish-daemon`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "daemon: show 'sandbox: off' in /thread stats when disabled (#535)"
```

---

## Task 7: Client `/thread sandbox` command handler

**Files:**
- Modify: `crates/omnish-client/src/chat_session.rs`

Context: command dispatch at lines 1307-1317 (for `/thread del` and `/thread list`). Handler definitions follow at line 1829 / 1982. `ChatSession` struct (add `pending_sandbox_off` field there).

- [ ] **Step 1: Add the buffer field to ChatSession**

Find the `pub struct ChatSession { ... }` definition in `chat_session.rs`. Append a field:

```rust
    /// Buffered /thread sandbox preference before a thread exists. Applied
    /// right after ChatReady for a new thread, then cleared.
    pending_sandbox_off: Option<bool>,
```

Also locate the `ChatSession::new` / default constructor and initialize the field: `pending_sandbox_off: None,`.

- [ ] **Step 2: Add dispatch in the command loop**

In `chat_session.rs`, right after the `/thread list` dispatch block (line ~1317), add:

```rust
            // /thread sandbox [on|off]
            if trimmed == "/thread sandbox"
                || trimmed == "/thread sandbox on"
                || trimmed == "/thread sandbox off"
            {
                self.handle_thread_sandbox(trimmed, session_id, rpc).await;
                continue;
            }
```

- [ ] **Step 3: Implement the handler**

In the `impl ChatSession` block in `chat_session.rs`, after `handle_thread_list` (line ~1982 and following), add:

```rust
    async fn handle_thread_sandbox(&mut self, trimmed: &str, session_id: &str, rpc: &RpcClient) {
        let sub = trimmed
            .strip_prefix("/thread sandbox")
            .map(|s| s.trim())
            .unwrap_or("");

        let desired_off: Option<bool> = match sub {
            "" => None,
            "on" => Some(false),
            "off" => Some(true),
            _ => {
                write_stdout(&display::render_error("Usage: /thread sandbox [on|off]"));
                return;
            }
        };

        // No active thread: buffer the preference for apply-after-create.
        if self.current_thread_id.is_none() {
            match desired_off {
                Some(off) => {
                    self.pending_sandbox_off = Some(off);
                    let state = if off { "off" } else { "on" };
                    write_stdout(&display::render_response(&format!(
                        "sandbox preference buffered ({}); will apply when a thread is created",
                        state
                    )));
                }
                None => {
                    let msg = match self.pending_sandbox_off {
                        Some(true) => "no active thread; pending: off".to_string(),
                        Some(false) => "no active thread; pending: on".to_string(),
                        None => "no active thread".to_string(),
                    };
                    write_stdout(&display::render_response(&msg));
                }
            }
            return;
        }

        let tid = self.current_thread_id.as_deref().unwrap();
        let query = match desired_off {
            Some(true) => format!("__cmd:thread sandbox off:{}", tid),
            Some(false) => format!("__cmd:thread sandbox on:{}", tid),
            None => format!("__cmd:thread sandbox:{}", tid),
        };

        let rid = Uuid::new_v4().to_string()[..8].to_string();
        let request = Message::Request(Request {
            request_id: rid.clone(),
            session_id: session_id.to_string(),
            query,
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
                write_stdout(&display::render_error("Failed to update sandbox state"));
            }
        }
    }
```

- [ ] **Step 4: Build**

Run: `cargo build --release -p omnish-client`
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/src/chat_session.rs
git commit -m "client: /thread sandbox [on|off] command + pending buffer (#535)"
```

---

## Task 8: Apply pending preference after new thread is created

**Files:**
- Modify: `crates/omnish-client/src/chat_session.rs`

Context: `ChatReady` for the new-thread creation path is received in the main loop around line 1456. The code path that handles a fresh `ChatReady` is where we need to apply the buffered flag before sending any `ChatMessage`.

- [ ] **Step 1: Find the ChatReady handling for a new thread**

Open `chat_session.rs` and find the `Ok(Message::ChatReady(ready)) if ready.request_id == req_id =>` arm near line 1456 (this is the arm reached after the `ChatStart` we send for a lazily-created thread).

Read ~30 lines of that arm to understand how `thread_id` is captured and what happens between `ChatReady` and the first `ChatMessage`. You need a point where:
- `thread_id` from `ready.thread_id` has been stored into `self.current_thread_id`
- The first `ChatMessage` has NOT yet been dispatched to the daemon

- [ ] **Step 2: Apply the pending flag right there**

Insert (after `self.current_thread_id = Some(ready.thread_id.clone());` or the equivalent line, before the `ChatMessage` is constructed/sent):

```rust
                        if let Some(off) = self.pending_sandbox_off.take() {
                            let arg = if off { "off" } else { "on" };
                            let query = format!("__cmd:thread sandbox {}:{}", arg, &ready.thread_id);
                            let rid = Uuid::new_v4().to_string()[..8].to_string();
                            let req = Message::Request(Request {
                                request_id: rid.clone(),
                                session_id: session_id.to_string(),
                                query,
                                scope: RequestScope::AllSessions,
                            });
                            let _ = rpc.call(req).await;
                        }
```

This is fire-and-await - we ignore the response intentionally; the preference has been persisted by the daemon before the `ChatMessage` is sent, which is the invariant we need.

- [ ] **Step 3: Build**

Run: `cargo build --release -p omnish-client`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/chat_session.rs
git commit -m "client: apply buffered /thread sandbox after new thread created (#535)"
```

---

## Task 9: Resume-warning on ChatReady.sandbox_disabled

**Files:**
- Modify: `crates/omnish-client/src/chat_session.rs`

Context: resume paths at lines 2309 and 2325 handle `ChatReady` for existing threads. The shared entry for user-initiated `/resume` should print the warning once before entering the chat loop.

- [ ] **Step 1: Identify where resume transitions to the chat loop**

Find the resume flow - the code block(s) that receive `ChatReady` with non-empty `ready.thread_id` and `ready.error.is_none()`, then hand control to the interactive loop. Lines ~2309 and ~2325 are both resume flows (one normal, one after a lock retry).

Read around each site to find the common "we are now entering the loop with this thread" point - usually just before invoking the chat I/O loop or setting `self.current_thread_id`.

- [ ] **Step 2: Print yellow warning when disabled**

At the common point (add it once in each resume arm if there's no shared function), add:

```rust
                if ready.sandbox_disabled == Some(true) {
                    write_stdout(&format!(
                        "{YELLOW}⚠ sandbox is OFF for this thread - tool calls bypass Landlock/bwrap{RESET}\r\n"
                    ));
                }
```

If `YELLOW` and `RESET` are not already in scope at this site, either import them (they come from `display::` or are defined at module top - search for `YELLOW` elsewhere in the file) or use `display::render_warning(...)` if such a helper exists.

- [ ] **Step 3: Build**

Run: `cargo build --release -p omnish-client`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/chat_session.rs
git commit -m "client: warn on resume when thread has sandbox disabled (#535)"
```

---

## Task 10: Integration test

**Files:**
- Create: `tools/integration_tests/test_thread_sandbox.sh`
- Modify: `.gitlab-ci.yml`

Context: `lib.sh` provides `test_init`, `enter_chat`, `send_keys`, `send_enter`, `send_special`, `capture_pane`, `restart_client`, `wait_for_client`. Look at `test_sandbox_rules.sh` tests 6-7 (lines ~200+) as the template for runtime bypass verification - they use `sudo ls` (blocked by Landlock) as the probe.

- [ ] **Step 1: Scaffold the script**

Create `tools/integration_tests/test_thread_sandbox.sh` with executable permissions:

```bash
#!/usr/bin/env bash
#
# test_thread_sandbox.sh - Integration tests for /thread sandbox on|off (#535).
#
# Tests:
#   1. /thread sandbox off sets sandbox_disabled in thread meta file
#   2. /thread sandbox off issued before first message (buffered path) applies to new thread
#   3. /thread sandbox on clears the override
#   4. Resume warning renders when thread has sandbox disabled

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. /thread sandbox off persists to thread meta
  2. Buffered /thread sandbox off applies on first message
  3. /thread sandbox on clears override
  4. Resume warning
EOF
}

test_init "thread-sandbox" "$@"

THREADS_DIR="${OMNISH_HOME:-$HOME/.omnish}/threads"

# Helper: read the current thread id from the most recent *.meta.json.
current_thread_id() {
    ls -t "$THREADS_DIR"/*.meta.json 2>/dev/null | head -1 | xargs -n1 basename | sed 's/\.meta\.json$//'
}

# Helper: return 1 if sandbox_disabled is true in the meta file, 0 otherwise.
meta_sandbox_off() {
    local tid="$1"
    local meta_file="$THREADS_DIR/$tid.meta.json"
    [[ -f "$meta_file" ]] || return 1
    grep -q '"sandbox_disabled":true' "$meta_file"
}

# ── Test 1: /thread sandbox off persists to meta ────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: /thread sandbox off persists ===${NC}"
    restart_client
    wait_for_client

    # Create a thread by sending a trivial message, then issue /thread sandbox off
    enter_chat
    send_keys "hello" 0.3
    send_enter 2   # wait for ChatReady + response

    local tid
    tid=$(current_thread_id)
    if [[ -z "$tid" ]]; then
        fail "no thread created"
        return
    fi

    send_keys "/thread sandbox off" 0.3
    send_enter 1

    # Verify the meta file was updated
    if meta_sandbox_off "$tid"; then
        pass "sandbox_disabled=true recorded in thread meta"
    else
        fail "sandbox_disabled not set in $THREADS_DIR/$tid.meta.json"
    fi
}

# ── Test 2: buffered off applies on first message ───────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: buffered /thread sandbox off ===${NC}"
    restart_client
    wait_for_client

    enter_chat
    # Issue /thread sandbox off BEFORE any thread exists
    send_keys "/thread sandbox off" 0.3
    send_enter 1

    # Verify feedback indicates buffering
    local content
    content=$(capture_pane -10)
    if echo "$content" | grep -q "buffered"; then
        pass "/thread sandbox off acknowledges buffering"
    else
        fail "expected 'buffered' feedback, got:\n$content"
    fi

    # Now send a first message to trigger thread creation
    send_keys "hello" 0.3
    send_enter 3

    local tid
    tid=$(current_thread_id)
    if [[ -z "$tid" ]]; then
        fail "no thread created"
        return
    fi

    if meta_sandbox_off "$tid"; then
        pass "buffered off applied to new thread"
    else
        fail "sandbox_disabled not set after buffered toggle"
    fi
}

# ── Test 3: /thread sandbox on clears override ──────────────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: /thread sandbox on clears override ===${NC}"
    restart_client
    wait_for_client

    enter_chat
    send_keys "hello" 0.3
    send_enter 2
    send_keys "/thread sandbox off" 0.3
    send_enter 1

    local tid
    tid=$(current_thread_id)
    meta_sandbox_off "$tid" || { fail "precondition: sandbox_disabled should be true"; return; }

    send_keys "/thread sandbox on" 0.3
    send_enter 1

    # sandbox_disabled field should be absent (skip_serializing_if)
    if meta_sandbox_off "$tid"; then
        fail "sandbox_disabled still true after /thread sandbox on"
    else
        pass "/thread sandbox on clears the override"
    fi
}

# ── Test 4: resume warning ──────────────────────────────────────────────
test_4() {
    echo -e "\n${YELLOW}=== Test 4: resume warning ===${NC}"
    restart_client
    wait_for_client

    enter_chat
    send_keys "hello" 0.3
    send_enter 2
    send_keys "/thread sandbox off" 0.3
    send_enter 1

    # Exit chat, then resume via /resume
    send_special Escape 0.5   # exit chat (if Escape is the exit - otherwise adjust per lib.sh)
    sleep 0.5
    enter_chat
    send_keys "/resume" 0.3
    send_enter 2

    local content
    content=$(capture_pane -30)
    if echo "$content" | grep -qi "sandbox is OFF"; then
        pass "resume warning rendered"
    else
        fail "expected 'sandbox is OFF' in pane, got:\n$content"
    fi
}

run_selected_tests test_1 test_2 test_3 test_4
test_summary
```

- [ ] **Step 2: Make script executable**

Run: `chmod +x tools/integration_tests/test_thread_sandbox.sh`

- [ ] **Step 3: Run the test locally**

Run: `bash tools/integration_tests/test_thread_sandbox.sh`
Expected: all 4 tests pass. If a helper like `fail` / `pass` / `run_selected_tests` / `test_summary` doesn't exist in `lib.sh`, look at the actual helpers used by `test_config_backend.sh` / `test_sandbox_rules.sh` and adapt. The exit sequence from chat (step "Escape" in test 4) may need adjustment - check how other tests exit chat mode.

- [ ] **Step 4: Add to CI**

In `.gitlab-ci.yml`, find the `integration-test` job and the list of test invocations. After the line:

```yaml
    - bash tools/integration_tests/test_config_backend.sh
```

add:

```yaml
    - bash tools/integration_tests/test_thread_sandbox.sh
```

Do the same in the `integration-test-zsh` job (if it exists per the repo's current state - the session history noted it was added).

- [ ] **Step 5: Commit**

```bash
git add tools/integration_tests/test_thread_sandbox.sh .gitlab-ci.yml
git commit -m "test: integration test for /thread sandbox on|off (#535)"
```

---

## Task 11: Documentation and changelog

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `docs/implementation/index.md` (if the per-thread override is worth surfacing)

- [ ] **Step 1: Add CHANGELOG entry**

At the top of `CHANGELOG.md`, under the next unreleased version heading (create one if it doesn't exist - the current released version is v0.8.9 per `Cargo.toml`), add:

```markdown
## v0.8.10 (UNRELEASED)

### Features
- 添加 `/thread sandbox on|off` - per-thread sandbox override; daemon forces
  `sandboxed=false` for all `ChatToolCall` when disabled; state persists in
  `ThreadMeta.sandbox_disabled`; resume shows warning when off (#535).

### Protocol
- Bump `PROTOCOL_VERSION` to v18 (`ChatReady.sandbox_disabled` added,
  backward compatible).
```

Adjust the version number to whatever the current unreleased line is.

- [ ] **Step 2: Update implementation index (optional)**

In `docs/implementation/index.md`, under the `ThreadMeta` bullet in the
`omnish-daemon` / `ConversationManager` section, append:

```markdown
- **sandbox_disabled**：per-thread 沙箱覆盖；`Some(true)` 时强制 `ChatToolCall.sandboxed=false`，
  通过 `/thread sandbox on|off` 切换；在 `ChatReady` 中回传给客户端以便恢复时提示。
```

If the structure doesn't cleanly accommodate this, skip - `CHANGELOG` is sufficient.

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md docs/implementation/index.md
git commit -m "docs: note /thread sandbox in changelog and index (#535)"
```

---

## Task 12: End-to-end smoke test

**Files:** none (manual run).

- [ ] **Step 1: Full workspace build**

Run: `cargo build --release`
Expected: clean build.

- [ ] **Step 2: Full test suite**

Run: `cargo test --release`
Expected: all pass.

- [ ] **Step 3: Run integration tests**

Ask the user to run `bash tools/integration_tests/test_thread_sandbox.sh` themselves (the daemon must be running). Then: `bash tools/integration_tests/test_sandbox_rules.sh` - verify we haven't regressed existing sandbox behavior.

- [ ] **Step 4: Manual verification**

Ask the user to exercise the flow interactively:
- `:hello` → `/thread sandbox off` → verify meta file contains `"sandbox_disabled":true`
- Exit, `:/resume` → verify yellow warning renders
- `/thread stats` → verify "sandbox: off" line appears for that thread
- `/thread sandbox on` → verify field is removed from meta file

- [ ] **Step 5: Close issue**

After the commits are pushed, run:

```bash
git push origin master
glab issue note 535 -m "Implemented in <commit-sha>. See CHANGELOG v0.8.10."
glab issue close 535
```

(Replace `<commit-sha>` with the actual hash of the final commit from this plan.)

---

## Self-Review

- [x] **Spec coverage:** Every spec section mapped. Data model → Task 2. Protocol → Task 1. Daemon wiring: command handler → Task 3, sandbox enforcement → Task 4, `ChatReady` mirror → Task 5, `/thread stats` → Task 6. Client wiring: command → Task 7, pending apply → Task 8, resume warning → Task 9. Testing → Tasks 2 (unit), 10 (integration). Rollout → Tasks 11, 12.
- [x] **Placeholder scan:** No "TBD"; each code block is complete enough to drop in. Task 6 has a soft reference ("find the per-thread rendering block") because `format_thread_stats` isn't quoted in full - engineer reads that function to locate the insertion point.
- [x] **Type consistency:** `sandbox_disabled: Option<bool>` everywhere - in `ThreadMeta`, `ChatReady`, and the setter return (well, setter returns plain `bool` reflecting effective state, which is documented). `pending_sandbox_off: Option<bool>` on client. RPC query grammar is `__cmd:thread sandbox[ on|off]:<tid>` in both the client handler (Task 7) and daemon handler (Task 3).
