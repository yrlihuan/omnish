# Line Editor Widget Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extract a reusable `LineEditor` widget supporting multi-line editing with cursor movement, and refactor `read_chat_input()` to use it.

**Architecture:** Create `widgets/` module under omnish-client with `LineEditor` as pure state (no I/O). Migrate inline editing logic from `read_chat_input()` to use `LineEditor` methods. Move existing `picker.rs` into `widgets/`.

**Tech Stack:** Rust, `unicode-width` crate for CJK display width

---

### Task 1: Create widgets module and LineEditor struct with basic insert/content

**Files:**
- Create: `crates/omnish-client/src/widgets/mod.rs`
- Create: `crates/omnish-client/src/widgets/line_editor.rs`
- Modify: `crates/omnish-client/src/main.rs` (add `mod widgets;`)

**Step 1: Write failing tests**

In `crates/omnish-client/src/widgets/line_editor.rs`:

```rust
use unicode_width::UnicodeWidthChar;

pub struct LineEditor {
    lines: Vec<Vec<char>>,
    cursor: (usize, usize), // (row, col) in char indices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_editor_is_empty() {
        let ed = LineEditor::new();
        assert!(ed.is_empty());
        assert_eq!(ed.content(), "");
        assert_eq!(ed.cursor(), (0, 0));
        assert_eq!(ed.line_count(), 1);
    }

    #[test]
    fn test_insert_chars() {
        let mut ed = LineEditor::new();
        ed.insert('h');
        ed.insert('i');
        assert_eq!(ed.content(), "hi");
        assert_eq!(ed.cursor(), (0, 2));
        assert!(!ed.is_empty());
    }

    #[test]
    fn test_insert_cjk() {
        let mut ed = LineEditor::new();
        ed.insert('你');
        ed.insert('好');
        assert_eq!(ed.content(), "你好");
        assert_eq!(ed.cursor(), (0, 2));
        assert_eq!(ed.cursor_display_col(), 4); // each CJK char is 2 columns wide
    }

    #[test]
    fn test_set_content() {
        let mut ed = LineEditor::new();
        ed.set_content("hello\nworld");
        assert_eq!(ed.line_count(), 2);
        assert_eq!(ed.content(), "hello\nworld");
        assert_eq!(ed.cursor(), (1, 5)); // cursor at end
    }

    #[test]
    fn test_line_accessor() {
        let mut ed = LineEditor::new();
        ed.set_content("abc");
        assert_eq!(ed.line(0), &['a', 'b', 'c']);
    }
}
```

**Step 2: Implement LineEditor basics**

In the same file, above the tests:

```rust
impl LineEditor {
    pub fn new() -> Self {
        Self {
            lines: vec![vec![]],
            cursor: (0, 0),
        }
    }

    pub fn insert(&mut self, ch: char) {
        let (row, col) = self.cursor;
        self.lines[row].insert(col, ch);
        self.cursor.1 += 1;
    }

    pub fn content(&self) -> String {
        self.lines.iter()
            .map(|line| line.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    pub fn cursor_display_col(&self) -> usize {
        let (row, col) = self.cursor;
        self.lines[row][..col].iter()
            .map(|c| UnicodeWidthChar::width(*c).unwrap_or(1))
            .sum()
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, row: usize) -> &[char] {
        &self.lines[row]
    }

    pub fn set_content(&mut self, s: &str) {
        self.lines = if s.is_empty() {
            vec![vec![]]
        } else {
            s.lines().map(|l| l.chars().collect()).collect()
        };
        // Handle trailing newline edge case
        if s.ends_with('\n') {
            self.lines.push(vec![]);
        }
        let last_row = self.lines.len() - 1;
        let last_col = self.lines[last_row].len();
        self.cursor = (last_row, last_col);
    }
}
```

**Step 3: Create module files**

`crates/omnish-client/src/widgets/mod.rs`:
```rust
pub mod line_editor;
```

Add to `crates/omnish-client/src/main.rs` (near other mod declarations):
```rust
mod widgets;
```

**Step 4: Run tests**

Run: `cargo test -p omnish-client -- widgets::line_editor`
Expected: All 5 tests pass

**Step 5: Commit**

```
feat(widgets): add LineEditor struct with basic insert and content
```

---

### Task 2: Add cursor movement methods

**Files:**
- Modify: `crates/omnish-client/src/widgets/line_editor.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn test_move_left_right() {
    let mut ed = LineEditor::new();
    ed.set_content("abc");
    assert_eq!(ed.cursor(), (0, 3));
    ed.move_left();
    assert_eq!(ed.cursor(), (0, 2));
    ed.move_right();
    assert_eq!(ed.cursor(), (0, 3));
    ed.move_right(); // at end, no-op
    assert_eq!(ed.cursor(), (0, 3));
}

#[test]
fn test_move_left_at_start() {
    let mut ed = LineEditor::new();
    ed.move_left(); // at (0,0), no-op
    assert_eq!(ed.cursor(), (0, 0));
}

#[test]
fn test_move_left_wraps_to_prev_line() {
    let mut ed = LineEditor::new();
    ed.set_content("ab\ncd");
    ed.cursor = (1, 0); // start of second line
    ed.move_left();
    assert_eq!(ed.cursor(), (0, 2)); // end of first line
}

#[test]
fn test_move_right_wraps_to_next_line() {
    let mut ed = LineEditor::new();
    ed.set_content("ab\ncd");
    ed.cursor = (0, 2); // end of first line
    ed.move_right();
    assert_eq!(ed.cursor(), (1, 0)); // start of second line
}

#[test]
fn test_move_up_down() {
    let mut ed = LineEditor::new();
    ed.set_content("hello\nhi");
    ed.cursor = (1, 2); // end of "hi"
    ed.move_up();
    assert_eq!(ed.cursor(), (0, 2)); // col clamped to same position
    ed.move_down();
    assert_eq!(ed.cursor(), (1, 2));
}

#[test]
fn test_move_up_clamps_col() {
    let mut ed = LineEditor::new();
    ed.set_content("hi\nhello");
    // cursor at (1, 5) - end of "hello"
    ed.move_up();
    assert_eq!(ed.cursor(), (0, 2)); // "hi" only has 2 chars
}

#[test]
fn test_move_home_end() {
    let mut ed = LineEditor::new();
    ed.set_content("hello");
    assert_eq!(ed.cursor(), (0, 5));
    ed.move_home();
    assert_eq!(ed.cursor(), (0, 0));
    ed.move_end();
    assert_eq!(ed.cursor(), (0, 5));
}
```

**Step 2: Implement cursor movement**

```rust
pub fn move_left(&mut self) {
    let (row, col) = self.cursor;
    if col > 0 {
        self.cursor.1 -= 1;
    } else if row > 0 {
        self.cursor.0 -= 1;
        self.cursor.1 = self.lines[row - 1].len();
    }
}

pub fn move_right(&mut self) {
    let (row, col) = self.cursor;
    if col < self.lines[row].len() {
        self.cursor.1 += 1;
    } else if row < self.lines.len() - 1 {
        self.cursor.0 += 1;
        self.cursor.1 = 0;
    }
}

pub fn move_up(&mut self) {
    if self.cursor.0 > 0 {
        self.cursor.0 -= 1;
        let line_len = self.lines[self.cursor.0].len();
        if self.cursor.1 > line_len {
            self.cursor.1 = line_len;
        }
    }
}

pub fn move_down(&mut self) {
    if self.cursor.0 < self.lines.len() - 1 {
        self.cursor.0 += 1;
        let line_len = self.lines[self.cursor.0].len();
        if self.cursor.1 > line_len {
            self.cursor.1 = line_len;
        }
    }
}

pub fn move_home(&mut self) {
    self.cursor.1 = 0;
}

pub fn move_end(&mut self) {
    self.cursor.1 = self.lines[self.cursor.0].len();
}
```

**Step 3: Run tests**

Run: `cargo test -p omnish-client -- widgets::line_editor`
Expected: All 12 tests pass

**Step 4: Commit**

```
feat(widgets): add cursor movement to LineEditor
```

---

### Task 3: Add word movement and delete operations

**Files:**
- Modify: `crates/omnish-client/src/widgets/line_editor.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn test_move_word_left() {
    let mut ed = LineEditor::new();
    ed.set_content("hello world");
    ed.move_word_left();
    assert_eq!(ed.cursor(), (0, 6)); // before "world"
    ed.move_word_left();
    assert_eq!(ed.cursor(), (0, 0)); // before "hello"
}

#[test]
fn test_move_word_right() {
    let mut ed = LineEditor::new();
    ed.set_content("hello world");
    ed.cursor = (0, 0);
    ed.move_word_right();
    assert_eq!(ed.cursor(), (0, 5)); // after "hello"
    ed.move_word_right();
    assert_eq!(ed.cursor(), (0, 11)); // after "world"
}

#[test]
fn test_delete_back() {
    let mut ed = LineEditor::new();
    ed.set_content("abc");
    ed.delete_back();
    assert_eq!(ed.content(), "ab");
    assert_eq!(ed.cursor(), (0, 2));
}

#[test]
fn test_delete_back_at_start_returns_false() {
    let mut ed = LineEditor::new();
    assert!(!ed.delete_back());
}

#[test]
fn test_delete_back_merges_lines() {
    let mut ed = LineEditor::new();
    ed.set_content("ab\ncd");
    ed.cursor = (1, 0);
    ed.delete_back();
    assert_eq!(ed.content(), "abcd");
    assert_eq!(ed.cursor(), (0, 2));
    assert_eq!(ed.line_count(), 1);
}

#[test]
fn test_delete_forward() {
    let mut ed = LineEditor::new();
    ed.set_content("abc");
    ed.cursor = (0, 1);
    ed.delete_forward();
    assert_eq!(ed.content(), "ac");
    assert_eq!(ed.cursor(), (0, 1));
}

#[test]
fn test_delete_forward_merges_lines() {
    let mut ed = LineEditor::new();
    ed.set_content("ab\ncd");
    ed.cursor = (0, 2);
    ed.delete_forward();
    assert_eq!(ed.content(), "abcd");
    assert_eq!(ed.line_count(), 1);
}

#[test]
fn test_kill_to_start() {
    let mut ed = LineEditor::new();
    ed.set_content("hello world");
    ed.cursor = (0, 5);
    ed.kill_to_start();
    assert_eq!(ed.content(), " world");
    assert_eq!(ed.cursor(), (0, 0));
}

#[test]
fn test_newline() {
    let mut ed = LineEditor::new();
    ed.set_content("abcd");
    ed.cursor = (0, 2);
    ed.newline();
    assert_eq!(ed.content(), "ab\ncd");
    assert_eq!(ed.cursor(), (1, 0));
    assert_eq!(ed.line_count(), 2);
}

#[test]
fn test_insert_mid_line() {
    let mut ed = LineEditor::new();
    ed.set_content("ac");
    ed.cursor = (0, 1);
    ed.insert('b');
    assert_eq!(ed.content(), "abc");
    assert_eq!(ed.cursor(), (0, 2));
}
```

**Step 2: Implement operations**

```rust
pub fn move_word_left(&mut self) {
    let (row, col) = self.cursor;
    let line = &self.lines[row];
    if col == 0 {
        return;
    }
    let mut i = col;
    // Skip whitespace
    while i > 0 && line[i - 1].is_whitespace() {
        i -= 1;
    }
    // Skip word chars
    while i > 0 && !line[i - 1].is_whitespace() {
        i -= 1;
    }
    self.cursor.1 = i;
}

pub fn move_word_right(&mut self) {
    let (row, col) = self.cursor;
    let line = &self.lines[row];
    let len = line.len();
    if col >= len {
        return;
    }
    let mut i = col;
    // Skip word chars
    while i < len && !line[i].is_whitespace() {
        i += 1;
    }
    // Skip whitespace
    while i < len && line[i].is_whitespace() {
        i += 1;
    }
    self.cursor.1 = i;
}

pub fn delete_back(&mut self) -> bool {
    let (row, col) = self.cursor;
    if col > 0 {
        self.lines[row].remove(col - 1);
        self.cursor.1 -= 1;
        true
    } else if row > 0 {
        let current_line = self.lines.remove(row);
        let prev_len = self.lines[row - 1].len();
        self.lines[row - 1].extend(current_line);
        self.cursor = (row - 1, prev_len);
        true
    } else {
        false
    }
}

pub fn delete_forward(&mut self) {
    let (row, col) = self.cursor;
    if col < self.lines[row].len() {
        self.lines[row].remove(col);
    } else if row < self.lines.len() - 1 {
        let next_line = self.lines.remove(row + 1);
        self.lines[row].extend(next_line);
    }
}

pub fn kill_to_start(&mut self) {
    let (row, col) = self.cursor;
    self.lines[row].drain(..col);
    self.cursor.1 = 0;
}

pub fn newline(&mut self) {
    let (row, col) = self.cursor;
    let rest = self.lines[row].split_off(col);
    self.lines.insert(row + 1, rest);
    self.cursor = (row + 1, 0);
}
```

**Step 3: Run tests**

Run: `cargo test -p omnish-client -- widgets::line_editor`
Expected: All 22 tests pass

**Step 4: Commit**

```
feat(widgets): add word movement, delete, newline to LineEditor
```

---

### Task 4: Move picker.rs into widgets/

**Files:**
- Move: `crates/omnish-client/src/picker.rs` → `crates/omnish-client/src/widgets/picker.rs`
- Modify: `crates/omnish-client/src/widgets/mod.rs`
- Modify: `crates/omnish-client/src/main.rs` (update `mod`/`use`)

**Step 1: Move file**

```bash
mv crates/omnish-client/src/picker.rs crates/omnish-client/src/widgets/picker.rs
```

**Step 2: Update widgets/mod.rs**

```rust
pub mod line_editor;
pub mod picker;
```

**Step 3: Update main.rs**

Remove `mod picker;` and update any `picker::` references to `widgets::picker::`.

**Step 4: Run tests**

Run: `cargo test -p omnish-client`
Expected: All existing picker tests pass

**Step 5: Commit**

```
refactor(widgets): move picker into widgets module
```

---

### Task 5: Enhance escape sequence parsing for new key combinations

**Files:**
- Modify: `crates/omnish-client/src/main.rs`

The current `parse_escape_sequence()` only handles `ESC [ X` (2-byte CSI). Need to support:

| Key | Sequence |
|-----|----------|
| Home | `ESC [ H` or `ESC [ 1 ~` |
| End | `ESC [ F` or `ESC [ 4 ~` |
| Delete | `ESC [ 3 ~` |
| Ctrl-Left | `ESC [ 1 ; 5 D` |
| Ctrl-Right | `ESC [ 1 ; 5 C` |
| Alt+Enter | `ESC \r` (ESC followed by CR) |

**Step 1: Refactor parse_escape_sequence**

Replace the current 2-byte parser with a more capable one that returns an enum:

```rust
#[derive(Debug, PartialEq)]
enum KeyEvent {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    Delete,
    CtrlLeft,
    CtrlRight,
    AltEnter,
    Esc,
}

/// Parse key events after receiving ESC byte.
/// Returns None for unrecognized sequences.
fn parse_key_after_esc(stdin_fd: i32) -> Option<KeyEvent> {
    let mut pfd = libc::pollfd { fd: stdin_fd, events: libc::POLLIN, revents: 0 };
    let ready = unsafe { libc::poll(&mut pfd, 1, 50) };
    if ready <= 0 {
        return Some(KeyEvent::Esc); // Bare ESC
    }

    let mut b = [0u8; 1];
    if nix::unistd::read(stdin_fd, &mut b) != Ok(1) {
        return Some(KeyEvent::Esc);
    }

    match b[0] {
        0x0d => return Some(KeyEvent::AltEnter), // ESC CR = Alt+Enter
        b'[' => {} // CSI sequence, continue parsing below
        _ => return None, // Unknown ESC + char
    }

    // Read CSI parameter bytes
    let mut params = Vec::new();
    loop {
        if nix::unistd::read(stdin_fd, &mut b) != Ok(1) {
            return None;
        }
        if b[0] >= 0x40 && b[0] <= 0x7E {
            // Final byte
            break;
        }
        params.push(b[0]);
    }
    let final_byte = b[0];

    match (params.as_slice(), final_byte) {
        ([], b'A') => Some(KeyEvent::ArrowUp),
        ([], b'B') => Some(KeyEvent::ArrowDown),
        ([], b'C') => Some(KeyEvent::ArrowRight),
        ([], b'D') => Some(KeyEvent::ArrowLeft),
        ([], b'H') => Some(KeyEvent::Home),
        ([], b'F') => Some(KeyEvent::End),
        ([b'3'], b'~') => Some(KeyEvent::Delete),
        ([b'1', b';', b'5'], b'C') => Some(KeyEvent::CtrlRight),
        ([b'1', b';', b'5'], b'D') => Some(KeyEvent::CtrlLeft),
        ([b'1'], b'~') => Some(KeyEvent::Home),    // alternate
        ([b'4'], b'~') => Some(KeyEvent::End),      // alternate
        _ => None,
    }
}
```

**Step 2: Run build**

Run: `cargo build -p omnish-client`
Expected: Compiles (the new function isn't called yet)

**Step 3: Commit**

```
feat(client): extend escape sequence parser for cursor movement keys
```

---

### Task 6: Integrate LineEditor into read_chat_input

**Files:**
- Modify: `crates/omnish-client/src/main.rs`

This is the main integration task. Replace the inline `Vec<u8>` buffer and byte-level editing with `LineEditor`.

**Step 1: Rewrite read_chat_input**

Key changes:
- Replace `buf: Vec<u8>` with `editor: LineEditor`
- Replace `parse_escape_sequence()` calls with `parse_key_after_esc()`
- Map `KeyEvent` variants to `editor` methods
- Add multi-line rendering: after each edit, redraw all lines from prompt position
- Enter submits `editor.content()`, Alt+Enter calls `editor.newline()`
- Ctrl-A → `editor.move_home()`, Ctrl-E → `editor.move_end()`, Ctrl-U → `editor.kill_to_start()`

The new rendering approach for multi-line:
- Track prompt row (save cursor position when prompt is drawn)
- After each edit: move to prompt row, clear from there, redraw all lines with `> ` prefix on first line and `  ` continuation on subsequent lines, position cursor

**Step 2: Run build and manual test**

Run: `cargo build -p omnish-client`
Expected: Compiles

Manual test: start omnish, enter chat mode (`::`), verify:
- Left/Right arrow moves cursor
- Typing inserts at cursor position
- Backspace deletes before cursor
- Home/End jump to line boundaries
- Ctrl-Left/Right move by word
- Alt+Enter creates new line
- Enter submits all lines
- Delete key works
- Ctrl-U kills to line start
- Ghost text still works (shown after last line)
- CJK characters render and edit correctly

**Step 3: Commit**

```
feat(client): integrate LineEditor into chat input for cursor editing

Closes #180
```

---

### Task 7: Remove dead code

**Files:**
- Modify: `crates/omnish-client/src/main.rs`

**Step 1: Remove old helpers**

Delete `last_utf8_char_len()` and `parse_escape_sequence()` (replaced by `parse_key_after_esc()`).

**Step 2: Run tests**

Run: `cargo test -p omnish-client`
Expected: All tests pass, no warnings about dead code

**Step 3: Commit**

```
refactor(client): remove obsolete byte-level input helpers
```
