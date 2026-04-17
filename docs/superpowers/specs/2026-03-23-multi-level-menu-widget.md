# Multi-Level Menu Widget

A terminal widget for navigating hierarchical option menus with mixed item types. Replaces nested picker calls with a single stateful navigation experience.

## Interaction Model

**Navigation:**
- `↑↓` - move cursor
- `Enter` - enter submenu / confirm selection / submit text edit / toggle bool
- `ESC` - back to parent level (exit at top level)
- `Ctrl-C` - quit entire widget immediately, discard changes

**Hint line:** `↑↓ move  Enter select  ESC back  ^C quit`

## Rendering

Reuses the existing picker rendering pattern:

```
Config > LLM > Backends
────────────────────────────────
> claude                    anthropic/claude-sonnet-4-20250514
  openai                   openai/gpt-4o
  local                    ollama/llama3
────────────────────────────────
↑↓ move  Enter select  ESC back  ^C quit
```

**Layout:** breadcrumb title + separator + item list (viewport scrolling at MAX_VISIBLE) + separator + hint

**Breadcrumb:** title line shows the navigation path, e.g. `Config > LLM > Backends`. Top level shows just the root title.

## Menu Item Types

Four item types, each with distinct rendering and Enter behavior:

### 1. Submenu

Points to a child menu. Enter replaces the current view with the child.

```
  LLM                                  >
```

Right-aligned `>` indicator. No value display.

### 2. Select

A fixed set of choices. Enter opens a nested picker (flat, single-select) with the current value pre-selected. After selection, returns to the menu with the updated value displayed.

```
  Default backend              claude
```

Right-aligned current value (dimmed).

### 3. Toggle

Boolean on/off. Enter toggles the value immediately in-place (no sub-view).

```
  Completion enabled              ON
```

Right-aligned `ON`/`OFF`. `ON` in green, `OFF` in dim.

### 4. TextInput

Free-form string/number. Enter activates inline editing on the same line - current value becomes editable with a cursor. Enter confirms, ESC cancels edit (restores previous value).

```
  Proxy URL         http://proxy:8080
```

Right-aligned current value (dimmed). During editing:

```
  Proxy URL         http://proxy:8080█
```

Value shown in normal color with a blinking cursor. Basic editing: type characters, backspace, left/right arrows to move cursor within the text.

## Data Model

```rust
/// A single menu item.
pub enum MenuItem {
    Submenu {
        label: String,
        children: Vec<MenuItem>,
    },
    Select {
        label: String,
        options: Vec<String>,
        selected: usize,
    },
    Toggle {
        label: String,
        value: bool,
    },
    TextInput {
        label: String,
        value: String,
    },
}

/// Result returned when the widget exits.
pub enum MenuResult {
    /// User exited normally (ESC at top level). Contains all modified values.
    Done(Vec<MenuChange>),
    /// User pressed Ctrl-C. Discard all changes.
    Cancelled,
}

/// A single value change made during the menu session.
pub struct MenuChange {
    /// Dot-separated path, e.g. "llm.default" or "shell.developer_mode"
    pub path: String,
    /// New value as a string representation.
    pub value: String,
}
```

The caller builds the menu tree, passes it to the widget, and receives back a list of changes. The widget does not know about config files - it is a pure UI component.

## State Machine

```
TopLevel
  │
  ├─ Enter on Submenu ──► push level, render children
  ├─ Enter on Select  ──► show flat picker for options
  ├─ Enter on Toggle  ──► flip value, redraw item
  ├─ Enter on TextInput ─► enter edit mode
  │
  ├─ ESC ──► exit widget, return Done(changes)
  └─ Ctrl-C ──► exit widget, return Cancelled

SubLevel
  │
  ├─ (same Enter behaviors as TopLevel)
  ├─ ESC ──► pop level, render parent
  └─ Ctrl-C ──► exit widget, return Cancelled

EditMode (TextInput)
  │
  ├─ Enter ──► confirm edit, return to menu
  ├─ ESC ──► cancel edit, restore old value, return to menu
  └─ Ctrl-C ──► exit widget, return Cancelled
```

## Implementation

**File:** `crates/omnish-client/src/widgets/menu.rs`

**Public API:**

```rust
/// Run the multi-level menu widget. Returns changes or Cancelled.
pub fn run_menu(title: &str, items: &mut [MenuItem]) -> MenuResult
```

**Internal structure:**
- Navigation stack: `Vec<(usize, usize)>` - each entry is `(item_index_in_parent, cursor_position)` so returning to a parent restores cursor position
- Rendering: reuse `render_separator()` and viewport scrolling logic from `picker.rs`
- Text editing: minimal line editor (insert char, backspace, left/right arrows, home/end)
- Changes tracking: `Vec<MenuChange>` accumulated during the session

**Viewport scrolling:** same as picker - MAX_VISIBLE items, scroll offset adjusts when cursor moves beyond viewport bounds.

## File Changes

| File | Action |
|------|--------|
| `crates/omnish-client/src/widgets/menu.rs` | Create |
| `crates/omnish-client/src/widgets/mod.rs` | Add `pub mod menu;` |
