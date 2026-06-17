# ServerPush (daemon -> client) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a generic `Message::ServerPush { kind, payload }` push type so the daemon can push content to clients, with `thread_title_update` as the first use case (live tmux title refresh during chat).

**Architecture:** Daemon broadcasts `ServerPush` through the existing `PushRegistry` after `thread_summary` generates a new `title_word`. A client-side always-running dispatcher task is the sole `push_rx` consumer: it stores `thread_title_update` into a shared `PendingTitles` map and forwards other push messages to the shell loop via a fallback channel. The chat render thread applies pending titles at safe points (so escape-sequence bytes are only ever written by one thread; full stdout serialization is deferred to issue #644).

**Tech Stack:** Rust, bincode (protocol), tokio (mpsc, tasks), serde_json (payload).

**Spec:** `docs/superpowers/specs/2026-06-16-server-push-thread-title-design.md`

---

## File Structure

- `crates/omnish-protocol/src/message.rs` - new `ServerPush` variant, version bump, guard tests.
- `crates/omnish-daemon/src/task_mgr.rs` - add `push_registry` to `DaemonContext`.
- `crates/omnish-daemon/src/main.rs` - create shared `PushRegistry`, wire into `DaemonContext` and `DaemonServer::new`.
- `crates/omnish-daemon/src/server.rs` - `DaemonServer::new` accepts the registry instead of self-creating it.
- `crates/omnish-daemon/src/push.rs` (new) - `broadcast_server_push` helper.
- `crates/omnish-daemon/src/thread_summary.rs` - broadcast after generating `title_word`.
- `crates/omnish-client/src/main.rs` - `PendingTitles` type, dispatcher task, fallback channel, main loop switch, `enter_chat_mode` wiring.
- `crates/omnish-client/src/chat_session.rs` - `pending_titles` field, `pick_pending_title` + `apply_pending_title`, call sites.

---

## Task 1: Protocol - add `ServerPush` variant

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs:8` (versions), `:160` (enum tail), `:707` (guard), `:965` (index pin)

- [ ] **Step 1: Write the failing round-trip test**

Add to the `tests` module in `crates/omnish-protocol/src/message.rs`:

```rust
#[test]
fn server_push_round_trip() {
    let msg = Message::ServerPush {
        kind: "thread_title_update".to_string(),
        payload: serde_json::json!({ "thread_id": "t1", "title": "deploy" }),
    };
    let bytes = bincode::serialize(&msg).expect("serialize");
    let back: Message = bincode::deserialize(&bytes).expect("deserialize");
    match back {
        Message::ServerPush { kind, payload } => {
            assert_eq!(kind, "thread_title_update");
            assert_eq!(payload["thread_id"], "t1");
            assert_eq!(payload["title"], "deploy");
        }
        _ => panic!("wrong variant"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-protocol server_push_round_trip`
Expected: FAIL (compile error: no variant `ServerPush`).

- [ ] **Step 3: Add the variant at the enum end**

In `crates/omnish-protocol/src/message.rs`, after `PluginSyncRequest { hostname: String },` (line 160), before the closing `}` of the enum:

```rust
    /// Generic daemon -> client push. `kind` names the semantics, `payload`
    /// is the kind-specific JSON body. Client dispatches on `kind`.
    /// First kind: "thread_title_update" with payload { thread_id, title }.
    /// PROTOCOL_VERSION 26.
    ServerPush { kind: String, payload: serde_json::Value },
```

- [ ] **Step 4: Bump version**

In `crates/omnish-protocol/src/message.rs`:
- Line 8: `pub const PROTOCOL_VERSION: u32 = 26;`
- Line 16: leave `MIN_COMPATIBLE_VERSION` at `25` (append-only, backward compatible).

- [ ] **Step 5: Update the variant guard count + exhaustive list**

In `message_variant_guard` (line 707): change `EXPECTED_VARIANT_COUNT` from `37` to `38`. Add to the `variants` vec right after `Message::PluginSyncRequest { hostname: String::new() },` (line 882):

```rust
            Message::ServerPush { kind: String::new(), payload: serde_json::Value::Null },
```

And add to the exhaustive match arm (line 922-924), extend the `| Message::PluginSyncRequest { .. } => {}` group:

```rust
                | Message::PluginSyncRequest { .. }
                | Message::ServerPush { .. } => {}
```

- [ ] **Step 6: Pin the new variant index**

In `variant_indices_are_stable` (after line 965):

```rust
        assert_eq!(variant_index(&Message::ServerPush { kind: String::new(), payload: serde_json::Value::Null }), 37, "ServerPush index shifted");
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p omnish-protocol`
Expected: PASS (including `server_push_round_trip`, `message_variant_guard`, `variant_indices_are_stable`).

- [ ] **Step 8: Commit**

```bash
git add crates/omnish-protocol/src/message.rs
git commit -m "feat(protocol): add generic ServerPush daemon-to-client message"
```

---

## Task 2: Daemon - expose `push_registry` to scheduled tasks

**Files:**
- Modify: `crates/omnish-daemon/src/task_mgr.rs:14-28` (DaemonContext)
- Modify: `crates/omnish-daemon/src/server.rs:505-532` (DaemonServer::new signature)
- Modify: `crates/omnish-daemon/src/main.rs:238-243, 404`

- [ ] **Step 1: Add field to `DaemonContext`**

In `crates/omnish-daemon/src/task_mgr.rs`, add to `pub struct DaemonContext` (after `plugin_bundler`):

```rust
    pub push_registry: omnish_transport::rpc_server::PushRegistry,
```

Add the import at the top of `task_mgr.rs` if not present:

```rust
use omnish_transport::rpc_server::PushRegistry;
```

(If you add the `use`, write the field as `pub push_registry: PushRegistry,`.)

- [ ] **Step 2: Make `DaemonServer::new` take the registry**

In `crates/omnish-daemon/src/server.rs`, add a parameter to `pub fn new(...)` (line 505):

```rust
        push_registry: PushRegistry,
```

Replace the self-created field at line 532:

```rust
            push_registry,
```

(`PushRegistry` and `OnPushConnect` are already imported at `server.rs:9`.)

- [ ] **Step 3: Create the shared registry in main and wire both consumers**

In `crates/omnish-daemon/src/main.rs`, immediately before line 238 (`let daemon_ctx = ...`):

```rust
    let push_registry: omnish_transport::rpc_server::PushRegistry =
        std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
```

Add the field inside the `DaemonContext { ... }` literal (after `plugin_bundler: Arc::clone(&plugin_bundler),`):

```rust
        push_registry: push_registry.clone(),
```

Pass it into `DaemonServer::new(...)` at line 404 as the final argument:

```rust
    let server = DaemonServer::new(session_mgr, llm_backend, task_mgr, conv_mgr, plugin_mgr.clone(), tool_registry.clone(), server_opts, formatter_mgr, Arc::clone(&update_cache), Arc::clone(&plugin_bundler), push_registry);
```

- [ ] **Step 4: Build to verify wiring compiles**

Run: `cargo build --release -p omnish-daemon`
Expected: builds clean. Fix any call sites of `DaemonServer::new` in tests if the compiler flags them (pass a fresh `Arc::new(tokio::sync::Mutex::new(HashMap::new()))`).

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/task_mgr.rs crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/main.rs
git commit -m "feat(daemon): share push_registry with scheduled tasks via DaemonContext"
```

---

## Task 3: Daemon - broadcast helper + thread_summary push

**Files:**
- Create: `crates/omnish-daemon/src/push.rs`
- Modify: `crates/omnish-daemon/src/lib.rs:22` to add `pub mod push;`
- Modify: `crates/omnish-daemon/src/thread_summary.rs:50, 74-78, 215-228`

- [ ] **Step 1: Write the failing broadcast test**

Create `crates/omnish-daemon/src/push.rs`:

```rust
//! Daemon -> client broadcast helpers over the shared PushRegistry.

use omnish_protocol::message::Message;
use omnish_transport::rpc_server::PushRegistry;

/// Broadcast a generic ServerPush to every connected client. Clients filter
/// by `kind`/payload. Mirrors `deploy::broadcast_notice`.
pub async fn broadcast_server_push(registry: &PushRegistry, kind: &str, payload: serde_json::Value) {
    let senders: Vec<_> = { registry.lock().await.values().cloned().collect() };
    for tx in senders {
        let _ = tx
            .send(Message::ServerPush { kind: kind.to_string(), payload: payload.clone() })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    #[tokio::test]
    async fn broadcast_reaches_registered_sender() {
        let registry: PushRegistry = Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = mpsc::channel(4);
        registry.lock().await.insert(1u64, tx);

        broadcast_server_push(&registry, "thread_title_update", serde_json::json!({ "thread_id": "t1", "title": "deploy" })).await;

        let msg = rx.recv().await.expect("message");
        match msg {
            Message::ServerPush { kind, payload } => {
                assert_eq!(kind, "thread_title_update");
                assert_eq!(payload["thread_id"], "t1");
                assert_eq!(payload["title"], "deploy");
            }
            _ => panic!("wrong variant"),
        }
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/omnish-daemon/src/lib.rs`, add next to `pub mod deploy;` (line 22):

```rust
pub mod push;
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test -p omnish-daemon broadcast_reaches_registered_sender`
Expected: PASS.

- [ ] **Step 4: Thread the registry into `generate_thread_summaries`**

In `crates/omnish-daemon/src/thread_summary.rs`:

Change the call in `create_job` (line 50). First capture the registry near the other clones (after line 43):

```rust
        let push_registry = ctx.daemon.push_registry.clone();
```

Move it into the async closure (add `let push_registry = push_registry.clone();` next to the other per-tick clones around line 45-47), then update the call (line 50):

```rust
                if let Err(e) = generate_thread_summaries(&conv_mgr, Some(llm.as_ref()), &language, &push_registry).await {
```

Update the function signature (line 74):

```rust
async fn generate_thread_summaries(
    conv_mgr: &ConversationManager,
    llm_backend: Option<&dyn LlmBackend>,
    language: &str,
    push_registry: &omnish_transport::rpc_server::PushRegistry,
) -> anyhow::Result<()> {
```

- [ ] **Step 5: Broadcast after a new title_word is generated**

In `thread_summary.rs`, inside the `if !word.is_empty()` block (right after `updated_meta.title_word = Some(word.clone());`, around line 190):

```rust
                        crate::push::broadcast_server_push(
                            push_registry,
                            "thread_title_update",
                            serde_json::json!({ "thread_id": thread_id, "title": word }),
                        ).await;
```

- [ ] **Step 6: Fix the existing test call sites**

The unit tests in `thread_summary.rs` call `generate_thread_summaries(&conv_mgr, None, "en")`. Update each to pass an empty registry:

```rust
        let registry = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        generate_thread_summaries(&conv_mgr, None, "en", &registry).await.unwrap();
```

(Apply to every call in the test module - around lines 256, 268, and the `test_title_override_not_touched_by_summary_loop` test.)

- [ ] **Step 7: Run daemon tests**

Run: `cargo test -p omnish-daemon`
Expected: PASS (existing thread_summary tests + new broadcast test).

- [ ] **Step 8: Commit**

```bash
git add crates/omnish-daemon/src/push.rs crates/omnish-daemon/src/lib.rs crates/omnish-daemon/src/thread_summary.rs
git commit -m "feat(daemon): broadcast ServerPush thread_title_update from thread_summary"
```

---

## Task 4: Client - dispatcher task + fallback channel

**Files:**
- Modify: `crates/omnish-client/src/main.rs` (type alias near top; setup near line 784-796; main loop at 1842)

- [ ] **Step 1: Write the failing routing test**

Add near the top-level items of `crates/omnish-client/src/main.rs` a pure routing function and a test. First the function (place it next to `apply_client_config_changes`, ~line 1919):

```rust
/// Shared map of thread_id -> latest pushed title, awaiting application by the
/// chat render thread.
pub type PendingTitles = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>;

/// Handle a pushed message. Returns `Some(msg)` for messages that should be
/// forwarded to the shell loop's existing handling; returns `None` when the
/// dispatcher fully handled it (currently: ServerPush).
fn route_push(msg: Message, pending: &PendingTitles) -> Option<Message> {
    match msg {
        Message::ServerPush { kind, payload } => {
            if kind == "thread_title_update" {
                if let (Some(tid), Some(title)) =
                    (payload["thread_id"].as_str(), payload["title"].as_str())
                {
                    pending.lock().unwrap().insert(tid.to_string(), title.to_string());
                }
            } else {
                event_log::push(format!("unknown ServerPush kind: {kind}"));
            }
            None
        }
        other => Some(other),
    }
}
```

Then the test (in the `#[cfg(test)]` module of `main.rs`):

```rust
#[test]
fn route_push_stores_title_and_forwards_others() {
    let pending: PendingTitles =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

    // ServerPush is handled (returns None) and stored.
    let handled = route_push(
        Message::ServerPush {
            kind: "thread_title_update".into(),
            payload: serde_json::json!({ "thread_id": "t1", "title": "deploy" }),
        },
        &pending,
    );
    assert!(handled.is_none());
    assert_eq!(pending.lock().unwrap().get("t1").map(String::as_str), Some("deploy"));

    // Other messages are forwarded.
    let forwarded = route_push(Message::ConfigClient { changes: vec![] }, &pending);
    assert!(matches!(forwarded, Some(Message::ConfigClient { .. })));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-client route_push_stores_title_and_forwards_others`
Expected: FAIL (compile error: `route_push` / `PendingTitles` not defined) until Step 1 functions are in place; then PASS. If it already passes after adding Step 1 code, continue.

- [ ] **Step 3: Create the shared map + fallback channel + dispatcher task**

In `crates/omnish-client/src/main.rs`, in the block where `rpc` is available (after line 791 `let rpc_poll = rpc.clone();`, before the polling `tokio::spawn`):

```rust
        let pending_titles: PendingTitles =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let (fallback_tx, mut fallback_rx) =
            tokio::sync::mpsc::channel::<Message>(256);

        // Dispatcher: sole consumer of push_rx. Handles ServerPush directly,
        // forwards ConfigClient/NoticePush to the shell loop via fallback_rx.
        let rpc_push = rpc.clone();
        let pending_push = pending_titles.clone();
        tokio::spawn(async move {
            loop {
                while let Some(msg) = rpc_push.try_recv_push().await {
                    if let Some(forward) = route_push(msg, &pending_push) {
                        if fallback_tx.send(forward).await.is_err() {
                            return; // receiver gone
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
```

Note: `pending_titles` must remain in scope for the main loop and `enter_chat_mode` (Task 5). If the surrounding `if let Some(ref rpc) = daemon_conn { ... }` block ends before the main loop, hoist `pending_titles` and `fallback_rx` declarations above that block and assign inside, or restructure so both are visible to the main `loop`. Verify scope by reading the function around lines 784-870 and 1840.

- [ ] **Step 4: Switch the main loop to drain the fallback channel**

In `crates/omnish-client/src/main.rs`, replace the push-draining block at lines 1840-1876. Change:

```rust
        // Check for pushed config changes from daemon (non-blocking)
        if let Some(ref rpc) = daemon_conn {
            while let Some(msg) = rpc.try_recv_push().await {
                match msg {
```

to drain `fallback_rx` instead:

```rust
        // Drain push messages forwarded by the dispatcher task.
        while let Ok(msg) = fallback_rx.try_recv() {
            match msg {
```

Keep the inner `Message::ConfigClient` and `Message::NoticePush` arms and the trailing `_ => {}` unchanged; just remove the now-unneeded `if let Some(ref rpc) = daemon_conn` wrapper and its closing brace, and the outer `while let Some(msg) = rpc.try_recv_push()` line.

- [ ] **Step 5: Build + run client tests**

Run: `cargo build --release -p omnish-client && cargo test -p omnish-client route_push_stores_title_and_forwards_others`
Expected: builds clean; test PASSES.

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat(client): add push dispatcher task and PendingTitles for ServerPush"
```

---

## Task 5: Client - apply pending title in chat render thread

**Files:**
- Modify: `crates/omnish-client/src/chat_session.rs:44-83` (struct), `:1058-1101` (new), `:1379-1409` (loop + read_input_with)
- Modify: `crates/omnish-client/src/main.rs:2776-2815` (enter_chat_mode signature + ChatSession::new call)

- [ ] **Step 1: Write the failing picker test**

Add a pure helper + test in `crates/omnish-client/src/chat_session.rs`. Helper (free function near `write_stdout`, ~line 88):

```rust
/// Remove and return the pending title for `current` thread, if any.
fn pick_pending_title(pending: &crate::PendingTitles, current: Option<&str>) -> Option<String> {
    let tid = current?;
    pending.lock().unwrap().remove(tid)
}
```

Test (in the chat_session `#[cfg(test)]` module):

```rust
#[test]
fn pick_pending_title_matches_current_thread() {
    let pending: crate::PendingTitles =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    pending.lock().unwrap().insert("t1".into(), "deploy".into());

    // Non-matching thread: nothing taken, entry stays.
    assert_eq!(pick_pending_title(&pending, Some("other")), None);
    assert_eq!(pick_pending_title(&pending, None), None);
    assert!(pending.lock().unwrap().contains_key("t1"));

    // Matching thread: taken and removed.
    assert_eq!(pick_pending_title(&pending, Some("t1")), Some("deploy".to_string()));
    assert!(pending.lock().unwrap().is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-client pick_pending_title_matches_current_thread`
Expected: FAIL (compile error) until helper added; then PASS.

- [ ] **Step 3: Add `pending_titles` field to ChatSession**

In `crates/omnish-client/src/chat_session.rs`, add to `pub struct ChatSession` (after `screen_capture` field, line 82):

```rust
    /// Shared map of thread_id -> pushed title, applied at safe render points.
    pending_titles: crate::PendingTitles,
```

- [ ] **Step 4: Accept it in the constructor**

In `ChatSession::new` (line 1058), add a parameter:

```rust
        pending_titles: crate::PendingTitles,
```

And set it in the `Self { ... }` literal (after `screen_capture,`, line 1099):

```rust
            pending_titles,
```

- [ ] **Step 5: Add the apply method**

Add to `impl ChatSession` (near `thread_id`, ~line 1111):

```rust
    /// Apply a pushed title for the current thread, if one is pending. Called
    /// only from the chat render flow so the tmux escape is written by a single
    /// thread (see issue #644).
    fn apply_pending_title(&self) {
        if let Some(title) = pick_pending_title(&self.pending_titles, self.current_thread_id.as_deref()) {
            super::set_chat_tmux_title(Some(&title));
        }
    }
```

- [ ] **Step 6: Call it at safe points**

In the chat loop (`crates/omnish-client/src/chat_session.rs:1379`), at the very top of the `loop {` body, before the prompt is drawn:

```rust
        loop {
            self.apply_pending_title();
```

And in `read_input_with` (line ~3933-3935), right after the `libc::poll` timeout returns with no input ready (the branch that loops again when `ready == 0`), call:

```rust
                self.apply_pending_title();
```

Read lines 3900-3960 first to place this in the no-input poll-timeout branch (so an idle prompt still refreshes). Do not place it where it would run mid escape sequence.

- [ ] **Step 7: Pass `pending_titles` through `enter_chat_mode`**

In `crates/omnish-client/src/main.rs`, add a parameter to `enter_chat_mode` (signature at line 2776):

```rust
    pending_titles: &PendingTitles,
```

Pass it at the `ChatSession::new(...)` call (line 2810):

```rust
            let mut session = chat_session::ChatSession::new(
                std::mem::take(chat_history),
                config.shell.extended_unicode,
                Arc::clone(&sandbox_state),
                Arc::clone(&screen_capture),
                pending_titles.clone(),
            );
```

Then update every `enter_chat_mode(...)` call site to pass `&pending_titles` (find them: `grep -n "enter_chat_mode(" crates/omnish-client/src/main.rs`). `pending_titles` is in scope from Task 4 Step 3.

- [ ] **Step 8: Build + run client tests**

Run: `cargo build --release -p omnish-client && cargo test -p omnish-client`
Expected: builds clean; all tests PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/omnish-client/src/chat_session.rs crates/omnish-client/src/main.rs
git commit -m "feat(client): apply pushed thread title at safe chat render points"
```

---

## Task 6: Full build + manual verification

**Files:** none (verification only)

- [ ] **Step 1: Full release build**

Run: `cargo build --release`
Expected: workspace builds clean.

- [ ] **Step 2: Full test suite**

Run: `cargo test --workspace`
Expected: all PASS.

- [ ] **Step 3: Manual end-to-end check (ask the user to run the daemon/client)**

Ask the user to start the daemon and a client under tmux (per CLAUDE.md, do not run the daemon yourself). To shorten the wait, temporarily set `thread_summary` schedule to run every minute (it already defaults to `* * * * *`). Steps to verify:
1. Enter chat, exchange enough rounds to trigger a summary (check daemon logs for `thread_summary: generated title_word`).
2. While idle at the `> ` prompt, confirm the tmux window name changes to `> <word>` without typing.
3. Confirm chat rendering shows no escape-sequence corruption during/after the title change.
4. Confirm `/thread rename` still updates the title immediately (unchanged path).
5. Confirm config changes and notices (deploy/plugin install) still display in shell mode (fallback channel path).

- [ ] **Step 4: Final commit (if any verification fixups were needed)**

```bash
git add -A
git commit -m "chore: ServerPush thread title verification fixups"
```

---

## Notes for the implementer

- Protocol: `ServerPush` MUST stay at the end of the `Message` enum; bincode keys variants by index.
- The dispatcher MUST use `try_recv_push` polling, never `recv().await` held across awaits, or it will block the reconnect path that swaps `push_rx` (`omnish-transport/src/rpc_client.rs:451-455`).
- The dispatcher MUST be the only `push_rx` consumer; the shell loop now reads `fallback_rx`, not `try_recv_push`.
- The tmux title escape is written only by the chat render thread (via `apply_pending_title`). Full stdout serialization is out of scope and tracked in issue #644.
