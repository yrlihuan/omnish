# Line Editor Widget Design

## Problem

Chat mode input (`read_chat_input()` in main.rs) has all editing logic inline with cursor always at the end. No cursor movement, mid-line editing, or multi-line support. Issue #180.

## Design

### Module Structure

```
crates/omnish-client/src/widgets/
├── mod.rs
├── line_editor.rs   // multi-line editor
└── picker.rs        // moved from src/picker.rs
```

### LineEditor

```rust
pub struct LineEditor {
    lines: Vec<Vec<char>>,   // multi-line, char-level buffer
    cursor: (usize, usize),  // (row, col) in char indices
}
```

Pure state management — no I/O. Caller handles terminal rendering and key parsing.

### Operations

| Key | Method | Behavior |
|-----|--------|----------|
| Left | `move_left()` | Move cursor left; wrap to prev line end |
| Right | `move_right()` | Move cursor right; wrap to next line start |
| Up | `move_up()` | Move to previous line, clamp col |
| Down | `move_down()` | Move to next line, clamp col |
| Home / Ctrl-A | `move_home()` | Move to line start |
| End / Ctrl-E | `move_end()` | Move to line end |
| Ctrl-Left | `move_word_left()` | Move to previous word boundary |
| Ctrl-Right | `move_word_right()` | Move to next word boundary |
| Char | `insert(char)` | Insert at cursor position |
| Backspace | `delete_back()` | Delete char before cursor; merge lines at line start |
| Delete | `delete_forward()` | Delete char after cursor; merge next line at line end |
| Ctrl-U | `kill_to_start()` | Delete from cursor to line start |
| Alt+Enter | `newline()` | Insert new line at cursor |
| Enter | — | Handled by caller (submit) |
| ESC | — | Handled by caller (cancel) |

### Public API

```rust
impl LineEditor {
    pub fn new() -> Self;
    pub fn insert(&mut self, ch: char);
    pub fn newline(&mut self);
    pub fn delete_back(&mut self) -> bool;  // false if at (0,0)
    pub fn delete_forward(&mut self);
    pub fn move_left(&mut self);
    pub fn move_right(&mut self);
    pub fn move_up(&mut self);
    pub fn move_down(&mut self);
    pub fn move_home(&mut self);
    pub fn move_end(&mut self);
    pub fn move_word_left(&mut self);
    pub fn move_word_right(&mut self);
    pub fn kill_to_start(&mut self);
    pub fn set_content(&mut self, s: &str);  // replace all content
    pub fn content(&self) -> String;         // join lines with \n
    pub fn is_empty(&self) -> bool;
    pub fn cursor(&self) -> (usize, usize);  // (row, char_col)
    pub fn cursor_display_col(&self) -> usize; // visual column (unicode-width aware)
    pub fn line_count(&self) -> usize;
    pub fn line(&self, row: usize) -> &[char];
}
```

### Rendering

LineEditor provides state; caller renders. Key info for rendering:
- `line_count()` and `line(row)` for content
- `cursor()` and `cursor_display_col()` for cursor positioning
- Caller uses ANSI sequences for cursor placement and line clearing

### Scope Exclusions

- History navigation (deferred)
- Widget trait (extract when second widget warrants it)
- Ghost text logic (stays in caller)
- Terminal I/O (stays in caller)

### Integration

`read_chat_input()` refactored to:
1. Create `LineEditor::new()`
2. Parse raw key events from stdin
3. Map keys to LineEditor methods
4. Render after each state change
5. On Enter: return `editor.content()`
