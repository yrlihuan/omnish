# Context Strategy/Formatter Separation Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Separate context building into three layers: `ContextStrategy` selects commands, a middle layer reads stream data into `CommandContext` structs, and `ContextFormatter` produces the final text.

**Architecture:** `ContextStrategy` trait changes from `build_context() -> String` to `select_commands() -> Vec<CommandRecord>`. New `CommandContext` struct holds pre-processed per-command data (command_line, cwd, timestamps, cleaned output text). New `ContextFormatter` trait takes `&[CommandContext]` and produces formatted text. A `build_context()` free function orchestrates: strategy selects → stream read + ANSI strip → formatter formats. `RecentCommands` becomes a pure selector; `DefaultFormatter` handles `$ cmd\noutput` formatting with line truncation.

**Tech Stack:** Rust, async-trait, omnish-store types

---

### Task 1: Add CommandContext struct and ContextFormatter trait, change ContextStrategy trait

**Files:**
- Modify: `crates/omnish-context/src/lib.rs`

**Step 1: Rewrite lib.rs**

Replace the current content with the new three-layer interface:

```rust
pub mod recent;

use anyhow::Result;
use async_trait::async_trait;
use omnish_store::command::CommandRecord;
use omnish_store::stream::StreamEntry;

/// Pre-processed command data, ready for formatting.
pub struct CommandContext {
    pub command_line: Option<String>,
    pub cwd: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub output: String,
}

/// Reads stream entries for a given command's byte range.
pub trait StreamReader: Send + Sync {
    fn read_command_output(&self, offset: u64, length: u64) -> Result<Vec<StreamEntry>>;
}

/// Selects which commands to include in context.
#[async_trait]
pub trait ContextStrategy: Send + Sync {
    async fn select_commands<'a>(&self, commands: &'a [CommandRecord]) -> Vec<&'a CommandRecord>;
}

/// Formats selected commands into the final context string.
pub trait ContextFormatter: Send + Sync {
    fn format(&self, commands: &[CommandContext]) -> String;
}

/// Orchestrates: strategy selects commands, reads stream data, formatter produces text.
pub async fn build_context(
    strategy: &dyn ContextStrategy,
    formatter: &dyn ContextFormatter,
    commands: &[CommandRecord],
    reader: &dyn StreamReader,
) -> Result<String> {
    let selected = strategy.select_commands(commands).await;

    let mut contexts = Vec::new();
    for cmd in selected {
        let entries = reader.read_command_output(cmd.stream_offset, cmd.stream_length)?;

        let mut raw_bytes = Vec::new();
        for entry in &entries {
            if entry.direction == 1 {
                raw_bytes.extend_from_slice(&entry.data);
            }
        }

        let output = strip_ansi(&raw_bytes);

        contexts.push(CommandContext {
            command_line: cmd.command_line.clone(),
            cwd: cmd.cwd.clone(),
            started_at: cmd.started_at,
            ended_at: cmd.ended_at,
            output,
        });
    }

    Ok(formatter.format(&contexts))
}

/// Strip ANSI escape sequences from raw bytes.
pub fn strip_ansi(raw: &[u8]) -> String {
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
```

Key changes from current code:
- `ContextStrategy::build_context()` replaced by `ContextStrategy::select_commands()` returning `Vec<&CommandRecord>`
- New `CommandContext` struct
- New `ContextFormatter` trait
- New `build_context()` free function as orchestrator
- `strip_ansi` moved from `recent.rs` to `lib.rs` (public, used by orchestrator)

**Step 2: Verify it compiles (will fail — recent.rs uses old trait, that's Task 2)**

Run: `cargo check -p omnish-context 2>&1 | head -5`
Expected: errors in recent.rs (expected, fixed in Task 2)

**Step 3: Commit**

Do NOT commit yet — wait for Task 2 to fix recent.rs first.

---

### Task 2: Refactor RecentCommands into strategy + DefaultFormatter

**Files:**
- Rewrite: `crates/omnish-context/src/recent.rs`

**Step 1: Rewrite recent.rs**

`RecentCommands` becomes a pure selector (implements `ContextStrategy::select_commands`). New `DefaultFormatter` handles formatting with line truncation.

```rust
use async_trait::async_trait;
use omnish_store::command::CommandRecord;

use crate::{CommandContext, ContextFormatter, ContextStrategy};

const MAX_COMMANDS: usize = 10;
const MAX_OUTPUT_LINES: usize = 20;
const HEAD_LINES: usize = 10;
const TAIL_LINES: usize = 10;

/// Selects the most recent N commands.
pub struct RecentCommands {
    max: usize,
}

impl RecentCommands {
    pub fn new() -> Self {
        Self { max: MAX_COMMANDS }
    }
}

#[async_trait]
impl ContextStrategy for RecentCommands {
    async fn select_commands<'a>(&self, commands: &'a [CommandRecord]) -> Vec<&'a CommandRecord> {
        if commands.len() > self.max {
            commands[commands.len() - self.max..].iter().collect()
        } else {
            commands.iter().collect()
        }
    }
}

/// Formats commands as `$ cmd\noutput`, with line truncation for long output.
pub struct DefaultFormatter {
    max_output_lines: usize,
    head_lines: usize,
    tail_lines: usize,
}

impl DefaultFormatter {
    pub fn new() -> Self {
        Self {
            max_output_lines: MAX_OUTPUT_LINES,
            head_lines: HEAD_LINES,
            tail_lines: TAIL_LINES,
        }
    }
}

impl ContextFormatter for DefaultFormatter {
    fn format(&self, commands: &[CommandContext]) -> String {
        let mut sections = Vec::new();

        for cmd in commands {
            let cmd_line = cmd
                .command_line
                .as_deref()
                .unwrap_or("(unknown)");

            let output = self.truncate_lines(&cmd.output);

            if output.is_empty() {
                sections.push(format!("$ {}", cmd_line));
            } else {
                sections.push(format!("$ {}\n{}", cmd_line, output));
            }
        }

        sections.join("\n\n")
    }
}

impl DefaultFormatter {
    fn truncate_lines(&self, text: &str) -> String {
        let lines: Vec<&str> = text
            .lines()
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| !l.is_empty())
            .collect();

        let total = lines.len();
        if total <= self.max_output_lines {
            lines.join("\n")
        } else {
            let head = &lines[..self.head_lines];
            let tail = &lines[total - self.tail_lines..];
            let omitted = total - self.head_lines - self.tail_lines;
            format!(
                "{}\n... ({} lines omitted) ...\n{}",
                head.join("\n"),
                omitted,
                tail.join("\n")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StreamReader;
    use anyhow::Result;
    use omnish_store::stream::StreamEntry;

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
            command_id: format!("sess:{}", seq),
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

    // --- Strategy tests ---

    #[tokio::test]
    async fn test_select_empty() {
        let strategy = RecentCommands::new();
        let result = strategy.select_commands(&[]).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_select_max_recent() {
        let strategy = RecentCommands::new();
        let cmds: Vec<_> = (0..15)
            .map(|i| make_cmd(i, Some(&format!("cmd{}", i))))
            .collect();
        let selected = strategy.select_commands(&cmds).await;
        assert_eq!(selected.len(), 10);
        assert_eq!(selected[0].command_line.as_deref(), Some("cmd5"));
        assert_eq!(selected[9].command_line.as_deref(), Some("cmd14"));
    }

    // --- Formatter tests ---

    #[test]
    fn test_format_single_command() {
        let formatter = DefaultFormatter::new();
        let contexts = vec![CommandContext {
            command_line: Some("ls".into()),
            cwd: None,
            started_at: 1000,
            ended_at: Some(1050),
            output: "file1.txt\nfile2.txt".into(),
        }];
        let result = formatter.format(&contexts);
        assert_eq!(result, "$ ls\nfile1.txt\nfile2.txt");
    }

    #[test]
    fn test_format_truncates_long_output() {
        let formatter = DefaultFormatter::new();
        let mut output = String::new();
        for i in 0..30 {
            output.push_str(&format!("line {}\n", i));
        }
        let contexts = vec![CommandContext {
            command_line: Some("long-cmd".into()),
            cwd: None,
            started_at: 1000,
            ended_at: Some(1050),
            output,
        }];
        let result = formatter.format(&contexts);
        assert!(result.contains("line 0"));
        assert!(result.contains("line 9"));
        assert!(result.contains("... (10 lines omitted) ..."));
        assert!(result.contains("line 20"));
        assert!(result.contains("line 29"));
        assert!(!result.contains("line 10\n"));
    }

    #[test]
    fn test_format_unknown_command() {
        let formatter = DefaultFormatter::new();
        let contexts = vec![CommandContext {
            command_line: None,
            cwd: None,
            started_at: 1000,
            ended_at: None,
            output: "output".into(),
        }];
        let result = formatter.format(&contexts);
        assert!(result.contains("$ (unknown)"));
    }

    // --- Integration test: build_context orchestrator ---

    #[tokio::test]
    async fn test_build_context_filters_direction() {
        let strategy = RecentCommands::new();
        let formatter = DefaultFormatter::new();
        let reader = MockReader::new(vec![
            make_input_entry("ls\r"),
            make_output_entry("file1.txt\n"),
        ]);
        let cmds = vec![make_cmd(0, Some("ls"))];
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader)
            .await
            .unwrap();
        assert_eq!(result, "$ ls\nfile1.txt");
    }

    #[tokio::test]
    async fn test_build_context_empty() {
        let strategy = RecentCommands::new();
        let formatter = DefaultFormatter::new();
        let reader = MockReader::empty();
        let result = crate::build_context(&strategy, &formatter, &[], &reader)
            .await
            .unwrap();
        assert_eq!(result, "");
    }
}
```

Tests are now split by concern:
- 2 strategy tests (`test_select_empty`, `test_select_max_recent`)
- 3 formatter tests (`test_format_single_command`, `test_format_truncates_long_output`, `test_format_unknown_command`)
- 2 integration tests via `build_context()` (`test_build_context_filters_direction`, `test_build_context_empty`)

**Step 2: Verify**

Run: `cargo test -p omnish-context`
Expected: 7 tests pass

**Step 3: Commit**

```
refactor(context): separate ContextStrategy selection from ContextFormatter
```

---

### Task 3: Update SessionManager to use new three-layer API

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs`

**Step 1: Update imports and context methods**

Replace:
```rust
use omnish_context::{ContextStrategy, StreamReader};
use omnish_context::recent::RecentCommands;
```

With:
```rust
use omnish_context::StreamReader;
use omnish_context::recent::{RecentCommands, DefaultFormatter};
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
    let formatter = DefaultFormatter::new();
    omnish_context::build_context(&strategy, &formatter, &session.commands, &reader).await
}
```

Rewrite `get_all_sessions_context`:
```rust
pub async fn get_all_sessions_context(&self) -> Result<String> {
    let sessions = self.sessions.lock().await;
    let strategy = RecentCommands::new();
    let formatter = DefaultFormatter::new();
    let mut parts = Vec::new();

    for (sid, session) in sessions.iter() {
        let reader = FileStreamReader {
            stream_path: session.dir.join("stream.bin"),
        };
        match omnish_context::build_context(&strategy, &formatter, &session.commands, &reader).await {
            Ok(ctx) if !ctx.is_empty() => {
                parts.push(format!("=== Session {} ===\n{}", sid, ctx));
            }
            _ => {}
        }
    }

    Ok(parts.join("\n\n"))
}
```

**Step 2: Verify**

Run: `cargo test --workspace`
Expected: all tests pass

**Step 3: Commit**

```
refactor(daemon): use separated strategy + formatter for context building
```
