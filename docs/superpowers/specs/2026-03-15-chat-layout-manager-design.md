# Chat Layout Manager — Region-based Widget Layout

## Goal

Introduce a `ChatLayout` manager that coordinates all widget rendering in chat mode, replacing the scattered `nix::unistd::write` calls in main.rs with a unified region-based layout system that supports multiple vertically-stacked widgets.

## Motivation

Currently, main.rs has 15+ direct stdout write calls for rendering chat UI elements (prompt, response, status, hint, separator). Each widget manages its own cursor positioning with relative ANSI movement. This works for the current single-ScrollView layout but breaks when:

- Multiple independently-updating widgets need to coexist on screen
- A widget's height changes and widgets below it need to shift
- Interactive modes (browse, picker) exit and the full layout must be restored

## Architecture

`ChatLayout` is a vertical region stack manager. Each region is a named rectangular area that tracks its line count and content.

```
ChatLayout {
    regions: Vec<Region>,   // ordered top-to-bottom
    total_height: usize,    // sum of all region heights
    cols: usize,            // terminal width
}

Region {
    id: &'static str,       // e.g. "scroll_view", "editor", "status"
    height: usize,          // current line count (0 = hidden)
    content: Vec<String>,   // one ANSI string per line
}
```

Typical chat mode region stack:
```
regions[0]: "scroll_view"   — 8 lines (compact tail + hint)
regions[1]: "editor"        — 1 line  ("> " + input)
regions[2]: "status"        — 0 lines (hidden until thinking)
```

### Cursor management

The cursor is tracked relative to the bottom of the layout. To position at a region, the manager moves up by `(total_height - region_start_row)` lines. After updating a region, the cursor returns to the bottom.

### Interactive takeover

Interactive widgets (ScrollView browse mode, Picker) temporarily take over the terminal. On exit, `ChatLayout::redraw_all()` restores all regions.

## API

```rust
impl ChatLayout {
    fn new(cols: usize) -> Self;

    /// Add a region at the bottom of the stack.
    fn push_region(&mut self, id: &'static str) -> &mut Region;

    /// Update region content. Handles height changes by shifting regions below.
    /// Returns ANSI sequence to write to terminal.
    fn update(&mut self, id: &str, lines: Vec<String>) -> String;

    /// Hide a region (height becomes 0). Regions below shift up.
    fn hide(&mut self, id: &str) -> String;

    /// Get the row offset of a region (sum of heights above it).
    fn region_offset(&self, id: &str) -> usize;

    /// Redraw all regions from scratch (after interactive takeover or resize).
    fn redraw_all(&self) -> String;

    /// Position cursor at the end of a specific region.
    fn cursor_to(&self, id: &str) -> String;
}
```

## Widget Adaptations

Existing widgets gain methods to produce line-based output for ChatLayout:

| Widget | New method | Purpose |
|--------|-----------|---------|
| ScrollView | `compact_lines() -> Vec<String>` | Returns compact view lines including hint |
| LineEditor | `render(prefix, ghost) -> Vec<String>` | Merges `> ` prefix into editor output |
| LineStatus | `lines() -> Vec<String>` | Accessor for current status lines |
| Picker | No change | Uses interactive takeover path |
| TextView (new) | Stores `Vec<String>` | Simple text display (short responses, errors, separators) |

### Chat prompt merger

The `> ` chat prompt (currently `display::render_chat_prompt()`) merges into LineEditor as a prefix parameter, eliminating it as a separate rendering concern.

## main.rs Refactoring

`run_chat_loop()` changes:

- All rendering goes through `layout.update(region_id, lines)` instead of direct `nix::unistd::write`
- `read_chat_input()` receives `&mut ChatLayout` instead of `&mut Option<ScrollView>`
- Browse/picker entry: save state, run interactive loop, then `layout.redraw_all()`
- `render_with_scroll_view()` replaced by `layout.update("scroll_view", sv.compact_lines())`

## Out of Scope

- InlineNotice (uses stderr, outside chat mode layout)
- Shell mode rendering (prompt, completion ghost text)
- Automatic resize relayout (future enhancement)

## Testing

### ChatLayout unit tests (vt100 emulator)

- Basic rendering: 3 regions with content, verify `redraw_all()` output
- Height change: region grows from 2 to 4 lines, verify regions below shift down
- Hide/show: hidden region has 0 height, regions below shift up
- Empty region: height-0 region occupies no space
- cursor_to: verify ANSI positioning sequence

### Widget adaptation tests

- LineEditor.render(): prefix + content + ghost text combination
- ScrollView.compact_lines(): returns compact_height lines from tail
- TextView: stores and returns lines

### Integration

Existing `verify_scroll_view.sh` must continue to pass after refactoring.
