# Picker Widget Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a terminal picker widget (single + multi select) that renders at the bottom of the terminal by pushing content up.

**Architecture:** New `picker.rs` module in omnish-client with `pick_one()` and `pick_many()` public functions. Internally uses a shared `run_picker()` core that handles rendering, input, and cleanup. Rendering uses ANSI escape sequences written directly to stdout via `nix::unistd::write`, input via `nix::unistd::read` — consistent with existing `read_chat_input` pattern.

**Tech Stack:** Rust, nix (read/write), libc (ioctl for terminal size, poll for ESC detection), vt100 (test-only)

---

# Picker Widget Design

## Summary

A terminal selection widget for omnish that supports single-select and multi-select modes. Renders at the bottom of the terminal by pushing existing content upward, preserving the user's visual context.

## Use Cases

- `/threads del` — select conversation(s) to delete
- `/resume` — select conversation to resume
- Configuration selection (e.g., LLM backend)

## API

```rust
// crates/omnish-client/src/picker.rs

/// Single select: returns the selected index (0-based), or None on ESC.
pub fn pick_one(title: &str, items: &[&str]) -> Option<usize>

/// Multi select: returns selected indices (0-based), or None on ESC.
pub fn pick_many(title: &str, items: &[&str]) -> Option<Vec<usize>>
```

Prerequisite: terminal must be in raw mode (already the case inside chat loop).

## Rendering

### Bottom push-in approach

1. Calculate total lines: N = 1 (title) + 1 (separator) + items.len() + 1 (separator) + 1 (hint)
2. Print N `\r\n` to push screen content up
3. Move cursor back up N lines (`\x1b[{N}A`)
4. Render the widget in the created space

### Layout

Single select:
```
Title text
──────────────────────────────────────
  [1] 5m ago  | 4 turns | What is 2+2
> [2] 1h ago  | 2 turns | 三原色          ← highlighted
  [3] 20h ago | 3 turns | 我的问题
──────────────────────────────────────
↑↓ move  Enter confirm  ESC cancel
```

Multi select:
```
Title text
──────────────────────────────────────
  [ ] [1] 5m ago  | 4 turns | What is 2+2
> [x] [2] 1h ago  | 2 turns | 三原色       ← cursor here, checked
  [ ] [3] 20h ago | 3 turns | 我的问题
──────────────────────────────────────
↑↓ move  Space select  Enter confirm  ESC cancel
```

### Highlight style

- Current item: `> ` prefix + bold/reverse video
- Non-current item: `  ` prefix + normal text
- Multi-select checked: `[x]`, unchecked: `[ ]`

## Input Handling

| Key | Single mode | Multi mode |
|-----|-------------|------------|
| ↑/↓ | Move cursor | Move cursor |
| Enter | Return `Some(index)` | Return `Some(checked_indices)` |
| ESC | Return `None` | Return `None` |
| Space | — | Toggle check on current item |

On ↑/↓: only redraw the two changed lines (old cursor, new cursor) for efficiency.

## Cleanup

1. Move cursor to the first line of the widget (title line)
2. `\x1b[J` — erase from cursor to end of screen
3. Cursor is now back at the original position

## Implementation

- New module: `crates/omnish-client/src/picker.rs`
- Internal `PickerMode` enum (`Single` / `Multi`) controls behavior
- Shared core loop handles rendering and input
- Arrow key parsing reuses ESC sequence detection (direct byte reading, same pattern as `read_chat_input`)
- Uses `nix::unistd::read` for stdin, `nix::unistd::write` for stdout (consistent with existing code)

---

## Implementation Tasks

### Task 1: Rendering functions and unit tests

**Files:**
- Create: `crates/omnish-client/src/picker.rs`
- Modify: `crates/omnish-client/src/main.rs:2` (add `mod picker;`)

**Step 1: Create picker.rs with rendering functions and tests**

Create `crates/omnish-client/src/picker.rs` with:

```rust
use std::os::unix::io::AsRawFd;

/// Get terminal width, fallback to 80.
fn terminal_cols() -> u16 {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 && ws.ws_col > 0 { ws.ws_col } else { 80 }
}

/// Separator line spanning `cols` columns (dim ─ characters).
fn render_separator(cols: u16) -> String {
    format!("\r\x1b[2m{}\x1b[0m", "─".repeat(cols as usize))
}

/// Render a single item line.
/// - `selected`: this is the cursor row (render with `> ` prefix + bold)
/// - `checked`: only used in multi mode (render `[x]` or `[ ]`)
/// - `multi`: whether to show checkboxes
fn render_item(text: &str, selected: bool, checked: bool, multi: bool) -> String {
    let prefix = if selected { "> " } else { "  " };
    let checkbox = if multi {
        if checked { "[x] " } else { "[ ] " }
    } else {
        ""
    };
    if selected {
        format!("\r\x1b[1;7m{}{}{}\x1b[0m\x1b[K", prefix, checkbox, text)
    } else {
        format!("\r{}{}{}\x1b[K", prefix, checkbox, text)
    }
}

/// Render the hint line at the bottom.
fn render_hint(multi: bool) -> String {
    let hint = if multi {
        "↑↓ move  Space select  Enter confirm  ESC cancel"
    } else {
        "↑↓ move  Enter confirm  ESC cancel"
    };
    format!("\r\x1b[2m{}\x1b[0m\x1b[K", hint)
}

/// Render the full picker widget (initial draw).
/// Returns the ANSI string to write to stdout.
fn render_full(title: &str, items: &[&str], cursor: usize, checked: &[bool], multi: bool, cols: u16) -> String {
    let total_lines = 1 + 1 + items.len() + 1 + 1; // title + sep + items + sep + hint
    let mut out = String::new();

    // Push screen content up by printing N blank lines
    for _ in 0..total_lines {
        out.push_str("\r\n");
    }
    // Move cursor back up
    out.push_str(&format!("\x1b[{}A", total_lines));

    // Title
    out.push_str(&format!("\r\x1b[1m{}\x1b[0m\x1b[K", title));
    out.push_str("\r\n");

    // Top separator
    out.push_str(&render_separator(cols));
    out.push_str("\r\n");

    // Items
    for (i, item) in items.iter().enumerate() {
        out.push_str(&render_item(item, i == cursor, checked[i], multi));
        if i < items.len() - 1 {
            out.push_str("\r\n");
        }
    }
    out.push_str("\r\n");

    // Bottom separator
    out.push_str(&render_separator(cols));
    out.push_str("\r\n");

    // Hint
    out.push_str(&render_hint(multi));

    out
}

/// Render cleanup: move cursor to title line and erase everything below.
fn render_cleanup(items_len: usize) -> String {
    let total_lines = 1 + 1 + items_len + 1 + 1; // title + sep + items + sep + hint
    // Move up to title line (we're on the hint line, so go up total_lines - 1)
    let up = total_lines - 1;
    format!("\x1b[{}A\r\x1b[J", up)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ansi(input: &str, cols: u16, rows: u16) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(input.as_bytes());
        parser
    }

    fn get_row(screen: &vt100::Screen, row: u16, cols: u16) -> String {
        screen.rows(0, cols).nth(row as usize).unwrap_or_default()
    }

    #[test]
    fn test_render_item_normal() {
        let output = render_item("hello", false, false, false);
        let p = parse_ansi(&output, 40, 5);
        let row = get_row(p.screen(), 0, 40);
        assert!(row.contains("hello"));
        assert!(row.starts_with("  "), "normal item should have 2-space prefix");
    }

    #[test]
    fn test_render_item_selected() {
        let output = render_item("hello", true, false, false);
        let p = parse_ansi(&output, 40, 5);
        let row = get_row(p.screen(), 0, 40);
        assert!(row.contains("> "));
        assert!(row.contains("hello"));
    }

    #[test]
    fn test_render_item_multi_checked() {
        let output = render_item("hello", false, true, true);
        let p = parse_ansi(&output, 40, 5);
        let row = get_row(p.screen(), 0, 40);
        assert!(row.contains("[x]"));
        assert!(row.contains("hello"));
    }

    #[test]
    fn test_render_item_multi_unchecked() {
        let output = render_item("hello", true, false, true);
        let p = parse_ansi(&output, 40, 5);
        let row = get_row(p.screen(), 0, 40);
        assert!(row.contains("[ ]"));
        assert!(row.contains("> "));
    }

    #[test]
    fn test_render_hint_single() {
        let output = render_hint(false);
        let p = parse_ansi(&output, 80, 5);
        let row = get_row(p.screen(), 0, 80);
        assert!(row.contains("Enter confirm"));
        assert!(!row.contains("Space"));
    }

    #[test]
    fn test_render_hint_multi() {
        let output = render_hint(true);
        let p = parse_ansi(&output, 80, 5);
        let row = get_row(p.screen(), 0, 80);
        assert!(row.contains("Space select"));
        assert!(row.contains("Enter confirm"));
    }

    #[test]
    fn test_render_full_single_select() {
        let items = &["item A", "item B", "item C"];
        let checked = vec![false; 3];
        let output = render_full("Pick one:", items, 1, &checked, false, 40);
        let p = parse_ansi(&output, 40, 20);
        let screen = p.screen();
        let all = screen.contents();

        assert!(all.contains("Pick one:"), "title should be visible");
        assert!(all.contains("item A"));
        assert!(all.contains("item B"));
        assert!(all.contains("item C"));
        assert!(all.contains("Enter confirm"));

        // Item B (index 1) should be highlighted with ">"
        // Find the row containing "item B"
        for i in 0..20 {
            let row = get_row(screen, i, 40);
            if row.contains("item B") {
                assert!(row.contains(">"), "selected item should have > prefix");
            }
            if row.contains("item A") {
                assert!(!row.contains(">"), "non-selected item should not have >");
            }
        }
    }

    #[test]
    fn test_render_full_multi_select() {
        let items = &["item A", "item B"];
        let checked = vec![false, true];
        let output = render_full("Pick many:", items, 0, &checked, true, 40);
        let p = parse_ansi(&output, 40, 20);
        let screen = p.screen();
        let all = screen.contents();

        assert!(all.contains("Pick many:"));
        assert!(all.contains("Space select"));

        for i in 0..20 {
            let row = get_row(screen, i, 40);
            if row.contains("item A") {
                assert!(row.contains("[ ]"), "unchecked item A");
            }
            if row.contains("item B") {
                assert!(row.contains("[x]"), "checked item B");
            }
        }
    }

    #[test]
    fn test_render_cleanup_erases_widget() {
        let items = &["A", "B", "C"];
        let checked = vec![false; 3];
        let mut output = render_full("Title", items, 0, &checked, false, 40);
        output.push_str(&render_cleanup(items.len()));

        let p = parse_ansi(&output, 40, 20);
        let all = p.screen().contents();
        // After cleanup, all widget content should be erased
        assert!(!all.contains("Title"), "title should be erased");
        assert!(!all.contains("A"), "items should be erased");
        assert!(!all.contains("Enter confirm"), "hint should be erased");
    }
}
```

**Step 2: Add `mod picker;` to main.rs**

Add `mod picker;` after line 2 in `crates/omnish-client/src/main.rs` (alongside other mod declarations).

**Step 3: Run tests**

Run: `cargo test -p omnish-client picker`
Expected: All 8 tests pass.

**Step 4: Commit**

```
feat(picker): add rendering functions with unit tests (issue #155)
```

---

### Task 2: Input loop and public API

**Files:**
- Modify: `crates/omnish-client/src/picker.rs`

**Step 1: Add the input loop and public functions**

Add to `picker.rs` above `#[cfg(test)]`:

```rust
/// Parse escape sequence after ESC byte (same approach as main.rs parse_escape_sequence).
fn parse_esc_seq(stdin_fd: i32) -> Option<[u8; 2]> {
    let mut pfd = libc::pollfd { fd: stdin_fd, events: libc::POLLIN, revents: 0 };
    let ready = unsafe { libc::poll(&mut pfd, 1, 50) };
    if ready <= 0 {
        return None;
    }
    let mut seq = [0u8; 2];
    if nix::unistd::read(stdin_fd, &mut seq[0..1]) != Ok(1) {
        return None;
    }
    if seq[0] == b'[' {
        if nix::unistd::read(stdin_fd, &mut seq[1..2]) == Ok(1) {
            return Some(seq);
        }
    }
    None
}

/// Rewrite a single item line in-place (cursor must be on that line).
fn redraw_item(text: &str, selected: bool, checked: bool, multi: bool) {
    let line = render_item(text, selected, checked, multi);
    nix::unistd::write(std::io::stdout(), line.as_bytes()).ok();
}

/// Core picker loop. Returns selected index(es) or None on ESC.
fn run_picker(title: &str, items: &[&str], multi: bool) -> Option<Vec<usize>> {
    if items.is_empty() {
        return None;
    }

    let cols = terminal_cols();
    let mut cursor: usize = 0;
    let mut checked = vec![false; items.len()];

    // Initial render
    let full = render_full(title, items, cursor, &checked, multi, cols);
    nix::unistd::write(std::io::stdout(), full.as_bytes()).ok();

    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut byte = [0u8; 1];

    loop {
        match nix::unistd::read(stdin_fd, &mut byte) {
            Ok(1) => match byte[0] {
                0x1b => {
                    // Check for arrow key sequence
                    if let Some(seq) = parse_esc_seq(stdin_fd) {
                        if seq[0] == b'[' {
                            match seq[1] {
                                b'A' if cursor > 0 => { // Up
                                    let old = cursor;
                                    cursor -= 1;
                                    // Move to old cursor line and redraw it
                                    // Items start at line offset 2 (title + separator)
                                    // Current position is hint line (bottom)
                                    // Go up from hint to the old item line
                                    let up_to_old = (items.len() - old) + 1; // +1 for bottom separator
                                    let s = format!("\x1b[{}A", up_to_old);
                                    nix::unistd::write(std::io::stdout(), s.as_bytes()).ok();
                                    redraw_item(items[old], false, checked[old], multi);
                                    // Move up one more to the new cursor line
                                    nix::unistd::write(std::io::stdout(), b"\x1b[1A").ok();
                                    redraw_item(items[cursor], true, checked[cursor], multi);
                                    // Move back down to hint line
                                    let down = (items.len() - cursor) + 1;
                                    let s = format!("\x1b[{}B", down);
                                    nix::unistd::write(std::io::stdout(), s.as_bytes()).ok();
                                }
                                b'B' if cursor < items.len() - 1 => { // Down
                                    let old = cursor;
                                    cursor += 1;
                                    let up_to_old = (items.len() - old) + 1;
                                    let s = format!("\x1b[{}A", up_to_old);
                                    nix::unistd::write(std::io::stdout(), s.as_bytes()).ok();
                                    redraw_item(items[old], false, checked[old], multi);
                                    // Move down one to the new cursor line
                                    nix::unistd::write(std::io::stdout(), b"\x1b[1B").ok();
                                    redraw_item(items[cursor], true, checked[cursor], multi);
                                    let down = (items.len() - cursor) + 1;
                                    let s = format!("\x1b[{}B", down);
                                    nix::unistd::write(std::io::stdout(), s.as_bytes()).ok();
                                }
                                _ => {} // Ignore other sequences
                            }
                        }
                    } else {
                        // Bare ESC — cancel
                        let cleanup = render_cleanup(items.len());
                        nix::unistd::write(std::io::stdout(), cleanup.as_bytes()).ok();
                        return None;
                    }
                }
                b' ' if multi => {
                    // Toggle check on current item
                    checked[cursor] = !checked[cursor];
                    // Redraw current item in place
                    let up = (items.len() - cursor) + 1;
                    let s = format!("\x1b[{}A", up);
                    nix::unistd::write(std::io::stdout(), s.as_bytes()).ok();
                    redraw_item(items[cursor], true, checked[cursor], multi);
                    let down = (items.len() - cursor) + 1;
                    let s = format!("\x1b[{}B", down);
                    nix::unistd::write(std::io::stdout(), s.as_bytes()).ok();
                }
                b'\r' | b'\n' => {
                    // Confirm
                    let cleanup = render_cleanup(items.len());
                    nix::unistd::write(std::io::stdout(), cleanup.as_bytes()).ok();
                    if multi {
                        let selected: Vec<usize> = checked.iter()
                            .enumerate()
                            .filter(|(_, &c)| c)
                            .map(|(i, _)| i)
                            .collect();
                        return Some(selected);
                    } else {
                        return Some(vec![cursor]);
                    }
                }
                _ => {} // Ignore other input
            },
            _ => break,
        }
    }

    let cleanup = render_cleanup(items.len());
    nix::unistd::write(std::io::stdout(), cleanup.as_bytes()).ok();
    None
}

/// Single select: returns the selected index (0-based), or None on ESC.
pub fn pick_one(title: &str, items: &[&str]) -> Option<usize> {
    run_picker(title, items, false).map(|v| v[0])
}

/// Multi select: returns selected indices (0-based), or None on ESC.
pub fn pick_many(title: &str, items: &[&str]) -> Option<Vec<usize>> {
    run_picker(title, items, true)
}
```

**Step 2: Build**

Run: `cargo build -p omnish-client`
Expected: Compiles without errors.

**Step 3: Run all picker tests**

Run: `cargo test -p omnish-client picker`
Expected: All 8 rendering tests still pass.

**Step 4: Commit**

```
feat(picker): add input loop with pick_one/pick_many API (issue #155)
```

---

### Task 3: Integration test

**Files:**
- Create: `tools/integration_tests/test_picker.sh`

**Step 1: Create integration test**

Create `tools/integration_tests/test_picker.sh`:

This test requires a caller command that invokes the picker. Since the picker isn't wired into any command yet, this task will be done after Task 4 (wiring into `/threads del`). For now, verify manually:

1. Build: `cargo build --release`
2. Start omnish-client
3. Enter chat mode (`:`)
4. Type `/threads del` (no number)
5. Verify picker appears with conversation list
6. Use ↑↓ to move, Enter to select, ESC to cancel

**Step 2: Commit**

```
test(picker): manual verification of picker widget
```

---

### Task 4: Wire picker into existing commands (optional, separate issue)

This task is out of scope for the picker widget itself. The picker module exposes `pick_one()` and `pick_many()` — callers like `/threads del`, `/resume` can be wired in separate issues. The picker module is self-contained and testable on its own.
