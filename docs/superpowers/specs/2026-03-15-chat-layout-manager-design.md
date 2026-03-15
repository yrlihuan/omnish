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
    regions: Vec<Region>,   // ordered top-to-bottom, fixed once built
    total_height: usize,    // sum of all region heights
    cols: usize,            // terminal width
}

Region {
    id: &'static str,       // e.g. "scroll_view", "editor", "status"
    height: usize,          // current line count (0 = hidden)
    content: Vec<String>,   // one pre-rendered ANSI-styled string per line
}
```

**Region stack is fixed once built.** Regions are pushed during chat loop initialization and never reordered, inserted, or removed. Visibility is controlled by setting height to 0 (via `hide()`) or non-zero (via `update()` with content).

**Content is pre-rendered ANSI.** Each string in `Region.content` contains final styled output (colors, bold, dim etc.) but no cursor movement sequences. Widgets are responsible for producing these styled lines. The layout manager handles all cursor positioning.

**Widgets pre-wrap lines to fit `cols`.** Each line in content must fit within the terminal width. Widgets handle wrapping/truncation before passing lines to the layout. `Region.height` equals `content.len()` (logical lines = physical terminal rows).

Typical chat mode region stack:
```
regions[0]: "scroll_view"   — 8 lines (compact tail + hint)
regions[1]: "editor"        — 1 line  ("> " + input)
regions[2]: "status"        — 0 lines (hidden until thinking)
```

### Cursor management

The cursor is tracked relative to the bottom of the layout. To position at a region, the manager moves up by `(total_height - region_start_row - target_line)` lines. After updating a non-editor region, the cursor returns to the bottom of the layout.

**Editor region exception:** After `update("editor", ...)`, the cursor stays at the end of the editor content (not the layout bottom), because the user is actively typing there. The caller is responsible for fine-grained cursor column positioning within the editor line (e.g., when the cursor is mid-line). The layout manager only positions to the correct row.

### Interactive takeover

Interactive widgets (ScrollView browse mode, Picker) temporarily take over the terminal. The caller erases the layout area before entering interactive mode. On exit, `ChatLayout::redraw_all()` restores all regions. `redraw_all()` assumes the cursor is at the top-left of the layout area (caller positions it there before calling) and renders all regions top-to-bottom without clearing the screen.

## API

```rust
impl ChatLayout {
    fn new(cols: usize) -> Self;

    /// Add a region at the bottom of the stack.
    fn push_region(&mut self, id: &'static str);

    /// Update region content. Handles height changes by shifting regions below.
    /// If the region was hidden (height 0) and lines is non-empty, it becomes visible.
    /// If lines is empty, equivalent to hide().
    /// Returns ANSI sequence to write to terminal.
    /// Panics if id is not found (region IDs are static literals, a missing ID is a bug).
    fn update(&mut self, id: &str, lines: Vec<String>) -> String;

    /// Hide a region (height becomes 0, content cleared). Regions below shift up.
    /// No-op if already hidden. Panics if id is not found.
    fn hide(&mut self, id: &str) -> String;

    /// Get the row offset of a region (sum of heights above it).
    /// Panics if id is not found.
    fn region_offset(&self, id: &str) -> usize;

    /// Redraw all regions top-to-bottom. Assumes cursor is at the layout origin.
    /// Does not clear the screen — caller erases if needed.
    fn redraw_all(&self) -> String;

    /// Position cursor at the last row of a specific region.
    /// Panics if id is not found.
    fn cursor_to(&self, id: &str) -> String;
}
```

## Widget Adaptations

Existing widgets gain methods to produce line-based output for ChatLayout:

| Widget | New method | Purpose |
|--------|-----------|---------|
| ScrollView | `compact_lines() -> Vec<String>` | Returns compact view lines (tail of content) plus the hint line ("... +N lines (ctrl+o to view)"). Lines contain ANSI styling but no cursor movement. |
| LineEditor | `render(prefix, ghost) -> Vec<String>` | Merges `> ` prefix into editor output. Returns styled lines with prefix, user text, and dim ghost text. |
| LineStatus | `lines() -> Vec<String>` | Accessor for current styled status lines. |
| Picker | No change | Uses interactive takeover path (erase layout, run picker, redraw_all). |
| TextView (new) | Trivial data holder | `struct TextView { lines: Vec<String> }` — stores pre-styled lines, provides `lines() -> &[String]`. No truncation or wrapping logic; callers pre-format content. |

### Chat prompt merger

The `> ` chat prompt (currently `display::render_chat_prompt()`) merges into LineEditor as a prefix parameter, eliminating it as a separate rendering concern.

### ScrollView and read_chat_input

`read_chat_input()` receives `&mut ChatLayout` instead of `&mut Option<ScrollView>`. The ScrollView instance is held separately by the chat loop (as today). For Ctrl+O browse mode, `read_chat_input` accesses the ScrollView via a parameter (not through the layout). The layout is only used for erase-before-browse and redraw-after-browse.

## main.rs Refactoring

`run_chat_loop()` changes:

- All rendering goes through `layout.update(region_id, lines)` instead of direct `nix::unistd::write`
- `read_chat_input()` receives `&mut ChatLayout` instead of `&mut Option<ScrollView>`
- Browse/picker entry: erase layout area, run interactive loop, then `layout.redraw_all()`
- `render_with_scroll_view()` replaced by `layout.update("scroll_view", sv.compact_lines())`

## Out of Scope

- InlineNotice (uses stderr, outside chat mode layout)
- Shell mode rendering (prompt, completion ghost text)
- Automatic resize relayout (future enhancement; `cols` is set at construction and not updated)

## Testing

### ChatLayout unit tests (vt100 emulator)

- Basic rendering: 3 regions with content, verify `redraw_all()` output
- Height change: region grows from 2 to 4 lines, verify regions below shift down
- Height shrink: region shrinks, verify regions below shift up
- Hide: hidden region has 0 height, regions below shift up; update re-shows it
- Empty region: height-0 region occupies no space in redraw_all
- cursor_to: verify ANSI positioning sequence for editor region
- Sequential updates: update same region multiple times, verify only latest content rendered

### Widget adaptation tests

- LineEditor.render(): prefix + content + ghost text combination
- ScrollView.compact_lines(): returns compact_height lines from tail plus hint line
- TextView: stores and returns lines

### Integration

Existing `verify_scroll_view.sh` must continue to pass after refactoring.
