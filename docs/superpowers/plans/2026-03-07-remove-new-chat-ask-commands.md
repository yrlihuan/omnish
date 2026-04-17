# Remove /new, /chat, /ask Commands Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Remove `/new`, `/chat`, `/ask` commands so users can start chatting immediately after entering chat mode.

**Architecture:** Delete command handling logic, update command registry, remove documentation references, and ensure automatic thread creation already works when `current_thread_id` is `None`.

**Tech Stack:** Rust, omnish-client, omnish-daemon

---

## Current State Analysis

The `/new`, `/chat`, and `/ask` commands are currently handled identically in `crates/omnish-client/src/main.rs:1540-1559`:
- All three commands send `Message::ChatStart` with `new_thread: true`
- They set `current_thread_id` and display "(new conversation)" message

However, the chat loop already has automatic thread creation logic at lines 1857-1874:
```rust
// Lazily create thread if not yet initialized
if current_thread_id.is_none() {
    // Creates new thread automatically
}
```

The `CHAT_ONLY_COMMANDS` constant in `crates/omnish-client/src/command.rs:165` includes `["/chat", "/ask", "/resume", "/new"]`.

Documentation in `crates/omnish-llm/src/template.rs:73` mentions `/new` command.

## Plan Overview

1. **Remove command handling logic** from `run_chat_loop`
2. **Update CHAT_ONLY_COMMANDS** constant to remove deleted commands
3. **Update completable_commands()** function
4. **Remove daemon-side command handling** (if any)
5. **Update documentation** in template and other files
6. **Test the changes** to ensure chat mode works correctly

### Task 1: Remove Command Handling from run_chat_loop

**Files:**
- Modify: `crates/omnish-client/src/main.rs:1540-1559`

**Step 1: Locate the command handling code**

The current code at lines 1540-1559:
```rust
// /new - start new thread within chat
if trimmed == "/new" || trimmed == "/chat" || trimmed == "/ask" {
    let req_id = Uuid::new_v4().to_string()[..8].to_string();
    let new_msg = Message::ChatStart(ChatStart {
        request_id: req_id.clone(),
        session_id: session_id.to_string(),
        new_thread: true,
    });
    match rpc.call(new_msg).await {
        Ok(Message::ChatReady(ready)) if ready.request_id == req_id => {
            current_thread_id = Some(ready.thread_id);
            let info = "\r\n\x1b[2;37m(new conversation)\x1b[0m";
            nix::unistd::write(std::io::stdout(), info.as_bytes()).ok();
        }
        _ => {
            let err = display::render_error("Failed to create new thread");
            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
        }
    }
    continue;
}
```

**Step 2: Remove the entire if block**

Delete lines 1540-1559.

**Step 3: Run tests to verify build still works**

Run: `cargo test -p omnish-client`
Expected: All existing tests pass

**Step 4: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat: remove /new, /chat, /ask command handling from chat loop"
```

### Task 2: Update CHAT_ONLY_COMMANDS Constant

**Files:**
- Modify: `crates/omnish-client/src/command.rs:165`

**Step 1: Locate the constant definition**

Current line 165:
```rust
pub const CHAT_ONLY_COMMANDS: &[&str] = &["/chat", "/ask", "/resume", "/new"];
```

**Step 2: Remove /chat, /ask, /new from the array**

Update to:
```rust
pub const CHAT_ONLY_COMMANDS: &[&str] = &["/resume"];
```

**Step 3: Run tests to verify build still works**

Run: `cargo test -p omnish-client`
Expected: All existing tests pass

**Step 4: Commit**

```bash
git add crates/omnish-client/src/command.rs
git commit -m "feat: remove /new, /chat, /ask from CHAT_ONLY_COMMANDS"
```

### Task 3: Update completable_commands() Function

**Files:**
- Modify: `crates/omnish-client/src/command.rs:174-176`

**Step 1: Check current function implementation**

Lines 174-176:
```rust
for cmd in CHAT_ONLY_COMMANDS {
    cmds.push(cmd.to_string());
}
```

**Step 2: No changes needed**

Since we already updated `CHAT_ONLY_COMMANDS` in Task 2, the `completable_commands()` function will automatically exclude `/new`, `/chat`, `/ask`.

**Step 3: Run tests to verify build still works**

Run: `cargo test -p omnish-client`
Expected: All existing tests pass

**Step 4: Commit** (if any changes were made)

No commit needed if no changes.

### Task 4: Remove Daemon-Side Command Handling

**Files:**
- Search: `crates/omnish-daemon/src/server.rs` for references to `/new`, `/chat`, `/ask`

**Step 1: Search for command handling**

Run: `grep -n "/new\|/chat\|/ask" crates/omnish-daemon/src/server.rs`

**Step 2: Check if any daemon-side handling exists**

If no references found, skip this task.

**Step 3: If references exist, remove them**

Remove any command handling logic for `/new`, `/chat`, `/ask`.

**Step 4: Run tests to verify build still works**

Run: `cargo test -p omnish-daemon`
Expected: All existing tests pass

**Step 5: Commit** (if changes were made)

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "feat: remove daemon-side /new, /chat, /ask command handling"
```

### Task 5: Update Documentation

**Files:**
- Modify: `crates/omnish-llm/src/template.rs:73`
- Modify: `config/README.md` (if references exist)
- Modify: `docs/plans/2026-03-06-chat-mode-arrow-history.md` (if references exist)
- Modify: `CHANGELOG.md` (if references exist)

**Step 1: Update template documentation**

Current line 73 in `crates/omnish-llm/src/template.rs`:
```rust
- /new - Start a new conversation thread
```

Update to remove `/new` reference:
```rust
// Remove the /new line entirely
```

**Step 2: Check other documentation files**

Search for references:
```bash
grep -r "/new\|/chat\|/ask" docs/ config/ CHANGELOG.md --include="*.md"
```

**Step 3: Remove all documentation references**

For each file found, remove or update references to `/new`, `/chat`, `/ask` commands.

**Step 4: Run tests to verify build still works**

Run: `cargo test`
Expected: All existing tests pass

**Step 5: Commit**

```bash
git add crates/omnish-llm/src/template.rs docs/ config/ CHANGELOG.md
git commit -m "docs: remove /new, /chat, /ask command references from documentation"
```

### Task 6: Test Chat Mode Functionality

**Step 1: Build the project**

```bash
cargo build --release
```

**Step 2: Test manual chat mode entry**

1. Start omnish-client: `./target/release/omnish-client`
2. Type `:` to enter chat mode
3. Type a message (e.g., "Hello")
4. Verify:
   - Chat mode starts immediately
   - No need for `/new`, `/chat`, `/ask` commands
   - Thread is automatically created
   - LLM responds correctly

**Step 3: Test chat history navigation**

1. Enter chat mode
2. Send multiple messages
3. Use up/down arrows to navigate history (implemented in issue #149)
4. Verify arrow navigation works correctly

**Step 4: Test /resume command still works**

1. Enter chat mode
2. Type `/resume` or `/resume 1`
3. Verify resume functionality still works

**Step 5: Commit if any fixes needed**

If issues found during testing, fix them and commit.

### Task 7: Verify Integration Tests

**Files:**
- Check: `tools/integration_tests/test_chat_history.sh`

**Step 1: Review integration test**

Check if the test uses `/new`, `/chat`, `/ask` commands.

**Step 2: Update test if needed**

If test references deleted commands, update to use direct chat entry instead.

**Step 3: Run integration test**

```bash
cargo build
tools/integration_tests/test_chat_history.sh
```

Expected: All tests pass

**Step 4: Commit if test changes were made**

```bash
git add tools/integration_tests/test_chat_history.sh
git commit -m "test: update integration test for removed /new, /chat, /ask commands"
```

---

**Plan complete and saved to `docs/plans/2026-03-07-remove-new-chat-ask-commands.md`. Two execution options:**

**1. Subagent-Driven (this session)** - I dispatch fresh subagent per task, review between tasks, fast iteration

**2. Parallel Session (separate)** - Open new session with executing-plans, batch execution with checkpoints

**Which approach?**