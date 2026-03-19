# Context Strategy Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace raw stream concatenation with a command-based `ContextStrategy` trait in a new `omnish-context` crate, with a `RecentCommands` implementation that uses the last 10 commands with 10+10 line truncation.

**Architecture:** New `omnish-context` crate defines `StreamReader` and `ContextStrategy` traits. `RecentCommands` is the first strategy: loads last N commands from `CommandRecord` list, reads direction=1 stream data via `StreamReader`, strips ANSI, truncates long output (>20 lines → first 10 + last 10). `SessionManager` delegates to the strategy instead of doing raw concatenation.

**Tech Stack:** Rust, async-trait, omnish-store types (CommandRecord, StreamEntry, read_range)

---

### Task 1: Create omnish-context crate with traits

**Files:**
- Create: `crates/omnish-context/Cargo.toml`
- Create: `crates/omnish-context/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

**Step 1: Create Cargo.toml**

```toml
[package]
name = "omnish-context"
version = "0.1.0"
edition = "2021"

[dependencies]
omnish-store = { path = "../omnish-store" }
anyhow = { workspace = true }
async-trait = "0.1"

[dev-dependencies]
tokio = { workspace = true }
```

**Step 2: Create lib.rs with traits**

```rust
use anyhow::Result;
use async_trait::async_trait;
use omnish_store::command::CommandRecord;
use omnish_store::stream::StreamEntry;

/// Reads stream entries for a given command's byte range.
pub trait StreamReader: Send + Sync {
    fn read_command_output(&self, offset: u64, length: u64) -> Result<Vec<StreamEntry>>;
}

/// Strategy for assembling LLM context from command history.
#[async_trait]
pub trait ContextStrategy: Send + Sync {
    async fn build_context(
        &self,
        commands: &[CommandRecord],
        reader: &dyn StreamReader,
    ) -> Result<String>;
}
```

**Step 3: Add to workspace**

In root `Cargo.toml`, add `"crates/omnish-context"` to the `members` list (after `omnish-store`).

**Step 4: Verify it compiles**

Run: `cargo check -p omnish-context`
Expected: success

**Step 5: Commit**

```
feat(context): add omnish-context crate with ContextStrategy trait
```

---

### Task 2: Implement RecentCommands strategy with tests

**Files:**
- Create: `crates/omnish-context/src/recent.rs`
- Modify: `crates/omnish-context/src/lib.rs` (add `pub mod recent;`)

**Step 1: Write tests in recent.rs**

Tests use a mock `StreamReader` that returns pre-built entries. Cover:

1. `test_empty_commands` — no commands → empty string
2. `test_single_command` — one command with short output → `$ cmd\noutput`
3. `test_truncates_long_output` — 30 lines → first 10 + `... (10 lines omitted) ...` + last 10
4. `test_max_recent_commands` — 15 commands → only last 10 appear
5. `test_filters_direction_1_only` — mixed direction entries → only output (direction=1) used
6. `test_command_without_command_line` — `command_line: None` → uses `(unknown)` placeholder

```rust
use anyhow::Result;
use async_trait::async_trait;
use omnish_store::command::CommandRecord;
use omnish_store::stream::StreamEntry;

use crate::{ContextStrategy, StreamReader};

const MAX_COMMANDS: usize = 10;
const MAX_OUTPUT_LINES: usize = 20;
const HEAD_LINES: usize = 10;
const TAIL_LINES: usize = 10;

pub struct RecentCommands;

impl RecentCommands {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ContextStrategy for RecentCommands {
    async fn build_context(
        &self,
        commands: &[CommandRecord],
        reader: &dyn StreamReader,
    ) -> Result<String> {
        let recent = if commands.len() > MAX_COMMANDS {
            &commands[commands.len() - MAX_COMMANDS..]
        } else {
            commands
        };

        let mut sections = Vec::new();

        for cmd in recent {
            let cmd_line = cmd
                .command_line
                .as_deref()
                .unwrap_or("(unknown)");

            let entries = reader.read_command_output(cmd.stream_offset, cmd.stream_length)?;

            // Collect only direction=1 (PTY output) bytes
            let mut raw_bytes = Vec::new();
            for entry in &entries {
                if entry.direction == 1 {
                    raw_bytes.extend_from_slice(&entry.data);
                }
            }

            let cleaned = strip_ansi(&raw_bytes);
            let output = truncate_lines(&cleaned);

            if output.is_empty() {
                sections.push(format!("$ {}", cmd_line));
            } else {
                sections.push(format!("$ {}\n{}", cmd_line, output));
            }
        }

        Ok(sections.join("\n\n"))
    }
}

/// Strip ANSI escape sequences from raw bytes.
fn strip_ansi(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Split into lines, truncate if over MAX_OUTPUT_LINES.
/// Keep first HEAD_LINES and last TAIL_LINES.
fn truncate_lines(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .collect();

    let total = lines.len();
    if total <= MAX_OUTPUT_LINES {
        lines.join("\n")
    } else {
        let head = &lines[..HEAD_LINES];
        let tail = &lines[total - TAIL_LINES..];
        let omitted = total - HEAD_LINES - TAIL_LINES;
        format!(
            "{}\n... ({} lines omitted) ...\n{}",
            head.join("\n"),
            omitted,
            tail.join("\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock StreamReader for tests
    struct MockReader {
        entries: Vec<StreamEntry>,
    }

    impl MockReader {
        fn new(entries: Vec<StreamEntry>) -> Self {
            Self { entries }
        }

        fn empty() -> Self {
            Self { entries: vec![] }
        }
    }

    impl StreamReader for MockReader {
        fn read_command_output(&self, _offset: u64, _length: u64) -> Result<Vec<StreamEntry>> {
            Ok(self.entries.clone())
        }
    }

    fn make_cmd(seq: u32, cmd_line: Option<&str>) -> CommandRecord {
        CommandRecord {
            command_id: format!("sess:{}",seq),
            session_id: "sess".into(),
            command_line: cmd_line.map(|s| s.to_string()),
            cwd: None,
            started_at: 1000 + seq as u64 * 100,
            ended_at: Some(1000 + seq as u64 * 100 + 50),
            output_summary: String::new(),
            stream_offset: 0,
            stream_length: 100,
        }
    }

    fn make_output_entry(text: &str) -> StreamEntry {
        StreamEntry {
            timestamp_ms: 1000,
            direction: 1,
            data: text.as_bytes().to_vec(),
        }
    }

    fn make_input_entry(text: &str) -> StreamEntry {
        StreamEntry {
            timestamp_ms: 1000,
            direction: 0,
            data: text.as_bytes().to_vec(),
        }
    }

    #[tokio::test]
    async fn test_empty_commands() {
        let strategy = RecentCommands::new();
        let reader = MockReader::empty();
        let result = strategy.build_context(&[], &reader).await.unwrap();
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn test_single_command() {
        let strategy = RecentCommands::new();
        let reader = MockReader::new(vec![
            make_output_entry("file1.txt\nfile2.txt\n"),
        ]);
        let cmds = vec![make_cmd(0, Some("ls"))];
        let result = strategy.build_context(&cmds, &reader).await.unwrap();
        assert_eq!(result, "$ ls\nfile1.txt\nfile2.txt");
    }

    #[tokio::test]
    async fn test_truncates_long_output() {
        let strategy = RecentCommands::new();
        let mut lines = String::new();
        for i in 0..30 {
            lines.push_str(&format!("line {}\n", i));
        }
        let reader = MockReader::new(vec![make_output_entry(&lines)]);
        let cmds = vec![make_cmd(0, Some("long-cmd"))];
        let result = strategy.build_context(&cmds, &reader).await.unwrap();

        assert!(result.contains("$ long-cmd"));
        assert!(result.contains("line 0"));
        assert!(result.contains("line 9"));
        assert!(result.contains("... (10 lines omitted) ..."));
        assert!(result.contains("line 20"));
        assert!(result.contains("line 29"));
        // Lines 10-19 should NOT appear
        assert!(!result.contains("line 10\n"));
    }

    #[tokio::test]
    async fn test_max_recent_commands() {
        let strategy = RecentCommands::new();
        let reader = MockReader::new(vec![make_output_entry("out\n")]);
        let cmds: Vec<_> = (0..15)
            .map(|i| make_cmd(i, Some(&format!("cmd{}", i))))
            .collect();
        let result = strategy.build_context(&cmds, &reader).await.unwrap();

        // First 5 commands should NOT appear
        assert!(!result.contains("$ cmd0"));
        assert!(!result.contains("$ cmd4"));
        // Last 10 should appear
        assert!(result.contains("$ cmd5"));
        assert!(result.contains("$ cmd14"));
    }

    #[tokio::test]
    async fn test_filters_direction_1_only() {
        let strategy = RecentCommands::new();
        let reader = MockReader::new(vec![
            make_input_entry("ls\r"),           // direction=0, should be ignored
            make_output_entry("file1.txt\n"),   // direction=1, included
        ]);
        let cmds = vec![make_cmd(0, Some("ls"))];
        let result = strategy.build_context(&cmds, &reader).await.unwrap();

        assert_eq!(result, "$ ls\nfile1.txt");
        // Should NOT contain the raw input
        assert!(!result.contains("ls\r"));
    }

    #[tokio::test]
    async fn test_command_without_command_line() {
        let strategy = RecentCommands::new();
        let reader = MockReader::new(vec![make_output_entry("output\n")]);
        let cmds = vec![make_cmd(0, None)];
        let result = strategy.build_context(&cmds, &reader).await.unwrap();
        assert!(result.contains("$ (unknown)"));
    }
}
```

**Step 2: Add module to lib.rs**

Add `pub mod recent;` to `crates/omnish-context/src/lib.rs`.

**Step 3: Run tests**

Run: `cargo test -p omnish-context`
Expected: 6 tests pass

**Step 4: Commit**

```
feat(context): implement RecentCommands strategy with tests
```

---

### Task 3: Integrate strategy into SessionManager

**Files:**
- Modify: `crates/omnish-daemon/Cargo.toml` (add `omnish-context` dependency)
- Modify: `crates/omnish-daemon/src/session_mgr.rs`

**Step 1: Add omnish-context dependency**

In `crates/omnish-daemon/Cargo.toml`, add:
```toml
omnish-context = { path = "../omnish-context" }
```

**Step 2: Add StreamReader impl and rewrite get_session_context**

In `session_mgr.rs`:

1. Add a `FileStreamReader` struct that wraps a `PathBuf` (stream.bin path) and implements `StreamReader` by calling `read_range()`.
2. Rewrite `get_session_context()`:
   - Load commands from the session's in-memory `commands` vec
   - Create `FileStreamReader` for the session's stream.bin
   - Call `RecentCommands.build_context(commands, reader)`
3. Rewrite `get_all_sessions_context()`:
   - For each session, build context with the strategy
   - Concatenate with session headers

```rust
use omnish_context::{ContextStrategy, StreamReader};
use omnish_context::recent::RecentCommands;
use omnish_store::stream::{read_range, StreamEntry};

struct FileStreamReader {
    stream_path: PathBuf,
}

impl StreamReader for FileStreamReader {
    fn read_command_output(&self, offset: u64, length: u64) -> Result<Vec<StreamEntry>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        read_range(&self.stream_path, offset, length)
    }
}
```

Rewrite `get_session_context`:
```rust
pub async fn get_session_context(&self, session_id: &str) -> Result<String> {
    let sessions = self.sessions.lock().await;
    let session = sessions
        .get(session_id)
        .ok_or_else(|| anyhow!("session not found: {}", session_id))?;

    let reader = FileStreamReader {
        stream_path: session.dir.join("stream.bin"),
    };
    let strategy = RecentCommands::new();
    strategy.build_context(&session.commands, &reader).await
}
```

Rewrite `get_all_sessions_context`:
```rust
pub async fn get_all_sessions_context(&self) -> Result<String> {
    let sessions = self.sessions.lock().await;
    let strategy = RecentCommands::new();
    let mut parts = Vec::new();

    for (sid, session) in sessions.iter() {
        let reader = FileStreamReader {
            stream_path: session.dir.join("stream.bin"),
        };
        match strategy.build_context(&session.commands, &reader).await {
            Ok(ctx) if !ctx.is_empty() => {
                parts.push(format!("=== Session {} ===\n{}", sid, ctx));
            }
            _ => {}
        }
    }

    Ok(parts.join("\n\n"))
}
```

**Step 3: Remove old imports**

Remove `use omnish_llm::context::ContextBuilder;` and `use omnish_store::stream::read_entries;` from session_mgr.rs (no longer needed).

**Step 4: Verify compilation**

Run: `cargo check -p omnish-daemon`
Expected: success

**Step 5: Run all workspace tests**

Run: `cargo test --workspace`
Expected: all tests pass

**Step 6: Commit**

```
refactor(daemon): delegate context building to ContextStrategy
```

---

### Task 4: Cleanup — remove unused ContextBuilder

After the migration, `omnish-llm::context::ContextBuilder` is no longer used by session_mgr. Check if it's used elsewhere; if not, remove it.

**Files:**
- Possibly delete: `crates/omnish-llm/src/context.rs`
- Possibly modify: `crates/omnish-llm/src/lib.rs` (remove `pub mod context;`)

**Step 1: Search for remaining usages**

Search for `ContextBuilder` and `omnish_llm::context` across the workspace. If only tests reference it, remove those too.

**Step 2: Remove if unused**

Delete `context.rs` and remove `pub mod context;` from `omnish-llm/src/lib.rs`.

**Step 3: Verify**

Run: `cargo test --workspace`
Expected: all pass

**Step 4: Commit**

```
refactor(llm): remove unused ContextBuilder
```
