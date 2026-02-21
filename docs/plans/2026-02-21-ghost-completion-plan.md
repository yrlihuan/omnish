# Ghost Completion UI Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add inline ghost text completion to omnish's `:` chat mode — gray suggestions appear after cursor, Tab accepts.

**Architecture:** A standalone `GhostCompleter` struct holds `CompletionProvider` trait objects. On each `Buffering` action in main.rs, the completer is queried and ghost text is rendered via save/restore cursor + dim gray ANSI. Tab in chat mode triggers accept, appending ghost suffix to the buffer.

**Tech Stack:** Rust, ANSI escape sequences, existing omnish-client crate

---

### Task 1: CompletionProvider trait + BuiltinProvider + GhostCompleter

**Files:**
- Create: `crates/omnish-client/src/ghost_complete.rs`
- Modify: `crates/omnish-client/src/main.rs:2` (add `mod ghost_complete;`)

**Step 1: Write the test file with tests for BuiltinProvider and GhostCompleter**

```rust
// At the bottom of ghost_complete.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_provider_exact_match_no_ghost() {
        let p = BuiltinProvider::new();
        // Exact match should return None (nothing left to suggest)
        assert_eq!(p.suggest("/debug context"), None);
    }

    #[test]
    fn test_builtin_provider_prefix_match() {
        let p = BuiltinProvider::new();
        assert_eq!(p.suggest("/deb"), Some("/debug".to_string()));
    }

    #[test]
    fn test_builtin_provider_subcommand_match() {
        let p = BuiltinProvider::new();
        assert_eq!(p.suggest("/debug con"), Some("/debug context".to_string()));
    }

    #[test]
    fn test_builtin_provider_no_match() {
        let p = BuiltinProvider::new();
        assert_eq!(p.suggest("/xyz"), None);
    }

    #[test]
    fn test_builtin_provider_empty_input() {
        let p = BuiltinProvider::new();
        // Empty input could match first command, but we don't suggest on empty
        assert_eq!(p.suggest(""), None);
    }

    #[test]
    fn test_completer_update_returns_ghost_suffix() {
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(BuiltinProvider::new()),
        ];
        let mut c = GhostCompleter::new(providers);
        // "/deb" matches "/debug" → ghost suffix is "ug"
        assert_eq!(c.update("/deb"), Some("ug"));
    }

    #[test]
    fn test_completer_update_no_match_returns_none() {
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(BuiltinProvider::new()),
        ];
        let mut c = GhostCompleter::new(providers);
        assert_eq!(c.update("hello world"), None);
    }

    #[test]
    fn test_completer_accept_returns_suffix() {
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(BuiltinProvider::new()),
        ];
        let mut c = GhostCompleter::new(providers);
        c.update("/deb");
        assert_eq!(c.accept(), Some("ug".to_string()));
        // After accept, ghost is cleared
        assert_eq!(c.accept(), None);
    }

    #[test]
    fn test_completer_clear() {
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(BuiltinProvider::new()),
        ];
        let mut c = GhostCompleter::new(providers);
        c.update("/deb");
        c.clear();
        assert_eq!(c.accept(), None);
    }

    #[test]
    fn test_completer_first_provider_wins() {
        // Custom provider that always suggests "hello"
        struct AlwaysHello;
        impl CompletionProvider for AlwaysHello {
            fn suggest(&self, input: &str) -> Option<String> {
                if !input.is_empty() {
                    Some(format!("{}hello", input))
                } else {
                    None
                }
            }
        }
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(AlwaysHello),
            Box::new(BuiltinProvider::new()),
        ];
        let mut c = GhostCompleter::new(providers);
        // AlwaysHello wins over BuiltinProvider
        assert_eq!(c.update("/deb"), Some("hello"));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-client ghost_complete 2>&1 | tail -5`
Expected: Compilation error — `ghost_complete` module not found.

**Step 3: Write the implementation**

Add `mod ghost_complete;` to `main.rs` (after `mod command;` line).

Create `ghost_complete.rs`:

```rust
/// Trait for completion data sources. Providers are queried in order; first match wins.
pub trait CompletionProvider {
    /// Given current input text (after the `:` prefix), return a full-line suggestion.
    /// Returns None if no completion available.
    /// The suggestion MUST start with `input` as a prefix.
    fn suggest(&self, input: &str) -> Option<String>;
}

/// Completes omnish built-in `/` commands.
pub struct BuiltinProvider {
    commands: Vec<String>,
}

impl BuiltinProvider {
    pub fn new() -> Self {
        Self {
            commands: vec![
                "/debug".to_string(),
                "/debug context".to_string(),
                "/debug template".to_string(),
            ],
        }
    }
}

impl CompletionProvider for BuiltinProvider {
    fn suggest(&self, input: &str) -> Option<String> {
        if input.is_empty() {
            return None;
        }
        // Find first command that starts with input and is longer than input
        self.commands
            .iter()
            .find(|cmd| cmd.starts_with(input) && cmd.len() > input.len())
            .cloned()
    }
}

/// Manages ghost text completion state.
pub struct GhostCompleter {
    providers: Vec<Box<dyn CompletionProvider>>,
    /// The full suggestion text (including the input prefix)
    current_suggestion: Option<String>,
    /// Length of the input that produced current suggestion
    current_input_len: usize,
}

impl GhostCompleter {
    pub fn new(providers: Vec<Box<dyn CompletionProvider>>) -> Self {
        Self {
            providers,
            current_suggestion: None,
            current_input_len: 0,
        }
    }

    /// Update with new input. Returns the ghost suffix to display, or None.
    pub fn update(&mut self, input: &str) -> Option<&str> {
        self.current_suggestion = None;
        self.current_input_len = input.len();

        for provider in &self.providers {
            if let Some(suggestion) = provider.suggest(input) {
                if suggestion.len() > input.len() {
                    self.current_suggestion = Some(suggestion);
                    break;
                }
            }
        }

        self.ghost_suffix()
    }

    /// Get the current ghost suffix (the part after what user typed).
    fn ghost_suffix(&self) -> Option<&str> {
        self.current_suggestion
            .as_deref()
            .map(|s| &s[self.current_input_len..])
            .filter(|s| !s.is_empty())
    }

    /// Accept the current ghost. Returns the suffix to append to the buffer.
    pub fn accept(&mut self) -> Option<String> {
        let suffix = self.ghost_suffix().map(|s| s.to_string());
        self.current_suggestion = None;
        suffix
    }

    /// Clear any active ghost text.
    pub fn clear(&mut self) {
        self.current_suggestion = None;
    }
}
```

**Step 4: Run tests**

Run: `cargo test -p omnish-client ghost_complete -- --nocapture 2>&1 | tail -20`
Expected: All 9 tests pass.

**Step 5: Commit**

```bash
git add crates/omnish-client/src/ghost_complete.rs crates/omnish-client/src/main.rs
git commit -m "feat(client): add GhostCompleter and BuiltinProvider"
```

---

### Task 2: Ghost text rendering in display.rs

**Files:**
- Modify: `crates/omnish-client/src/display.rs`

**Step 1: Write the test**

Add to `display.rs` tests:

```rust
#[test]
fn test_render_ghost_text() {
    let output = render_ghost_text("ug context");
    let parser = parse_ansi(&output, 40, 24);
    let screen = parser.screen();
    let row = get_row(screen, 0, 40);
    assert!(row.contains("ug context"), "ghost text should be visible");
    // Cursor should be at column 0 (restored to start by \x1b8)
    let cursor = screen.cursor_position();
    assert_eq!(cursor.1, 0, "cursor should be restored to saved position");
}

#[test]
fn test_render_ghost_text_empty() {
    let output = render_ghost_text("");
    assert!(output.is_empty(), "empty ghost should produce no output");
}

#[test]
fn test_input_echo_with_ghost() {
    let cols: u16 = 40;
    let mut output = String::new();
    output.push_str(&render_input_echo(b"/deb"));
    output.push_str(&render_ghost_text("ug"));

    let parser = parse_ansi(&output, cols, 24);
    let screen = parser.screen();
    let row = get_row(screen, 0, cols);
    // Should see both real text and ghost
    assert!(row.contains("/deb"), "input text should be visible");
    assert!(row.contains("ug"), "ghost text should be visible");
    // Cursor should be right after "/deb" (col = 2 for "❯ " + 4 for "/deb" = 6)
    let cursor = screen.cursor_position();
    assert_eq!(cursor.1, 6, "cursor should be after real input, not ghost");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-client display::tests::test_render_ghost 2>&1 | tail -5`
Expected: `render_ghost_text` not found.

**Step 3: Write the implementation**

Add to `display.rs`:

```rust
/// Render ghost text (completion suggestion) in dim gray after the cursor.
/// Uses save/restore cursor so the cursor stays at the real input position.
/// Returns empty string if ghost is empty.
pub fn render_ghost_text(ghost: &str) -> String {
    if ghost.is_empty() {
        return String::new();
    }
    format!("\x1b7\x1b[90m{}\x1b[0m\x1b8", ghost)
}
```

**Step 4: Run tests**

Run: `cargo test -p omnish-client display::tests 2>&1 | tail -20`
Expected: All display tests pass.

**Step 5: Commit**

```bash
git add crates/omnish-client/src/display.rs
git commit -m "feat(client): add render_ghost_text for completion UI"
```

---

### Task 3: Tab handling in InputInterceptor

**Files:**
- Modify: `crates/omnish-client/src/interceptor.rs`

**Step 1: Write the tests**

Add to `interceptor.rs` tests:

```rust
#[test]
fn test_tab_in_chat_mode_returns_tab_action() {
    let mut ic = new_interceptor(":");
    ic.feed_byte(b':');
    ic.feed_byte(b'h');
    // Tab in chat mode should return Tab action
    assert_eq!(ic.feed_byte(b'\t'), InterceptAction::Tab(vec![b':', b'h']));
}

#[test]
fn test_tab_not_in_chat_forwards() {
    let mut ic = new_interceptor(":");
    // Tab without being in chat/buffering mode should forward
    assert_eq!(ic.feed_byte(b'\t'), InterceptAction::Forward(vec![b'\t']));
}

#[test]
fn test_tab_during_prefix_buffering() {
    let mut ic = new_interceptor("::");
    ic.feed_byte(b':');
    // Still matching prefix (not yet in chat) — Tab should forward
    // because we don't have enough context to complete yet
    assert_eq!(ic.feed_byte(b'\t'), InterceptAction::Forward(vec![b':', b'\t']));
}
```

**Step 2: Run to verify failure**

Run: `cargo test -p omnish-client interceptor::tests::test_tab 2>&1 | tail -5`
Expected: `Tab` variant not found on `InterceptAction`.

**Step 3: Implement**

Add `Tab` variant to `InterceptAction`:

```rust
#[derive(Debug, PartialEq)]
pub enum InterceptAction {
    Buffering(Vec<u8>),
    Forward(Vec<u8>),
    Chat(String),
    Backspace(Vec<u8>),
    Cancel,
    Pending,
    /// Tab pressed while in chat mode. Contains current buffer.
    /// Caller should check GhostCompleter for completion to accept.
    Tab(Vec<u8>),
}
```

In `feed_byte`, add Tab handling just before the existing backspace handling block (around line 330):

```rust
// Handle Tab
if byte == b'\t' {
    if self.in_chat {
        let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
        return InterceptAction::Tab(current_buf);
    } else if !self.buffer.is_empty() {
        // During prefix matching, flush buffer + tab to PTY
        let mut flushed: Vec<u8> = self.buffer.iter().copied().collect();
        flushed.push(byte);
        self.buffer.clear();
        return self.forward(flushed);
    } else {
        return self.forward(vec![byte]);
    }
}
```

**Step 4: Run tests**

Run: `cargo test -p omnish-client interceptor::tests 2>&1 | tail -20`
Expected: All interceptor tests pass.

**Step 5: Commit**

```bash
git add crates/omnish-client/src/interceptor.rs
git commit -m "feat(client): add Tab action to InputInterceptor for completion"
```

---

### Task 4: Wire GhostCompleter into main.rs

**Files:**
- Modify: `crates/omnish-client/src/main.rs`

**Step 1: No unit test needed** — this is integration wiring in the main loop. Verified by existing tests passing + manual testing.

**Step 2: Add completer initialization after interceptor creation (~line 104)**

```rust
let completer = ghost_complete::GhostCompleter::new(vec![
    Box::new(ghost_complete::BuiltinProvider::new()),
]);
```

**Step 3: In the `Buffering` action handler (~line 146), after rendering input echo, add ghost text rendering**

Replace the block that handles `InterceptAction::Buffering(buf)` when `buf.len() > 1 && buf.starts_with(b":")`:

```rust
InterceptAction::Buffering(buf) => {
    if buf == b":" {
        dismiss_col = col_tracker.col;
        let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
        let prompt = display::render_prompt(cols);
        nix::unistd::write(std::io::stdout(), prompt.as_bytes()).ok();
    } else if buf.len() > 1 && buf.starts_with(b":") {
        let user_input = &buf[1..];
        let echo = display::render_input_echo(user_input);
        nix::unistd::write(std::io::stdout(), echo.as_bytes()).ok();

        // Query completer for ghost text
        if let Ok(input_str) = std::str::from_utf8(user_input) {
            if let Some(ghost) = completer.update(input_str) {
                let ghost_render = display::render_ghost_text(ghost);
                nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
            }
        }
    }
}
```

**Step 4: Add Tab handler after the Buffering handler**

```rust
InterceptAction::Tab(buf) => {
    // Check if completer has a suggestion to accept
    if let Some(suffix) = completer.accept() {
        // Append suffix bytes to interceptor buffer
        for &b in suffix.as_bytes() {
            interceptor.inject_byte(b);
        }
        // Re-render with updated buffer
        let new_buf = interceptor.current_buffer();
        if new_buf.len() > 1 && new_buf.starts_with(b":") {
            let user_input = &new_buf[1..];
            let echo = display::render_input_echo(user_input);
            nix::unistd::write(std::io::stdout(), echo.as_bytes()).ok();

            // Query for next ghost after accepting
            if let Ok(input_str) = std::str::from_utf8(user_input) {
                if let Some(ghost) = completer.update(input_str) {
                    let ghost_render = display::render_ghost_text(ghost);
                    nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
                }
            }
        }
    }
    // If no ghost, Tab is silently ignored
}
```

**Step 5: Add helper methods to InputInterceptor**

In `interceptor.rs`, add:

```rust
/// Inject bytes directly into the buffer (for accepting completions).
pub fn inject_byte(&mut self, byte: u8) {
    self.buffer.push_back(byte);
}

/// Get a copy of the current buffer contents.
pub fn current_buffer(&self) -> Vec<u8> {
    self.buffer.iter().copied().collect()
}
```

**Step 6: Clear completer on Cancel and Chat actions**

In the `Cancel` handler, add `completer.clear();`
In the `Chat` handler, add `completer.clear();`
In the `Backspace` handler where ghost may be stale, the next `Buffering` re-query handles it.

**Step 7: Run full test suite**

Run: `cargo test -p omnish-client 2>&1 | tail -20`
Expected: All tests pass.

**Step 8: Commit**

```bash
git add crates/omnish-client/src/main.rs crates/omnish-client/src/interceptor.rs
git commit -m "feat(client): wire GhostCompleter into main loop with Tab accept"
```

---

### Task 5: Workspace verification

**Step 1: Run full workspace tests**

Run: `cargo test --workspace 2>&1 | tail -10`
Expected: All tests pass, no warnings.

**Step 2: Build release check**

Run: `cargo build --release 2>&1 | tail -5`
Expected: Compiles without errors (release build has `#[cfg(not(debug_assertions))]` path for commands).
