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
