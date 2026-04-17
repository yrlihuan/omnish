# Chat Layout Manager Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace scattered stdout writes in the chat loop with a region-based `ChatLayout` that manages vertical widget stacking.

**Architecture:** `ChatLayout` owns a fixed-order vector of `Region`s. Each region tracks its content (pre-rendered ANSI lines) and height. `update()` repositions the cursor, overwrites content, and handles height changes by redrawing regions below. Widgets produce `Vec<String>` lines; the layout handles all cursor movement.

**Tech Stack:** Rust, ANSI escape sequences, vt100 crate (testing)

**Spec:** `docs/superpowers/specs/2026-03-15-chat-layout-manager-design.md`

---

## Chunk 1: ChatLayout Core

### Task 1: ChatLayout struct + redraw_all

**Files:**
- Create: `crates/omnish-client/src/widgets/chat_layout.rs`
- Modify: `crates/omnish-client/src/widgets/mod.rs`

- [ ] **Step 1: Write failing tests for empty layout and redraw_all**

```rust
// In chat_layout.rs

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ansi(s: &str) -> vt100::Parser {
        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(s.as_bytes());
        p
    }

    #[test]
    fn test_empty_layout() {
        let layout = ChatLayout::new(80);
        assert_eq!(layout.total_height(), 0);
        assert_eq!(layout.redraw_all(), "");
    }

    #[test]
    fn test_push_regions() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("a");
        layout.push_region("b");
        // Regions exist but have no content
        assert_eq!(layout.total_height(), 0);
    }

    #[test]
    fn test_redraw_all_with_content() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("a");
        layout.push_region("b");
        layout.regions[0].content = vec!["line 1".into(), "line 2".into()];
        layout.regions[0].height = 2;
        layout.regions[1].content = vec!["line 3".into()];
        layout.regions[1].height = 1;

        let output = layout.redraw_all();
        let p = parse_ansi(&output);
        let screen = p.screen().contents();
        assert!(screen.contains("line 1"));
        assert!(screen.contains("line 2"));
        assert!(screen.contains("line 3"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p omnish-client chat_layout -- --nocapture`
Expected: FAIL - `ChatLayout` not found

- [ ] **Step 3: Implement ChatLayout struct + redraw_all**

```rust
// crates/omnish-client/src/widgets/chat_layout.rs

pub struct Region {
    pub(crate) id: &'static str,
    pub(crate) height: usize,
    pub(crate) content: Vec<String>,
}

pub struct ChatLayout {
    pub(crate) regions: Vec<Region>,
    cols: usize,
}

impl ChatLayout {
    pub fn new(cols: usize) -> Self {
        Self { regions: Vec::new(), cols }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn push_region(&mut self, id: &'static str) {
        self.regions.push(Region {
            id,
            height: 0,
            content: Vec::new(),
        });
    }

    pub fn total_height(&self) -> usize {
        self.regions.iter().map(|r| r.height).sum()
    }

    /// Redraw all regions top-to-bottom.
    /// Assumes cursor is at the layout origin.
    pub fn redraw_all(&self) -> String {
        let mut out = String::new();
        let mut first = true;
        for region in &self.regions {
            for line in &region.content {
                if first {
                    out.push_str(&format!("\r\x1b[K{}", line));
                    first = false;
                } else {
                    out.push_str(&format!("\r\n\x1b[K{}", line));
                }
            }
        }
        out
    }

    /// Update region content without producing ANSI output.
    /// Use before redraw_all() when rebuilding layout state.
    pub fn set_content(&mut self, id: &str, lines: Vec<String>) {
        let idx = self.find_region_idx(id);
        self.regions[idx].height = lines.len();
        self.regions[idx].content = lines;
    }
}
```

Add to `crates/omnish-client/src/widgets/mod.rs`:
```rust
pub mod chat_layout;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p omnish-client chat_layout -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/src/widgets/chat_layout.rs crates/omnish-client/src/widgets/mod.rs
git commit -m "feat: add ChatLayout struct with redraw_all"
```

---

### Task 2: ChatLayout update (same height)

**Files:**
- Modify: `crates/omnish-client/src/widgets/chat_layout.rs`

- [ ] **Step 1: Write failing tests for update and region_offset**

```rust
#[test]
fn test_region_offset() {
    let mut layout = ChatLayout::new(80);
    layout.push_region("a");
    layout.push_region("b");
    layout.push_region("c");
    // Set heights manually for this test
    layout.regions[0].height = 3;
    layout.regions[1].height = 2;
    layout.regions[2].height = 1;
    layout.regions[0].content = vec!["a1".into(), "a2".into(), "a3".into()];
    layout.regions[1].content = vec!["b1".into(), "b2".into()];
    layout.regions[2].content = vec!["c1".into()];

    assert_eq!(layout.region_offset("a"), 0);
    assert_eq!(layout.region_offset("b"), 3);
    assert_eq!(layout.region_offset("c"), 5);
}

#[test]
fn test_update_same_height() {
    let mut layout = ChatLayout::new(80);
    layout.push_region("top");
    layout.push_region("bottom");

    // Initial content
    layout.update("top", vec!["hello".into()]);
    layout.update("bottom", vec!["world".into()]);

    // Update top region with same height
    let seq = layout.update("top", vec!["HELLO".into()]);

    // Apply redraw_all to see final state
    let all = layout.redraw_all();
    let p = parse_ansi(&all);
    let screen = p.screen().contents();
    assert!(screen.contains("HELLO"));
    assert!(screen.contains("world"));
    assert!(!screen.contains("hello")); // old content replaced
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p omnish-client chat_layout -- --nocapture`
Expected: FAIL - `region_offset` and `update` not found

- [ ] **Step 3: Implement region_offset and update**

```rust
fn find_region_idx(&self, id: &str) -> usize {
    self.regions.iter().position(|r| r.id == id)
        .unwrap_or_else(|| panic!("region not found: {}", id))
}

pub fn region_offset(&self, id: &str) -> usize {
    let mut offset = 0;
    for r in &self.regions {
        if r.id == id {
            return offset;
        }
        offset += r.height;
    }
    panic!("region not found: {}", id);
}

/// Update region content. Returns ANSI sequence to write to terminal.
/// Cursor convention: cursor starts and ends at the row after the last
/// line of the layout (row = total_height).
pub fn update(&mut self, id: &str, lines: Vec<String>) -> String {
    let idx = self.find_region_idx(id);
    let old_height = self.regions[idx].height;
    let new_height = lines.len();
    let offset = self.region_offset(id);
    let old_total = self.total_height();

    self.regions[idx].content = lines;
    self.regions[idx].height = new_height;

    let mut out = String::new();

    if old_total == 0 && new_height == 0 {
        return out;
    }

    // Move cursor from bottom (row old_total) to region start (row offset)
    let up = old_total.saturating_sub(offset);
    if up > 0 {
        out.push_str(&format!("\x1b[{}A", up));
    }
    out.push('\r');

    if old_height == new_height {
        // Same height: overwrite region lines, move back to bottom
        for line in &self.regions[idx].content {
            out.push_str(&format!("\x1b[K{}\r\n", line));
        }
        let below: usize = self.regions[idx + 1..].iter().map(|r| r.height).sum();
        if below > 0 {
            out.push_str(&format!("\x1b[{}B", below));
        }
    } else {
        // Height changed: redraw this region + all below, clear leftover
        for i in idx..self.regions.len() {
            for line in &self.regions[i].content {
                out.push_str(&format!("\x1b[K{}\r\n", line));
            }
        }
        let new_total = self.total_height();
        if old_total > new_total {
            for _ in 0..(old_total - new_total) {
                out.push_str("\x1b[K\r\n");
            }
            // Move back up to new bottom
            let extra = old_total - new_total;
            if extra > 0 {
                out.push_str(&format!("\x1b[{}A", extra));
            }
        }
    }

    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p omnish-client chat_layout -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/src/widgets/chat_layout.rs
git commit -m "feat: add ChatLayout update and region_offset"
```

---

### Task 3: ChatLayout hide + cursor_to + height change tests

**Files:**
- Modify: `crates/omnish-client/src/widgets/chat_layout.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn test_update_height_increase() {
    let mut layout = ChatLayout::new(80);
    layout.push_region("top");
    layout.push_region("bottom");
    layout.update("top", vec!["t1".into()]);
    layout.update("bottom", vec!["b1".into()]);

    // Top grows from 1 to 3 lines
    layout.update("top", vec!["t1".into(), "t2".into(), "t3".into()]);

    let all = layout.redraw_all();
    let p = parse_ansi(&all);
    let screen = p.screen().contents();
    assert!(screen.contains("t1"));
    assert!(screen.contains("t2"));
    assert!(screen.contains("t3"));
    assert!(screen.contains("b1"));
    assert_eq!(layout.total_height(), 4);
    assert_eq!(layout.region_offset("bottom"), 3);
}

#[test]
fn test_update_height_decrease() {
    let mut layout = ChatLayout::new(80);
    layout.push_region("top");
    layout.push_region("bottom");
    layout.update("top", vec!["t1".into(), "t2".into(), "t3".into()]);
    layout.update("bottom", vec!["b1".into()]);

    // Top shrinks from 3 to 1 line
    layout.update("top", vec!["t1".into()]);

    assert_eq!(layout.total_height(), 2);
    assert_eq!(layout.region_offset("bottom"), 1);
}

#[test]
fn test_hide() {
    let mut layout = ChatLayout::new(80);
    layout.push_region("a");
    layout.push_region("b");
    layout.update("a", vec!["visible".into()]);
    layout.update("b", vec!["below".into()]);

    layout.hide("a");
    assert_eq!(layout.total_height(), 1);
    assert_eq!(layout.region_offset("b"), 0);

    let all = layout.redraw_all();
    let p = parse_ansi(&all);
    let screen = p.screen().contents();
    assert!(!screen.contains("visible"));
    assert!(screen.contains("below"));
}

#[test]
fn test_hide_then_update_reshows() {
    let mut layout = ChatLayout::new(80);
    layout.push_region("a");
    layout.push_region("b");
    layout.update("a", vec!["first".into()]);
    layout.update("b", vec!["below".into()]);
    layout.hide("a");

    // Re-show by updating with content
    layout.update("a", vec!["second".into()]);
    assert_eq!(layout.total_height(), 2);

    let all = layout.redraw_all();
    let p = parse_ansi(&all);
    let screen = p.screen().contents();
    assert!(screen.contains("second"));
    assert!(screen.contains("below"));
}

#[test]
fn test_cursor_to() {
    let mut layout = ChatLayout::new(80);
    layout.push_region("top");
    layout.push_region("editor");
    layout.push_region("status");
    layout.update("top", vec!["t1".into(), "t2".into()]);
    layout.update("editor", vec!["> input".into()]);
    layout.update("status", vec!["thinking...".into()]);

    // cursor_to("editor") should move cursor from bottom (row 4)
    // to last row of editor (row 2)
    let seq = layout.cursor_to("editor");
    assert!(seq.contains("\x1b[")); // contains cursor movement
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p omnish-client chat_layout -- --nocapture`
Expected: FAIL - `hide` and `cursor_to` not found

- [ ] **Step 3: Implement hide and cursor_to**

```rust
/// Hide a region (height becomes 0). Returns ANSI to update terminal.
pub fn hide(&mut self, id: &str) -> String {
    self.update(id, Vec::new())
}

/// Position cursor at the last row of a region.
/// Cursor moves from bottom of layout (row = total_height) to
/// the last row of the target region (row = offset + height - 1).
/// If region is empty (height 0), positions at offset row.
pub fn cursor_to(&self, id: &str) -> String {
    let idx = self.find_region_idx(id);
    let offset = self.region_offset(id);
    let height = self.regions[idx].height;
    let total = self.total_height();
    // Target the last row of the region (offset + height - 1),
    // not one past it. The cursor is currently at row total_height
    // (one past the last content line, due to trailing \r\n).
    let target = if height > 0 { offset + height - 1 } else { offset };
    let up = total.saturating_sub(target);
    if up > 0 {
        format!("\x1b[{}A\r", up)
    } else {
        "\r".to_string()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p omnish-client chat_layout -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/src/widgets/chat_layout.rs
git commit -m "feat: add ChatLayout hide and cursor_to"
```

---

### Task 4: ChatLayout full vt100 integration tests

Verify that applying `update()` sequences to a vt100 terminal produces the correct visible output, including height changes and hide/show cycles.

**Files:**
- Modify: `crates/omnish-client/src/widgets/chat_layout.rs`

- [ ] **Step 1: Write vt100 integration tests**

```rust
/// Apply a sequence of layout operations to a vt100 parser and verify
/// the final screen state matches expectations.
#[test]
fn test_vt100_update_sequence() {
    let mut layout = ChatLayout::new(80);
    layout.push_region("sv");
    layout.push_region("ed");
    layout.push_region("st");

    let mut p = vt100::Parser::new(24, 80, 0);

    // Initial: render scroll_view + editor
    let s1 = layout.update("sv", vec!["Response line 1".into(), "Response line 2".into()]);
    p.process(s1.as_bytes());
    let s2 = layout.update("ed", vec!["> ".into()]);
    p.process(s2.as_bytes());

    let screen = p.screen().contents();
    assert!(screen.contains("Response line 1"));
    assert!(screen.contains("Response line 2"));
    assert!(screen.contains("> "));

    // Show status
    let s3 = layout.update("st", vec!["(thinking...)".into()]);
    p.process(s3.as_bytes());
    let screen = p.screen().contents();
    assert!(screen.contains("(thinking...)"));

    // Hide status
    let s4 = layout.hide("st");
    p.process(s4.as_bytes());
    let screen = p.screen().contents();
    assert!(!screen.contains("(thinking...)"));
    // Editor should still be visible
    assert!(screen.contains("> "));
}

#[test]
fn test_vt100_scroll_view_grows() {
    let mut layout = ChatLayout::new(80);
    layout.push_region("sv");
    layout.push_region("ed");

    let mut p = vt100::Parser::new(24, 80, 0);

    let s1 = layout.update("sv", vec!["line 1".into()]);
    p.process(s1.as_bytes());
    let s2 = layout.update("ed", vec!["> hello".into()]);
    p.process(s2.as_bytes());

    // Scroll view grows
    let s3 = layout.update("sv", vec![
        "line 1".into(), "line 2".into(), "line 3".into(),
    ]);
    p.process(s3.as_bytes());

    let screen = p.screen().contents();
    assert!(screen.contains("line 1"));
    assert!(screen.contains("line 2"));
    assert!(screen.contains("line 3"));
    assert!(screen.contains("> hello"));
}
```

```rust
#[test]
fn test_vt100_update_last_region() {
    let mut layout = ChatLayout::new(80);
    layout.push_region("a");
    layout.push_region("b");

    let mut p = vt100::Parser::new(24, 80, 0);
    p.process(layout.update("a", vec!["first".into()]).as_bytes());
    p.process(layout.update("b", vec!["second".into()]).as_bytes());

    // Update the last region (no regions below)
    p.process(layout.update("b", vec!["UPDATED".into()]).as_bytes());

    let screen = p.screen().contents();
    assert!(screen.contains("first"));
    assert!(screen.contains("UPDATED"));
    assert!(!screen.contains("second"));
}

#[test]
fn test_update_with_empty_lines_hides() {
    let mut layout = ChatLayout::new(80);
    layout.push_region("a");
    layout.push_region("b");
    layout.update("a", vec!["visible".into()]);
    layout.update("b", vec!["below".into()]);

    // update with empty vec should behave like hide
    layout.update("a", vec![]);
    assert_eq!(layout.total_height(), 1);
    assert_eq!(layout.region_offset("b"), 0);
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p omnish-client chat_layout -- --nocapture`
Expected: PASS (if logic is correct) or FAIL (fix ANSI sequences)

- [ ] **Step 3: Fix any ANSI issues found by vt100 tests**

Iterate until all vt100 tests pass. The vt100 parser is the source of truth.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/widgets/chat_layout.rs
git commit -m "test: add vt100 integration tests for ChatLayout"
```

---

## Chunk 2: Widget Adaptations

### Task 5: TextView widget

**Files:**
- Create: `crates/omnish-client/src/widgets/text_view.rs`
- Modify: `crates/omnish-client/src/widgets/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_view_stores_lines() {
        let tv = TextView::new(vec!["hello".into(), "world".into()]);
        assert_eq!(tv.lines(), &["hello", "world"]);
    }

    #[test]
    fn test_text_view_empty() {
        let tv = TextView::new(vec![]);
        assert!(tv.lines().is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-client text_view -- --nocapture`
Expected: FAIL - `TextView` not found

- [ ] **Step 3: Implement TextView**

```rust
// crates/omnish-client/src/widgets/text_view.rs

/// Trivial widget that stores pre-styled lines for display.
pub struct TextView {
    content: Vec<String>,
}

impl TextView {
    pub fn new(lines: Vec<String>) -> Self {
        Self { content: lines }
    }

    pub fn lines(&self) -> &[String] {
        &self.content
    }
}
```

Add to `mod.rs`:
```rust
pub mod text_view;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-client text_view -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/src/widgets/text_view.rs crates/omnish-client/src/widgets/mod.rs
git commit -m "feat: add TextView widget"
```

---

### Task 6: ScrollView.compact_lines()

**Files:**
- Modify: `crates/omnish-client/src/widgets/scroll_view.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn test_compact_lines() {
    let mut sv = ScrollView::new(3, 10, 80);
    for i in 1..=10 {
        sv.push_line(&format!("line {}", i));
    }

    let lines = sv.compact_lines();
    // compact_height=3, so last 3 content lines + 1 hint line
    assert_eq!(lines.len(), 4);
    // Last 3 content lines (stripped of cursor movement)
    assert!(lines[0].contains("line 8"));
    assert!(lines[1].contains("line 9"));
    assert!(lines[2].contains("line 10"));
    // Hint line
    assert!(lines[3].contains("ctrl+o to view"));
}

#[test]
fn test_compact_lines_fewer_than_height() {
    let mut sv = ScrollView::new(5, 10, 80);
    sv.push_line("only line");

    let lines = sv.compact_lines();
    // Only 1 line, no scrolling needed, no hint
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("only line"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-client scroll_view::tests::test_compact_lines -- --nocapture`
Expected: FAIL - `compact_lines` not found

- [ ] **Step 3: Implement compact_lines**

```rust
/// Returns compact view lines for ChatLayout.
/// Content lines contain ANSI styling but no cursor movement.
/// Lines are truncated to max_cols. If content exceeds compact_height,
/// returns the tail + a hint line.
pub fn compact_lines(&self) -> Vec<String> {
    if self.lines.len() <= self.compact_height {
        return self.lines.iter()
            .map(|l| Self::truncate_line(l, self.max_cols))
            .collect();
    }
    let start = self.lines.len().saturating_sub(self.compact_height);
    let mut result: Vec<String> = self.lines[start..].iter()
        .map(|l| Self::truncate_line(l, self.max_cols))
        .collect();
    let hidden = self.lines.len().saturating_sub(self.compact_height);
    result.push(format!(
        "\x1b[2m\u{2026} +{} lines (ctrl+o to view)\x1b[0m",
        hidden
    ));
    result
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-client scroll_view::tests::test_compact_lines -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/src/widgets/scroll_view.rs
git commit -m "feat: add ScrollView.compact_lines() for ChatLayout"
```

---

### Task 7: LineEditor.render()

**Files:**
- Modify: `crates/omnish-client/src/widgets/line_editor.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn test_render_single_line() {
    let mut editor = LineEditor::new();
    editor.insert('h');
    editor.insert('i');

    let lines = editor.render("\x1b[36m> \x1b[0m", "");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("\x1b[36m> \x1b[0m"));
    assert!(lines[0].contains("hi"));
}

#[test]
fn test_render_with_ghost() {
    let mut editor = LineEditor::new();
    editor.insert('h');

    let lines = editor.render("> ", "ello");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("h"));
    assert!(lines[0].contains("\x1b[2;37mello\x1b[0m"));
}

#[test]
fn test_render_empty() {
    let editor = LineEditor::new();
    let lines = editor.render("> ", "");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], "> ");
}

#[test]
fn test_render_multiline() {
    let mut editor = LineEditor::new();
    editor.insert('a');
    editor.newline();
    editor.insert('b');

    let lines = editor.render("> ", "");
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("> "));
    assert!(lines[0].contains("a"));
    assert!(lines[1].starts_with("  ")); // continuation indent
    assert!(lines[1].contains("b"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-client line_editor::tests::test_render -- --nocapture`
Expected: FAIL - `render` not found

- [ ] **Step 3: Implement render**

```rust
/// Render editor content with prefix and optional ghost text.
/// Returns one styled line per editor row. No cursor movement sequences.
/// Ghost text appears dim after cursor on the last line.
pub fn render(&self, prefix: &str, ghost: &str) -> Vec<String> {
    let mut result = Vec::new();
    for (i, line_chars) in self.lines.iter().enumerate() {
        let mut s = String::new();
        if i == 0 {
            s.push_str(prefix);
        } else {
            // Continuation indent matching prefix display width.
            // Strip ANSI escape sequences before measuring width.
            let stripped = strip_ansi_escapes(prefix);
            let prefix_width = stripped.chars()
                .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
                .sum::<usize>();
            for _ in 0..prefix_width {
                s.push(' ');
            }
        }
        let text: String = line_chars.iter().collect();
        s.push_str(&text);

        // Ghost text on the last line after cursor
        if i == self.lines.len() - 1 && !ghost.is_empty() {
            s.push_str(&format!("\x1b[2;37m{}\x1b[0m", ghost));
        }
        result.push(s);
    }
    if result.is_empty() {
        result.push(prefix.to_string());
    }
    result
}
```

Note: `unicode_width` is already a dependency (used in `cursor_display_col`).

Add a private helper to strip ANSI escape sequences for width calculation:

```rust
/// Strip ANSI escape sequences from a string (for display width measurement).
fn strip_ansi_escapes(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip ESC [ ... (final byte is letter)
            if let Some(next) = chars.next() {
                if next == '[' {
                    for c2 in chars.by_ref() {
                        if c2.is_ascii_alphabetic() { break; }
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p omnish-client line_editor::tests::test_render -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/src/widgets/line_editor.rs
git commit -m "feat: add LineEditor.render() with prefix and ghost text"
```

---

### Task 8: LineStatus.lines() accessor

**Files:**
- Modify: `crates/omnish-client/src/widgets/line_status.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn test_lines_accessor() {
    let mut status = LineStatus::new(80, 5);
    assert!(status.lines_content().is_empty());

    status.show("thinking...");
    let lines = status.lines_content();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("thinking..."));

    status.append("tool call 1");
    let lines = status.lines_content();
    assert_eq!(lines.len(), 2);

    status.clear();
    assert!(status.lines_content().is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-client line_status::tests::test_lines_accessor -- --nocapture`
Expected: FAIL - `lines_content` not found

- [ ] **Step 3: Implement lines_content**

```rust
/// Returns current styled content lines for ChatLayout integration.
/// Each line has dim styling applied.
pub fn lines_content(&self) -> Vec<String> {
    if self.content.is_empty() {
        return Vec::new();
    }
    let visible = self.visible_lines();
    visible.iter().map(|l| {
        let truncated = Self::truncate_line(l, self.max_cols);
        format!("\x1b[2m{}\x1b[0m", truncated)
    }).collect()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-client line_status::tests::test_lines_accessor -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/src/widgets/line_status.rs
git commit -m "feat: add LineStatus.lines_content() accessor"
```

---

## Chunk 3: main.rs Integration

### Task 9: Wire ChatLayout into run_chat_loop - status + scroll_view

Replace direct stdout writes for LineStatus and ScrollView rendering with ChatLayout.

**Files:**
- Modify: `crates/omnish-client/src/main.rs`

- [ ] **Step 1: Add ChatLayout to run_chat_loop initialization**

At the top of `run_chat_loop()`, after the existing variable declarations, add:

```rust
use widgets::chat_layout::ChatLayout;

let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
let mut layout = ChatLayout::new(cols as usize);
layout.push_region("scroll_view");
layout.push_region("editor");
layout.push_region("status");
```

- [ ] **Step 2: Replace render_with_scroll_view with layout.update**

Find every call to `render_with_scroll_view(&rendered)` (lines ~2427, ~2611) and replace with:

```rust
// Before (old):
// last_scroll_view = render_with_scroll_view(&rendered);

// After (new):
let (rows, cols) = get_terminal_size().unwrap_or((24, 80));
let lines: Vec<&str> = rendered.split("\r\n").collect();
let compact_h = (rows as usize / 3).max(3);
if lines.len() <= rows as usize - 2 {
    // Fits on screen - use as-is
    let content_lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let seq = layout.update("scroll_view", content_lines);
    nix::unistd::write(std::io::stdout(), seq.as_bytes()).ok();
    last_scroll_view = None;
} else {
    let expanded_h = (rows as usize).saturating_sub(3);
    let mut sv = ScrollView::new(compact_h, expanded_h, cols as usize);
    for line in &lines {
        sv.push_line(line); // ignore return value - layout handles rendering
    }
    let sv_lines = sv.compact_lines();
    let seq = layout.update("scroll_view", sv_lines);
    nix::unistd::write(std::io::stdout(), seq.as_bytes()).ok();
    last_scroll_view = Some(sv);
}
```

**Important:** After the scroll_view update, the existing separator rendering (lines ~2613-2616: `display::render_separator`) should be included as the last line of the scroll_view region content, or rendered as a separate write between the layout update and the next prompt. Simplest approach: append the separator line to `content_lines` / `sv_lines` before calling `layout.update("scroll_view", ...)`.

- [ ] **Step 3: Replace LineStatus stdout writes with layout.update**

Find LineStatus show/append/clear calls and replace:

```rust
// Before (old):
// nix::unistd::write(std::io::stdout(), line_status.show("(thinking...)").as_bytes()).ok();

// After (new):
line_status.show("(thinking...)");
let seq = layout.update("status", line_status.lines_content());
nix::unistd::write(std::io::stdout(), seq.as_bytes()).ok();

// Similarly for append:
line_status.append(&text);
let seq = layout.update("status", line_status.lines_content());
nix::unistd::write(std::io::stdout(), seq.as_bytes()).ok();

// And clear:
line_status.clear();
let seq = layout.hide("status");
nix::unistd::write(std::io::stdout(), seq.as_bytes()).ok();
```

- [ ] **Step 4: Build and verify**

Run: `cargo build -p omnish-client`
Expected: compiles without errors

- [ ] **Step 5: Run existing tests**

Run: `cargo test -p omnish-client`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "refactor: wire ChatLayout into chat loop for status + scroll_view"
```

---

### Task 10: Wire editor region + refactor read_chat_input

Replace the chat prompt rendering and the redraw closure in `read_chat_input()` with ChatLayout-based rendering.

**Files:**
- Modify: `crates/omnish-client/src/main.rs`

- [ ] **Step 1: Change read_chat_input signature**

Replace `scroll_view: &mut Option<ScrollView>` with `layout: &mut ChatLayout` and add `scroll_view: &mut Option<ScrollView>` as a separate parameter:

```rust
fn read_chat_input(
    completer: &mut ghost_complete::GhostCompleter,
    allow_backspace_exit: bool,
    history: &VecDeque<String>,
    history_index: &mut Option<usize>,
    layout: &mut ChatLayout,
    scroll_view: &mut Option<ScrollView>,
) -> Option<String>
```

- [ ] **Step 2: Replace chat prompt rendering at call site**

In `run_chat_loop()`, remove the separate prompt rendering before `read_chat_input`:

```rust
// Before (old):
// let prompt = display::render_chat_prompt();
// nix::unistd::write(std::io::stdout(), prompt.as_bytes()).ok();
// match read_chat_input(..., &mut last_scroll_view) {

// After (new):
// Initial editor content with prompt
let editor_lines = vec!["\x1b[36m> \x1b[0m".to_string()];
let seq = layout.update("editor", editor_lines);
nix::unistd::write(std::io::stdout(), seq.as_bytes()).ok();
let seq = layout.cursor_to("editor");
nix::unistd::write(std::io::stdout(), seq.as_bytes()).ok();
match read_chat_input(..., &mut layout, &mut last_scroll_view) {
```

- [ ] **Step 3: Refactor redraw closure to use layout.update**

Inside `read_chat_input()`, modify the `redraw` closure to use the layout:

```rust
// The redraw closure now builds lines via editor.render() and
// updates through the layout
let redraw = |editor: &LineEditor, ghost: &str, has_ghost: bool| {
    let ghost_text = if has_ghost { ghost } else { "" };
    let lines = editor.render("\x1b[36m> \x1b[0m", ghost_text);
    let seq = layout.update("editor", lines);
    nix::unistd::write(std::io::stdout(), seq.as_bytes()).ok();
    // Position cursor at editor region
    let pos = layout.cursor_to("editor");
    nix::unistd::write(std::io::stdout(), pos.as_bytes()).ok();
    // Fine-tune cursor column position
    let col = editor.cursor_display_col() + 2; // +2 for "> " prefix
    if col > 0 {
        nix::unistd::write(
            std::io::stdout(),
            format!("\x1b[{}G", col + 1).as_bytes(), // 1-indexed
        ).ok();
    }
};
```

Note: The exact cursor column calculation may need adjustment for multi-line editors and ANSI prefix width. The `\x1b[36m> \x1b[0m` prefix has display width 2.

- [ ] **Step 4: Update Ctrl+O handler to use layout**

```rust
0x0f => { // Ctrl-O - browse scroll view
    if let Some(ref mut sv) = scroll_view {
        // Erase layout area
        let total = layout.total_height();
        for _ in 0..total {
            nix::unistd::write(std::io::stdout(), b"\x1b[1A\r\x1b[K").ok();
        }
        sv.run_browse();
        // Redraw all regions
        let sv_lines = sv.compact_lines();
        layout.set_content("scroll_view", sv_lines);
        let seq = layout.redraw_all();
        nix::unistd::write(std::io::stdout(), seq.as_bytes()).ok();
        // Position cursor at editor
        let pos = layout.cursor_to("editor");
        nix::unistd::write(std::io::stdout(), pos.as_bytes()).ok();
    }
}
```

This uses `set_content()` (defined in Task 1) which updates region content without producing ANSI - suitable for use before `redraw_all()`.

- [ ] **Step 5: Build and verify**

Run: `cargo build -p omnish-client`
Expected: compiles without errors

- [ ] **Step 6: Run all tests**

Run: `cargo test -p omnish-client`
Expected: all tests pass

- [ ] **Step 7: Manual test - run omnish and verify chat mode works**

Test scenarios:
1. Enter chat with `:`, type a message, verify response renders correctly
2. Type a long-response prompt, verify ScrollView compact + hint appears
3. Press Ctrl+O, browse, press q, verify layout restores
4. Verify "(thinking...)" status appears and disappears

- [ ] **Step 8: Run integration test**

Run: `tools/integration_tests/verify_scroll_view.sh`
Expected: all tests pass

- [ ] **Step 9: Commit**

```bash
git add crates/omnish-client/src/main.rs crates/omnish-client/src/widgets/chat_layout.rs
git commit -m "refactor: wire ChatLayout into read_chat_input with editor region"
```

---

### Task 11: Cleanup - remove render_with_scroll_view and dead code

**Files:**
- Modify: `crates/omnish-client/src/main.rs`
- Modify: `crates/omnish-client/src/display.rs`

- [ ] **Step 1: Remove render_with_scroll_view function**

Delete the `render_with_scroll_view()` function (lines ~2742-2763) - it's fully replaced by layout.update + ScrollView.compact_lines.

- [ ] **Step 2: Remove render_chat_prompt if unused**

Check if `display::render_chat_prompt()` has any remaining callers. If not, remove it from `display.rs`.

- [ ] **Step 3: Build and verify**

Run: `cargo build -p omnish-client && cargo test -p omnish-client`
Expected: compiles with no warnings, all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/main.rs crates/omnish-client/src/display.rs
git commit -m "refactor: remove render_with_scroll_view and dead display helpers"
```
