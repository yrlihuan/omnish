# Context Formatter Enhancements Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add session_id to CommandContext, implement relative time formatting, rename sessions to "term A/B/C", and build two formatter variants: GroupedFormatter (by session) and InterleavedFormatter (by time).

**Architecture:** Add `session_id` to `CommandContext` and `build_context()`. Extract shared formatting helpers (relative time, line truncation, command line rendering) into `format_utils.rs`. Rename `DefaultFormatter` to `GroupedFormatter` in `recent.rs`, add `InterleavedFormatter`. Both formatters take `current_session_id` and `now_ms` at construction. Session naming maps unique session_ids to "term A", "term B", etc., with current session always assigned "term A".

**Tech Stack:** Rust, async-trait, omnish-store types

---

### Task 1: Add session_id to CommandContext and build_context

**Files:**
- Modify: `crates/omnish-context/src/lib.rs`

**Step 1: Add session_id field to CommandContext**

```rust
pub struct CommandContext {
    pub session_id: String,          // NEW
    pub command_line: Option<String>,
    pub cwd: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub output: String,
}
```

**Step 2: Update build_context to populate session_id**

In the `build_context()` function, add `session_id` when constructing `CommandContext`:

```rust
contexts.push(CommandContext {
    session_id: cmd.session_id.clone(),  // NEW
    command_line: cmd.command_line.clone(),
    cwd: cmd.cwd.clone(),
    started_at: cmd.started_at,
    ended_at: cmd.ended_at,
    output,
});
```

**Step 3: Fix all test compilation errors in recent.rs**

Every `CommandContext` literal in tests needs `session_id: "sess".into()` added. This will be done in Task 2 together with the formatter rewrite.

Do NOT commit yet — code won't compile until Task 2 updates the tests.

---

### Task 2: Extract format_utils and implement shared helpers

**Files:**
- Create: `crates/omnish-context/src/format_utils.rs`
- Modify: `crates/omnish-context/src/lib.rs` (add `pub mod format_utils;`)

Shared helpers used by both formatters:

```rust
/// Format millisecond timestamp as relative time string.
/// Rules: <60s → "Ns ago", <60m → "Nm ago", <24h → "Nh ago", ≥24h → "Nd ago"
pub fn format_relative_time(timestamp_ms: u64, now_ms: u64) -> String {
    if now_ms <= timestamp_ms {
        return "just now".to_string();
    }
    let diff_secs = (now_ms - timestamp_ms) / 1000;
    if diff_secs < 60 {
        format!("{}s ago", diff_secs)
    } else if diff_secs < 3600 {
        format!("{}m ago", diff_secs / 60)
    } else if diff_secs < 86400 {
        format!("{}h ago", diff_secs / 3600)
    } else {
        format!("{}d ago", diff_secs / 86400)
    }
}

/// Assign session labels: current session → "term A", others → "term B", "term C", etc.
/// Returns a map from session_id to label.
pub fn assign_term_labels(
    commands: &[super::CommandContext],
    current_session_id: &str,
) -> std::collections::HashMap<String, String> {
    let mut labels = std::collections::HashMap::new();
    labels.insert(current_session_id.to_string(), "term A".to_string());

    let mut next_letter = b'B';
    for cmd in commands {
        if !labels.contains_key(&cmd.session_id) {
            labels.insert(
                cmd.session_id.clone(),
                format!("term {}", next_letter as char),
            );
            next_letter += 1;
        }
    }
    labels
}

/// Truncate output lines. If over max_lines, keep head_lines + tail_lines.
pub fn truncate_lines(text: &str, max_lines: usize, head: usize, tail: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .collect();

    let total = lines.len();
    if total <= max_lines {
        lines.join("\n")
    } else {
        let head_lines = &lines[..head];
        let tail_lines = &lines[total - tail..];
        let omitted = total - head - tail;
        format!(
            "{}\n... ({} lines omitted) ...\n{}",
            head_lines.join("\n"),
            omitted,
            tail_lines.join("\n")
        )
    }
}
```

**Tests** (in `format_utils.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relative_time_seconds() {
        assert_eq!(format_relative_time(59_000, 60_000), "1s ago");
        assert_eq!(format_relative_time(10_000, 10_000), "just now");
    }

    #[test]
    fn test_relative_time_minutes() {
        assert_eq!(format_relative_time(0, 120_000), "2m ago");
        assert_eq!(format_relative_time(0, 3_599_000), "59m ago");
    }

    #[test]
    fn test_relative_time_hours() {
        assert_eq!(format_relative_time(0, 3_600_000), "1h ago");
        assert_eq!(format_relative_time(0, 86_399_000), "23h ago");
    }

    #[test]
    fn test_relative_time_days() {
        assert_eq!(format_relative_time(0, 86_400_000), "1d ago");
    }

    #[test]
    fn test_relative_time_future() {
        assert_eq!(format_relative_time(100_000, 50_000), "just now");
    }

    #[test]
    fn test_assign_labels_current_first() {
        let contexts = vec![
            crate::CommandContext {
                session_id: "other-sess".into(),
                command_line: Some("ls".into()),
                cwd: None,
                started_at: 1000,
                ended_at: None,
                output: String::new(),
            },
            crate::CommandContext {
                session_id: "my-sess".into(),
                command_line: Some("pwd".into()),
                cwd: None,
                started_at: 2000,
                ended_at: None,
                output: String::new(),
            },
        ];
        let labels = assign_term_labels(&contexts, "my-sess");
        assert_eq!(labels["my-sess"], "term A");
        assert_eq!(labels["other-sess"], "term B");
    }

    #[test]
    fn test_assign_labels_single_session() {
        let contexts = vec![
            crate::CommandContext {
                session_id: "sess1".into(),
                command_line: None,
                cwd: None,
                started_at: 1000,
                ended_at: None,
                output: String::new(),
            },
        ];
        let labels = assign_term_labels(&contexts, "sess1");
        assert_eq!(labels.len(), 1);
        assert_eq!(labels["sess1"], "term A");
    }

    #[test]
    fn test_truncate_lines_short() {
        let result = truncate_lines("line1\nline2\nline3", 20, 10, 10);
        assert_eq!(result, "line1\nline2\nline3");
    }

    #[test]
    fn test_truncate_lines_long() {
        let mut text = String::new();
        for i in 0..30 {
            text.push_str(&format!("line {}\n", i));
        }
        let result = truncate_lines(&text, 20, 10, 10);
        assert!(result.contains("line 0"));
        assert!(result.contains("line 9"));
        assert!(result.contains("... (10 lines omitted) ..."));
        assert!(result.contains("line 20"));
        assert!(!result.contains("line 10\n"));
    }
}
```

**Step: Add module**

Add `pub mod format_utils;` to `lib.rs`.

**Step: Verify**

Run: `cargo test -p omnish-context -- format_utils`
Expected: 9 tests pass

**Step: Commit**

```
feat(context): add format_utils with relative time, term labels, and line truncation
```

---

### Task 3: Rewrite GroupedFormatter and add InterleavedFormatter

**Files:**
- Rewrite: `crates/omnish-context/src/recent.rs`

Rename `DefaultFormatter` → `GroupedFormatter`. Add `InterleavedFormatter`. Both take `current_session_id` and `now_ms` at construction. Remove the old `truncate_lines` method (now in `format_utils`).

**GroupedFormatter output format:**

```
--- term A (current) ---

[2m ago] $ ls
file1.txt  file2.txt

[30s ago] $ echo hello
hello

--- term B ---

[5m ago] $ npm start
Server running on port 3000
```

**InterleavedFormatter output format:**

```
[5m ago] term B $ npm start
Server running on port 3000

[2m ago] term A* $ ls
file1.txt  file2.txt
```

(`*` marks current session)

**Full replacement for recent.rs:**

```rust
use async_trait::async_trait;
use omnish_store::command::CommandRecord;

use crate::{CommandContext, ContextFormatter, ContextStrategy};
use crate::format_utils::{assign_term_labels, format_relative_time, truncate_lines};

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

/// Formats commands grouped by session, with session headers.
///
/// ```text
/// --- term A (current) ---
///
/// [2m ago] $ ls
/// file1.txt
///
/// --- term B ---
///
/// [5m ago] $ npm start
/// Server running on port 3000
/// ```
pub struct GroupedFormatter {
    current_session_id: String,
    now_ms: u64,
}

impl GroupedFormatter {
    pub fn new(current_session_id: &str, now_ms: u64) -> Self {
        Self {
            current_session_id: current_session_id.to_string(),
            now_ms,
        }
    }
}

impl ContextFormatter for GroupedFormatter {
    fn format(&self, commands: &[CommandContext]) -> String {
        if commands.is_empty() {
            return String::new();
        }

        let labels = assign_term_labels(commands, &self.current_session_id);

        // Group commands by session, preserving order of first appearance
        let mut session_order: Vec<String> = Vec::new();
        let mut groups: std::collections::HashMap<String, Vec<&CommandContext>> =
            std::collections::HashMap::new();

        for cmd in commands {
            if !groups.contains_key(&cmd.session_id) {
                session_order.push(cmd.session_id.clone());
            }
            groups.entry(cmd.session_id.clone()).or_default().push(cmd);
        }

        // Put current session first
        if let Some(pos) = session_order.iter().position(|s| s == &self.current_session_id) {
            session_order.remove(pos);
            session_order.insert(0, self.current_session_id.clone());
        }

        let mut sections = Vec::new();
        for sid in &session_order {
            let label = &labels[sid];
            let header = if *sid == self.current_session_id {
                format!("--- {} (current) ---", label)
            } else {
                format!("--- {} ---", label)
            };

            let mut cmd_parts = Vec::new();
            for cmd in &groups[sid] {
                let time = format_relative_time(cmd.started_at, self.now_ms);
                let cmd_line = cmd.command_line.as_deref().unwrap_or("(unknown)");
                let output = truncate_lines(&cmd.output, MAX_OUTPUT_LINES, HEAD_LINES, TAIL_LINES);

                if output.is_empty() {
                    cmd_parts.push(format!("[{}] $ {}", time, cmd_line));
                } else {
                    cmd_parts.push(format!("[{}] $ {}\n{}", time, cmd_line, output));
                }
            }

            sections.push(format!("{}\n\n{}", header, cmd_parts.join("\n\n")));
        }

        sections.join("\n\n")
    }
}

/// Formats commands interleaved by time across all sessions.
///
/// ```text
/// [5m ago] term B $ npm start
/// Server running on port 3000
///
/// [2m ago] term A* $ ls
/// file1.txt
/// ```
pub struct InterleavedFormatter {
    current_session_id: String,
    now_ms: u64,
}

impl InterleavedFormatter {
    pub fn new(current_session_id: &str, now_ms: u64) -> Self {
        Self {
            current_session_id: current_session_id.to_string(),
            now_ms,
        }
    }
}

impl ContextFormatter for InterleavedFormatter {
    fn format(&self, commands: &[CommandContext]) -> String {
        if commands.is_empty() {
            return String::new();
        }

        let labels = assign_term_labels(commands, &self.current_session_id);

        // Sort by started_at
        let mut sorted: Vec<&CommandContext> = commands.iter().collect();
        sorted.sort_by_key(|c| c.started_at);

        let mut sections = Vec::new();
        for cmd in sorted {
            let time = format_relative_time(cmd.started_at, self.now_ms);
            let label = &labels[&cmd.session_id];
            let marker = if cmd.session_id == self.current_session_id {
                format!("{}*", label)
            } else {
                label.clone()
            };
            let cmd_line = cmd.command_line.as_deref().unwrap_or("(unknown)");
            let output = truncate_lines(&cmd.output, MAX_OUTPUT_LINES, HEAD_LINES, TAIL_LINES);

            if output.is_empty() {
                sections.push(format!("[{}] {} $ {}", time, marker, cmd_line));
            } else {
                sections.push(format!("[{}] {} $ {}\n{}", time, marker, cmd_line, output));
            }
        }

        sections.join("\n\n")
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

    fn make_cmd(seq: u32, session_id: &str, cmd_line: Option<&str>) -> CommandRecord {
        CommandRecord {
            command_id: format!("{}:{}", session_id, seq),
            session_id: session_id.into(),
            command_line: cmd_line.map(|s| s.to_string()),
            cwd: None,
            started_at: 1000 + seq as u64 * 100,
            ended_at: Some(1000 + seq as u64 * 100 + 50),
            output_summary: String::new(),
            stream_offset: 0,
            stream_length: 100,
        }
    }

    fn make_ctx(session_id: &str, cmd_line: &str, started_at: u64, output: &str) -> CommandContext {
        CommandContext {
            session_id: session_id.into(),
            command_line: Some(cmd_line.into()),
            cwd: None,
            started_at,
            ended_at: Some(started_at + 50),
            output: output.into(),
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
            .map(|i| make_cmd(i, "sess", Some(&format!("cmd{}", i))))
            .collect();
        let selected = strategy.select_commands(&cmds).await;
        assert_eq!(selected.len(), 10);
        assert_eq!(selected[0].command_line.as_deref(), Some("cmd5"));
        assert_eq!(selected[9].command_line.as_deref(), Some("cmd14"));
    }

    // --- GroupedFormatter tests ---

    #[test]
    fn test_grouped_single_session() {
        let now = 60_000;
        let formatter = GroupedFormatter::new("sess1", now);
        let contexts = vec![
            make_ctx("sess1", "ls", now - 30_000, "file1.txt"),
        ];
        let result = formatter.format(&contexts);
        assert!(result.contains("--- term A (current) ---"));
        assert!(result.contains("[30s ago] $ ls"));
        assert!(result.contains("file1.txt"));
    }

    #[test]
    fn test_grouped_multi_session() {
        let now = 600_000;
        let formatter = GroupedFormatter::new("sess1", now);
        let contexts = vec![
            make_ctx("sess2", "npm start", now - 300_000, "Server running"),
            make_ctx("sess1", "ls", now - 120_000, "file1.txt"),
            make_ctx("sess1", "pwd", now - 30_000, "/home"),
        ];
        let result = formatter.format(&contexts);

        // Current session first
        let pos_a = result.find("--- term A (current) ---").unwrap();
        let pos_b = result.find("--- term B ---").unwrap();
        assert!(pos_a < pos_b, "current session should appear first");

        assert!(result.contains("[2m ago] $ ls"));
        assert!(result.contains("[30s ago] $ pwd"));
        assert!(result.contains("[5m ago] $ npm start"));
    }

    #[test]
    fn test_grouped_empty() {
        let formatter = GroupedFormatter::new("sess1", 1000);
        let result = formatter.format(&[]);
        assert_eq!(result, "");
    }

    // --- InterleavedFormatter tests ---

    #[test]
    fn test_interleaved_sorted_by_time() {
        let now = 600_000;
        let formatter = InterleavedFormatter::new("sess1", now);
        let contexts = vec![
            make_ctx("sess1", "ls", now - 120_000, "file1.txt"),
            make_ctx("sess2", "npm start", now - 300_000, "Server running"),
            make_ctx("sess1", "pwd", now - 30_000, "/home"),
        ];
        let result = formatter.format(&contexts);

        // npm start (5m ago) should appear before ls (2m ago)
        let pos_npm = result.find("npm start").unwrap();
        let pos_ls = result.find("$ ls").unwrap();
        let pos_pwd = result.find("$ pwd").unwrap();
        assert!(pos_npm < pos_ls);
        assert!(pos_ls < pos_pwd);
    }

    #[test]
    fn test_interleaved_marks_current() {
        let now = 60_000;
        let formatter = InterleavedFormatter::new("sess1", now);
        let contexts = vec![
            make_ctx("sess1", "ls", now - 30_000, "file1.txt"),
            make_ctx("sess2", "pwd", now - 20_000, "/home"),
        ];
        let result = formatter.format(&contexts);
        assert!(result.contains("term A*"), "current session should have * marker");
        assert!(result.contains("term B $"), "other session should not have * marker");
    }

    #[test]
    fn test_interleaved_empty() {
        let formatter = InterleavedFormatter::new("sess1", 1000);
        let result = formatter.format(&[]);
        assert_eq!(result, "");
    }

    // --- Integration: build_context with new formatters ---

    #[tokio::test]
    async fn test_build_context_grouped() {
        let strategy = RecentCommands::new();
        let formatter = GroupedFormatter::new("sess", 60_000);
        let reader = MockReader::new(vec![make_output_entry("file1.txt\n")]);
        let cmds = vec![make_cmd(0, "sess", Some("ls"))];
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader)
            .await
            .unwrap();
        assert!(result.contains("--- term A (current) ---"));
        assert!(result.contains("$ ls"));
        assert!(result.contains("file1.txt"));
    }

    #[tokio::test]
    async fn test_build_context_interleaved() {
        let strategy = RecentCommands::new();
        let formatter = InterleavedFormatter::new("sess", 60_000);
        let reader = MockReader::new(vec![make_output_entry("file1.txt\n")]);
        let cmds = vec![make_cmd(0, "sess", Some("ls"))];
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader)
            .await
            .unwrap();
        assert!(result.contains("term A*"));
        assert!(result.contains("$ ls"));
    }

    #[tokio::test]
    async fn test_build_context_filters_direction() {
        let strategy = RecentCommands::new();
        let formatter = GroupedFormatter::new("sess", 60_000);
        let reader = MockReader::new(vec![
            make_input_entry("ls\r"),
            make_output_entry("file1.txt\n"),
        ]);
        let cmds = vec![make_cmd(0, "sess", Some("ls"))];
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader)
            .await
            .unwrap();
        assert!(result.contains("file1.txt"));
        assert!(!result.contains("ls\r"));
    }

    #[tokio::test]
    async fn test_build_context_empty() {
        let strategy = RecentCommands::new();
        let formatter = GroupedFormatter::new("sess", 1000);
        let reader = MockReader::empty();
        let result = crate::build_context(&strategy, &formatter, &[], &reader)
            .await
            .unwrap();
        assert_eq!(result, "");
    }
}
```

**Step: Verify**

Run: `cargo test -p omnish-context`
Expected: all tests pass (9 format_utils + tests in recent.rs)

**Step: Commit**

```
feat(context): add GroupedFormatter and InterleavedFormatter with session labels and relative time
```

---

### Task 4: Update SessionManager to use GroupedFormatter

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs`

**Step 1: Update imports**

Replace:
```rust
use omnish_context::recent::{RecentCommands, DefaultFormatter};
```
With:
```rust
use omnish_context::recent::{RecentCommands, GroupedFormatter};
```

**Step 2: Update get_session_context**

The formatter now needs `current_session_id` and `now_ms`:

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
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let formatter = GroupedFormatter::new(session_id, now_ms);
    omnish_context::build_context(&strategy, &formatter, &session.commands, &reader).await
}
```

**Step 3: Update get_all_sessions_context**

This method no longer needs manual session headers — the formatter handles them. It needs to collect commands from ALL sessions into one list, pass a current_session_id (use the first one or a designated one). Since `get_all_sessions_context` is called without a "current" session context, pick the first session as current:

```rust
pub async fn get_all_sessions_context(&self) -> Result<String> {
    let sessions = self.sessions.lock().await;
    let strategy = RecentCommands::new();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Collect all commands from all sessions
    let mut all_commands = Vec::new();
    let mut first_session_id = String::new();
    for (sid, session) in sessions.iter() {
        if first_session_id.is_empty() {
            first_session_id = sid.clone();
        }
        all_commands.extend(session.commands.clone());
    }

    if all_commands.is_empty() {
        return Ok(String::new());
    }

    let formatter = GroupedFormatter::new(&first_session_id, now_ms);

    // Build a reader that can read from any session's stream.bin
    // Since commands have session_id, we need to find the right stream path
    // For simplicity, create a MultiSessionReader
    let mut stream_paths: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    for (sid, session) in sessions.iter() {
        stream_paths.insert(sid.clone(), session.dir.join("stream.bin"));
    }
    let reader = MultiSessionReader { stream_paths, commands: &all_commands };

    omnish_context::build_context(&strategy, &formatter, &all_commands, &reader).await
}
```

Wait — the `StreamReader` doesn't know which session a read belongs to. The `build_context` orchestrator calls `reader.read_command_output(offset, length)` per command, but different commands may reference different stream.bin files.

**Solution:** Add a `MultiSessionReader` that maps `(offset, length)` back to the correct stream.bin by looking at which command is being read. A simpler approach: since `build_context` calls the reader for each command in order, and we know commands are from different sessions, we can make the reader accept `session_id` — but that would change the trait.

**Better approach:** Keep `get_all_sessions_context` building per-session contexts and joining them. The GroupedFormatter already handles session labeling within a single call, but for multi-session we need all commands in one call. So let's add a `CompositeStreamReader` that wraps multiple `FileStreamReader`s keyed by stream offset ranges. Actually, the simplest approach: since each `CommandRecord` has unique `(stream_offset, stream_length)` pairs per session, and different sessions have different stream.bin files, we need the reader to know which file to read from.

**Simplest fix:** Change the `StreamReader` trait to accept a session_id parameter, or build a reader that maps command offsets to files. Since changing the trait is cleaner:

Actually, let's not change the trait. Instead, make `get_all_sessions_context` pass a current_session_id parameter and use a different approach — build context per-session then let the formatter handle multi-session display. But GroupedFormatter is designed to handle multiple sessions in one call.

**Final approach for Task 4:** Add a `session_id` parameter to `get_all_sessions_context`. Modify the `StreamReader` trait signature is too disruptive. Instead, create a `MultiSessionReader` that maps `(stream_offset, stream_length)` to the right file by pre-computing which commands belong to which session:

```rust
struct MultiSessionReader {
    readers: HashMap<(u64, u64), PathBuf>,
}

impl StreamReader for MultiSessionReader {
    fn read_command_output(&self, offset: u64, length: u64) -> Result<Vec<StreamEntry>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let path = self.readers.get(&(offset, length))
            .ok_or_else(|| anyhow!("no stream file for offset={}, length={}", offset, length))?;
        read_range(path, offset, length)
    }
}
```

Build this reader by iterating all commands and mapping their `(stream_offset, stream_length)` to the session's stream.bin path.

**Updated get_all_sessions_context:**

```rust
pub async fn get_all_sessions_context(&self, current_session_id: &str) -> Result<String> {
    let sessions = self.sessions.lock().await;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let mut all_commands = Vec::new();
    let mut offset_to_path: HashMap<(u64, u64), PathBuf> = HashMap::new();

    for (_sid, session) in sessions.iter() {
        let stream_path = session.dir.join("stream.bin");
        for cmd in &session.commands {
            offset_to_path.insert(
                (cmd.stream_offset, cmd.stream_length),
                stream_path.clone(),
            );
        }
        all_commands.extend(session.commands.clone());
    }

    if all_commands.is_empty() {
        return Ok(String::new());
    }

    let reader = MultiSessionReader { readers: offset_to_path };
    let strategy = RecentCommands::new();
    let formatter = GroupedFormatter::new(current_session_id, now_ms);
    omnish_context::build_context(&strategy, &formatter, &all_commands, &reader).await
}
```

This changes the signature of `get_all_sessions_context` to take `current_session_id`. Check the caller in `server.rs` and update it.

**Step 4: Update server.rs caller**

In `crates/omnish-daemon/src/server.rs`, the `handle_llm_request` function calls `mgr.get_all_sessions_context().await?`. Change to pass the request's session_id:

```rust
RequestScope::AllSessions => {
    mgr.get_all_sessions_context(&req.session_id).await?
}
```

**Step 5: Verify**

Run: `cargo test --workspace`
Expected: all tests pass

**Step 6: Commit**

```
refactor(daemon): use GroupedFormatter with session labels and relative time
```
