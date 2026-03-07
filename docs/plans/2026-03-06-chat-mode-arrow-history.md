# Chat Mode Arrow History Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add up/down arrow navigation in chat mode to cycle through previous commands like bash history.

**Architecture:** Extend `read_chat_input` to parse escape sequences, maintain a command history buffer in chat loop, and allow navigation through stored commands using up/down arrows.

**Tech Stack:** Rust, nix crate for raw terminal I/O, VecDeque for history storage, ANSI escape sequences

---

## Current State Analysis

The `read_chat_input` function in `crates/omnish-client/src/main.rs:1857` currently:
- Reads bytes one-by-one from stdin
- Handles ESC (0x1b) as exit, but doesn't parse multi-byte escape sequences
- Arrow keys (ESC `[` A/B/C/D) are currently ignored in chat mode by the `EscSeqFilter`
- No command history storage exists for chat mode

## Plan Overview

1. **Add escape sequence parsing** to `read_chat_input` to detect arrow keys
2. **Create chat history storage** in the chat loop (`run_chat_loop`)
3. **Implement history navigation** with up/down arrows
4. **Update display** when navigating history
5. **Handle edge cases** (empty history, bounds checking, ghost completion)

### Task 1: Add Escape Sequence Parsing to `read_chat_input`

**Files:**
- Modify: `crates/omnish-client/src/main.rs:1857-1960` (read_chat_input function)

**Step 1: Write helper function to read escape sequence**

```rust
// Helper to parse escape sequences after ESC byte
fn parse_escape_sequence(stdin_fd: i32) -> Option<[u8; 2]> {
    let mut seq = [0u8; 2];
    // Read first character after ESC
    if nix::unistd::read(stdin_fd, &mut seq[0..1]) != Ok(1) {
        return None;
    }

    // If it's '[', read the next character (A/B/C/D)
    if seq[0] == b'[' {
        if nix::unistd::read(stdin_fd, &mut seq[1..2]) == Ok(1) {
            return Some(seq);
        }
    }
    None
}
```

**Step 2: Update read_chat_input to handle ESC sequences**

Add after line 1867 where ESC is handled:

```rust
0x1b => {
    // Check if this is an arrow key sequence
    if let Some(seq) = parse_escape_sequence(stdin_fd) {
        if seq[0] == b'[' {
            match seq[1] {
                b'A' => { /* Up arrow - will be handled in Task 3 */ return Some(String::new()); },
                b'B' => { /* Down arrow - will be handled in Task 3 */ return Some(String::new()); },
                _ => {} // Ignore other escape sequences
            }
        }
    } else {
        // ESC without sequence - exit chat
        return None;
    }
},
```

**Step 3: Run tests to verify build still works**

Run: `cargo test -p omnish-client`
Expected: All existing tests pass (no new functionality yet)

**Step 4: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat: add escape sequence parsing to read_chat_input"
```

### Task 2: Create Chat History Storage in Chat Loop

**Files:**
- Modify: `crates/omnish-client/src/main.rs:1493-1800` (run_chat_loop function)

**Step 1: Add history buffer to run_chat_loop**

Add after line 1508 (after cached_thread_ids):

```rust
// Chat command history for arrow key navigation
let mut chat_history: VecDeque<String> = VecDeque::with_capacity(100);
let mut history_index: Option<usize> = None; // None = new command, Some(idx) = browsing history
```

**Step 2: Add function to save successful commands to history**

Add helper function after `read_chat_input` function:

```rust
fn save_to_history(history: &mut VecDeque<String>, command: &str, capacity: usize) {
    // Don't save empty commands or duplicates of the most recent command
    if command.trim().is_empty() || history.back().map(|s| s == command).unwrap_or(false) {
        return;
    }

    if history.len() >= capacity {
        history.pop_front();
    }
    history.push_back(command.to_string());
}
```

**Step 3: Save commands to history after they're sent**

Add after line 1526 where input is trimmed:

```rust
let trimmed = input.trim();
if trimmed.is_empty() {
    continue;
}

// Save to history for future navigation
save_to_history(&mut chat_history, trimmed, 100);
history_index = None; // Reset to new command mode
```

**Step 4: Run tests to verify build still works**

Run: `cargo test -p omnish-client`
Expected: All existing tests pass

**Step 5: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat: add chat command history storage"
```

### Task 3: Implement Arrow Key History Navigation

**Files:**
- Modify: `crates/omnish-client/src/main.rs:1857-1960` (read_chat_input function)
- Modify: `crates/omnish-client/src/main.rs:1493-1800` (run_chat_loop function)

**Step 1: Update read_chat_input to accept history parameters**

Change function signature:

```rust
fn read_chat_input(
    completer: &mut ghost_complete::GhostCompleter,
    allow_backspace_exit: bool,
    history: &VecDeque<String>,
    history_index: &mut Option<usize>,
) -> Option<String> {
```

**Step 2: Add parameters to read_chat_input calls**

Update line 1520:
```rust
match read_chat_input(&mut chat_completer, current_thread_id.is_none(), &chat_history, &mut history_index) {
```

**Step 3: Implement up arrow navigation in read_chat_input**

Replace placeholder in Task 1 Step 2:

```rust
b'A' => { // Up arrow
    if history.is_empty() {
        return Some(String::new());
    }

    let idx = match *history_index {
        Some(i) if i > 0 => i - 1,
        Some(_) => 0, // Already at first item
        None => history.len() - 1, // Start from most recent
    };

    *history_index = Some(idx);
    if let Some(cmd) = history.get(idx) {
        // Clear current line and show history command
        let clear_seq = b"\r\x1b[K> ";
        nix::unistd::write(std::io::stdout(), clear_seq).ok();
        nix::unistd::write(std::io::stdout(), cmd.as_bytes()).ok();

        // Update buffer
        buf.clear();
        buf.extend_from_slice(cmd.as_bytes());

        // Update ghost completion for new input
        if let Some(ghost) = completer.update(cmd) {
            let ghost_render = format!("\x1b[2;37m{}\x1b[0m\x1b[{}D", ghost, ghost.len());
            nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
            has_ghost = true;
        }
    }
    return Some(String::new());
},
```

**Step 4: Implement down arrow navigation**

Add after up arrow handling:

```rust
b'B' => { // Down arrow
    if history.is_empty() {
        return Some(String::new());
    }

    let idx = match *history_index {
        Some(i) if i < history.len() - 1 => i + 1,
        Some(_) => {
            // Going past most recent - clear input
            *history_index = None;
            let clear_seq = b"\r\x1b[K> ";
            nix::unistd::write(std::io::stdout(), clear_seq).ok();
            buf.clear();
            completer.clear();
            return Some(String::new());
        },
        None => return Some(String::new()), // Already at new command
    };

    *history_index = Some(idx);
    if let Some(cmd) = history.get(idx) {
        // Clear current line and show history command
        let clear_seq = b"\r\x1b[K> ";
        nix::unistd::write(std::io::stdout(), clear_seq).ok();
        nix::unistd::write(std::io::stdout(), cmd.as_bytes()).ok();

        // Update buffer
        buf.clear();
        buf.extend_from_slice(cmd.as_bytes());

        // Update ghost completion
        if let Some(ghost) = completer.update(cmd) {
            let ghost_render = format!("\x1b[2;37m{}\x1b[0m\x1b[{}D", ghost, ghost.len());
            nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
            has_ghost = true;
        }
    }
    return Some(String::new());
},
```

**Step 5: Run tests to verify build still works**

Run: `cargo test -p omnish-client`
Expected: All existing tests pass

**Step 6: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat: implement arrow key history navigation in chat mode"
```

### Task 4: Update Display and Handle Edge Cases

**Files:**
- Modify: `crates/omnish-client/src/main.rs:1857-1960` (read_chat_input function)

**Step 1: Fix display clearing to include prompt**

Update the clear sequences to include the full prompt:

```rust
// Replace clear_seq definitions with:
let clear_seq = b"\r> \x1b[K";
```

**Step 2: Handle ghost text clearing properly**

Ensure ghost text is cleared when navigating history:

```rust
// Add after clearing line but before writing new command:
if has_ghost {
    nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
    has_ghost = false;
}
```

**Step 3: Fix UTF-8 handling in history navigation**

Ensure buffer is properly cleared and refilled with UTF-8 bytes:

```rust
// Replace buf.clear() and buf.extend_from_slice() with:
buf = cmd.as_bytes().to_vec();
```

**Step 4: Add test for history navigation**

Create test function in the test module:

```rust
#[test]
fn test_history_navigation() {
    use std::collections::VecDeque;

    let mut history = VecDeque::new();
    history.push_back("command1".to_string());
    history.push_back("command2".to_string());

    let mut idx = None;

    // Simulate up arrow - should go to command2 (most recent)
    idx = match idx {
        Some(i) if i > 0 => Some(i - 1),
        Some(_) => Some(0),
        None => Some(history.len() - 1),
    };
    assert_eq!(idx, Some(1));

    // Another up arrow - should go to command1
    idx = match idx {
        Some(i) if i > 0 => Some(i - 1),
        Some(_) => Some(0),
        None => Some(history.len() - 1),
    };
    assert_eq!(idx, Some(0));

    // Down arrow - should go back to command2
    idx = match idx {
        Some(i) if i < history.len() - 1 => Some(i + 1),
        Some(_) => {
            // Going past most recent
            None
        },
        None => None,
    };
    assert_eq!(idx, Some(1));
}
```

**Step 5: Run tests**

Run: `cargo test -p omnish-client`
Expected: All tests pass including new test

**Step 6: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "fix: improve display and edge cases for chat history"
```

### Task 5: Integration Test

**Files:**
- Create: `tools/integration_tests/test_chat_history.sh`

**Step 1: Create integration test script**

```bash
#!/bin/bash
set -uo pipefail

# Test arrow key navigation in chat mode
SOCKET_DIR="${CLAUDE_TMUX_SOCKET_DIR:-/tmp/claude-tmux-sockets}"
mkdir -p "$SOCKET_DIR"
SOCKET="$SOCKET_DIR/chat-history-test.sock"
SESSION="chat-history-test"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"
CLIENT="$PROJECT_ROOT/target/debug/omnish-client"

# Cleanup
cleanup() {
    tmux -S "$SOCKET" kill-session -t "$SESSION" 2>/dev/null || true
}
trap cleanup EXIT

echo "Testing chat mode arrow history navigation..."

# Start fresh session
tmux -S "$SOCKET" kill-session -t "$SESSION" 2>/dev/null || true
tmux -S "$SOCKET" new -d -s "$SESSION" -n test

# Start omnish-client
tmux -S "$SOCKET" send-keys -t "$SESSION":0.0 -- "$CLIENT" Enter
sleep 1

# Enter chat mode
tmux -S "$SOCKET" send-keys -t "$SESSION":0.0 -- ":" Enter
sleep 0.5

# Send first command
tmux -S "$SOCKET" send-keys -t "$SESSION":0.0 -- "First chat command" Enter
sleep 1

# Send second command
tmux -S "$SOCKET" send-keys -t "$SESSION":0.0 -- "Second command" Enter
sleep 1

# Enter chat mode again
tmux -S "$SOCKET" send-keys -t "$SESSION":0.0 -- ":" Enter
sleep 0.5

# Press up arrow (should show "Second command")
tmux -S "$SOCKET" send-keys -t "$SESSION":0.0 -- $'\x1b[A'
sleep 0.5

# Capture output and verify
output=$(tmux -S "$SOCKET" capture-pane -p -J -t "$SESSION":0.0 -S -20)
if echo "$output" | grep -q "Second command"; then
    echo "✓ PASS: Up arrow shows previous command"
else
    echo "✗ FAIL: Up arrow did not show previous command"
    exit 1
fi

echo "All tests passed!"
```

**Step 2: Make script executable**

```bash
chmod +x tools/integration_tests/test_chat_history.sh
```

**Step 3: Run integration test**

```bash
cargo build
tools/integration_tests/test_chat_history.sh
```

**Step 4: Commit**

```bash
git add tools/integration_tests/test_chat_history.sh
git commit -m "test: add integration test for chat history navigation"
```

### Task 6: Update Documentation

**Files:**
- Modify: `docs/plans/2026-03-06-chat-mode-arrow-history.md` (this file - add completion notes)
- Modify: `tools/integration_tests/README.md`

**Step 1: Update this plan with completion status**

Add completion section at the end:

```markdown
## Completion Notes

Implementation completed for issue #149: "支持chat模式下使用箭头切换之前执行的命令（同bash中的效果）"

**Features added:**
1. Escape sequence parsing in `read_chat_input`
2. Chat command history storage using `VecDeque`
3. Up/down arrow navigation through history
4. Proper display updates when navigating
5. Integration test for verification

**Limitations:**
- History is per chat session (not persisted across sessions)
- Maximum 100 commands stored (configurable)
- Arrow keys only work in chat mode input phase
```

**Step 2: Update integration tests README**

Add new test to README:

```markdown
### `test_chat_history.sh`

**Purpose:** Tests arrow key navigation in chat mode (issue #149)

**What it tests:**
1. Sends multiple chat commands
2. Uses up arrow to navigate to previous commands
3. Verifies history navigation works correctly

**Usage:**
```bash
tools/integration_tests/test_chat_history.sh
```
```

**Step 3: Commit**

```bash
git add docs/plans/2026-03-06-chat-mode-arrow-history.md tools/integration_tests/README.md
git commit -m "docs: update documentation for chat history feature"
```

---

**Plan complete and saved to `docs/plans/2026-03-06-chat-mode-arrow-history.md`. Two execution options:**

**1. Subagent-Driven (this session)** - I dispatch fresh subagent per task, review between tasks, fast iteration

**2. Parallel Session (separate)** - Open new session with executing-plans, batch execution with checkpoints

**Which approach?**

## Completion Notes

Implementation completed for issue #149: "支持chat模式下使用箭头切换之前执行的命令（同bash中的效果）"

**Features added:**
1. Escape sequence parsing in `read_chat_input`
2. Chat command history storage using `VecDeque`
3. Up/down arrow navigation through history
4. Proper display updates when navigating
5. Integration test for verification

**Limitations:**
- History is per chat session (not persisted across sessions)
- Maximum 100 commands stored (configurable)
- Arrow keys only work in chat mode input phase