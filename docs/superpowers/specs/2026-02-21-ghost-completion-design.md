# Ghost Completion UI Design

**Date:** 2026-02-21

## Goal

Add inline ghost text completion to omnish's `:` chat mode. When the user types after the `:` prefix, a gray suggestion appears after the cursor. Pressing Tab accepts the entire suggestion. The UI component should be generic enough to reuse in shell input mode later.

## Scope

- **Phase 1 (this design):** Chat mode only (`:` prefix active)
- **Future:** Extend to shell input mode with different providers

## Architecture

### Component: GhostCompleter

An independent struct that manages completion state, decoupled from both `InputInterceptor` and `display.rs`.

```
┌─────────────┐     query      ┌─────────────────┐
│ main.rs     │ ──────────────>│ GhostCompleter   │
│ (on Buffer) │                │                  │
│             │ <──────────────│ ghost: Option<T>  │
│             │   render cmd   │ providers: Vec<> │
│ display.rs  │ <──────────────│                  │
│ render_ghost│                └─────────────────┘
└─────────────┘                        │
                                       │ CompletionProvider trait
                               ┌───────┴────────┐
                               │                 │
                        ┌──────┴───┐    ┌────────┴────┐
                        │BuiltinCP │    │  (future)   │
                        │/debug    │    │  LlmCP etc  │
                        │/context  │    │             │
                        └──────────┘    └─────────────┘
```

### Trait: CompletionProvider

```rust
pub trait CompletionProvider {
    /// Given current input text (after the prefix), return a full-line suggestion.
    /// The ghost text displayed = suggestion[input.len()..] (the suffix).
    fn suggest(&self, input: &str) -> Option<String>;
}
```

Providers are queried in order; first match wins.

### GhostCompleter API

```rust
pub struct GhostCompleter {
    providers: Vec<Box<dyn CompletionProvider>>,
    current_ghost: Option<String>,   // full suggestion text
    current_input_len: usize,        // len of input that triggered this ghost
}

impl GhostCompleter {
    pub fn new(providers: Vec<Box<dyn CompletionProvider>>) -> Self;

    /// Update with new input text. Returns the ghost suffix to display, or None.
    pub fn update(&mut self, input: &str) -> Option<&str>;

    /// Accept the current ghost. Returns the suffix text to insert into buffer.
    pub fn accept(&mut self) -> Option<String>;

    /// Clear any active ghost (e.g., on cancel/dismiss).
    pub fn clear(&mut self);
}
```

### BuiltinProvider

Matches omnish built-in commands by prefix:

- `/debug` → `/debug session`, `/debug stream`, etc.
- `/context` → `/context`
- `/session` → `/session`
- Other registered commands from `command.rs`

### Rendering

Ghost text is rendered as dim gray text after the cursor, with cursor restored to original position:

```
\x1b7              (save cursor)
\x1b[90m{ghost}\x1b[0m  (dim gray ghost text)
\x1b8              (restore cursor)
```

Added as `render_ghost_text(ghost: &str) -> String` in `display.rs`.

When input changes, the existing `render_input_echo` already clears to end of line (`\x1b[K`), which naturally removes stale ghost text before new ghost is drawn.

### Tab Handling in InputInterceptor

Tab (0x09) in chat/buffering mode:
1. Check if `GhostCompleter` has an active suggestion
2. If yes: append ghost suffix to interceptor buffer, return `Buffering(updated_buf)` — main loop re-renders echo + queries new ghost
3. If no: ignore Tab (don't forward to PTY)

Tab outside chat mode: forward to PTY as normal (preserves shell Tab completion).

### Data Flow

```
User types 'h' in chat mode
  → InterceptAction::Buffering(":h")
  → main.rs extracts input "h"
  → completer.update("h") → Some("help")  // ghost suffix = "elp"
  → render_input_echo(b"h") + render_ghost_text("elp")
  → stdout: "❯ h\x1b7\x1b[90melp\x1b[0m\x1b8"

User presses Tab
  → interceptor detects Tab in chat mode
  → completer.accept() → Some("elp")
  → append "elp" to buffer → buffer = ":help"
  → InterceptAction::Buffering(":help")
  → re-render with new input, completer.update("help") → None (exact match, no ghost)
```

### File Plan

- `crates/omnish-client/src/ghost_complete.rs` — GhostCompleter + CompletionProvider trait + BuiltinProvider
- `crates/omnish-client/src/display.rs` — add `render_ghost_text()`
- `crates/omnish-client/src/interceptor.rs` — Tab handling in chat mode
- `crates/omnish-client/src/main.rs` — wire completer into Buffering action handler
