# Command Recording Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Record individual commands (segmented by shell prompt detection) as `CommandRecord` entries, stored per-session alongside the existing stream.bin.

**Architecture:** A `PromptDetector` state machine in `omnish-daemon` watches Output I/O chunks for shell prompt patterns. When a prompt is detected, the preceding I/O is finalized as a `CommandRecord` with metadata (command line, timestamps, output summary, stream offsets). Records are persisted to `commands.json` per session via `omnish-store`.

**Tech Stack:** Rust, serde/serde_json, existing omnish crate structure

---

### Task 1: CommandRecord struct and persistence in omnish-store

**Files:**
- Create: `crates/omnish-store/src/command.rs`
- Modify: `crates/omnish-store/src/lib.rs`
- Test: `crates/omnish-store/tests/store_test.rs`

**Step 1: Write the failing test**

Add to `crates/omnish-store/tests/store_test.rs`:

```rust
use omnish_store::command::CommandRecord;

#[test]
fn test_command_record_save_and_load() {
    let dir = tempdir().unwrap();
    let records = vec![
        CommandRecord {
            command_id: "sess1:0".into(),
            session_id: "sess1".into(),
            command_line: Some("cargo build".into()),
            cwd: Some("/home/user/project".into()),
            started_at: 1000,
            ended_at: Some(2000),
            output_summary: "Compiling omnish v0.1.0\nFinished dev".into(),
            stream_offset: 0,
            stream_length: 512,
        },
        CommandRecord {
            command_id: "sess1:1".into(),
            session_id: "sess1".into(),
            command_line: Some("cargo test".into()),
            cwd: Some("/home/user/project".into()),
            started_at: 2000,
            ended_at: Some(3000),
            output_summary: "running 5 tests\ntest result: ok".into(),
            stream_offset: 512,
            stream_length: 1024,
        },
    ];

    CommandRecord::save_all(&records, dir.path()).unwrap();
    let loaded = CommandRecord::load_all(dir.path()).unwrap();

    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].command_id, "sess1:0");
    assert_eq!(loaded[0].command_line.as_deref(), Some("cargo build"));
    assert_eq!(loaded[0].stream_offset, 0);
    assert_eq!(loaded[1].command_id, "sess1:1");
    assert_eq!(loaded[1].ended_at, Some(3000));
}

#[test]
fn test_command_record_load_empty() {
    let dir = tempdir().unwrap();
    let loaded = CommandRecord::load_all(dir.path()).unwrap();
    assert!(loaded.is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-store test_command_record`
Expected: FAIL — module `command` not found

**Step 3: Write minimal implementation**

Create `crates/omnish-store/src/command.rs`:

```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    pub command_id: String,
    pub session_id: String,
    pub command_line: Option<String>,
    pub cwd: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub output_summary: String,
    pub stream_offset: u64,
    pub stream_length: u64,
}

impl CommandRecord {
    pub fn save_all(records: &[CommandRecord], dir: &Path) -> Result<()> {
        let path = dir.join("commands.json");
        let json = serde_json::to_string_pretty(records)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load_all(dir: &Path) -> Result<Vec<CommandRecord>> {
        let path = dir.join("commands.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let json = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&json)?)
    }
}
```

Add to `crates/omnish-store/src/lib.rs`:

```rust
pub mod command;
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-store test_command_record`
Expected: PASS (2 tests)

**Step 5: Commit**

```bash
git add crates/omnish-store/src/command.rs crates/omnish-store/src/lib.rs crates/omnish-store/tests/store_test.rs
git commit -m "feat(store): add CommandRecord struct and persistence"
```

---

### Task 2: Add byte-position tracking to StreamWriter

The `CommandRecord` needs `stream_offset` and `stream_length` to locate full I/O in `stream.bin`. `StreamWriter` currently doesn't track its write position.

**Files:**
- Modify: `crates/omnish-store/src/stream.rs`
- Test: `crates/omnish-store/tests/store_test.rs`

**Step 1: Write the failing test**

Add to `crates/omnish-store/tests/store_test.rs`:

```rust
#[test]
fn test_stream_writer_position_tracking() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("stream.bin");

    let mut writer = StreamWriter::create(&path).unwrap();

    let pos0 = writer.position();
    assert_eq!(pos0, 0);

    writer.write_entry(1000, 0, b"ls\n").unwrap();  // 8+1+4+3 = 16 bytes
    let pos1 = writer.position();
    assert_eq!(pos1, 16);

    writer.write_entry(1001, 1, b"file.txt\n").unwrap();  // 8+1+4+9 = 22 bytes
    let pos2 = writer.position();
    assert_eq!(pos2, 38);  // 16 + 22
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-store test_stream_writer_position`
Expected: FAIL — no method `position` found

**Step 3: Write minimal implementation**

Modify `crates/omnish-store/src/stream.rs`, add a `pos` field to `StreamWriter`:

```rust
pub struct StreamWriter {
    writer: BufWriter<File>,
    pos: u64,
}
```

Update `create`:
```rust
pub fn create(path: &Path) -> Result<Self> {
    let file = File::create(path)?;
    Ok(Self {
        writer: BufWriter::new(file),
        pos: 0,
    })
}
```

Add `position` method and update `write_entry` to track `pos`:
```rust
pub fn position(&self) -> u64 {
    self.pos
}

pub fn write_entry(&mut self, timestamp_ms: u64, direction: u8, data: &[u8]) -> Result<()> {
    self.writer.write_all(&timestamp_ms.to_be_bytes())?;
    self.writer.write_all(&[direction])?;
    self.writer.write_all(&(data.len() as u32).to_be_bytes())?;
    self.writer.write_all(data)?;
    self.writer.flush()?;
    self.pos += 8 + 1 + 4 + data.len() as u64;
    Ok(())
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-store test_stream_writer_position`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/omnish-store/src/stream.rs crates/omnish-store/tests/store_test.rs
git commit -m "feat(store): add byte-position tracking to StreamWriter"
```

---

### Task 3: PromptDetector state machine

Detects shell prompt patterns in the Output stream to identify command boundaries. Pure state machine with no I/O — operates on byte chunks.

**Files:**
- Create: `crates/omnish-daemon/src/prompt_detector.rs`
- Modify: `crates/omnish-daemon/src/lib.rs`

**Step 1: Write the failing tests**

Add tests directly in `crates/omnish-daemon/src/prompt_detector.rs` (module-internal tests):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_dollar_prompt() {
        let mut detector = PromptDetector::new();
        // Simulate: command output, then prompt
        let events = detector.feed(b"total 0\r\nfile.txt\r\nuser@host:~$ ");
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], PromptEvent::PromptDetected { .. }));
    }

    #[test]
    fn test_detect_hash_prompt() {
        let mut detector = PromptDetector::new();
        let events = detector.feed(b"some output\r\nroot@host:/# ");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_no_prompt_in_partial_output() {
        let mut detector = PromptDetector::new();
        let events = detector.feed(b"compiling crate...\r\n");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_consecutive_prompts() {
        let mut detector = PromptDetector::new();
        // First prompt (session start)
        let events1 = detector.feed(b"user@host:~$ ");
        assert_eq!(events1.len(), 1);
        // Command output then second prompt
        let events2 = detector.feed(b"hello\r\nuser@host:~$ ");
        assert_eq!(events2.len(), 1);
    }

    #[test]
    fn test_prompt_split_across_chunks() {
        let mut detector = PromptDetector::new();
        // Prompt arrives in two chunks
        let events1 = detector.feed(b"output\r\nuser@ho");
        assert_eq!(events1.len(), 0);
        let events2 = detector.feed(b"st:~$ ");
        assert_eq!(events2.len(), 1);
    }

    #[test]
    fn test_ansi_stripped_before_matching() {
        let mut detector = PromptDetector::new();
        // Prompt with ANSI color codes (common in zsh/bash)
        let events = detector.feed(b"output\r\n\x1b[32muser@host\x1b[0m:\x1b[34m~\x1b[0m$ ");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_dollar_in_output_not_false_positive() {
        let mut detector = PromptDetector::new();
        // "$" in middle of line is not a prompt
        let events = detector.feed(b"price is $100\r\n");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_custom_pattern() {
        let mut detector = PromptDetector::with_patterns(vec![
            r"❯\s*$".to_string(),
        ]);
        let events = detector.feed(b"output\r\n❯ ");
        assert_eq!(events.len(), 1);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon prompt_detector`
Expected: FAIL — module not found

**Step 3: Write minimal implementation**

Create `crates/omnish-daemon/src/prompt_detector.rs`:

```rust
use regex::Regex;

/// Event emitted when a shell prompt is detected in the output stream.
#[derive(Debug, Clone)]
pub struct PromptEvent {
    /// Byte offset within the chunk where the prompt line starts.
    pub line_start_offset: usize,
}

/// Detects shell prompt patterns in terminal output.
///
/// Accumulates a line buffer across `feed()` calls. When a complete line
/// (terminated by `\n`) or a trailing partial line matches a prompt pattern,
/// emits a `PromptEvent`.
pub struct PromptDetector {
    patterns: Vec<Regex>,
    line_buf: Vec<u8>,
}

/// Default prompt patterns:
/// - `$ ` at end of line (bash default)
/// - `# ` at end of line (root prompt)
/// - `% ` at end of line (zsh default)
/// - `❯ ` at end of line (starship/custom)
const DEFAULT_PATTERNS: &[&str] = &[
    r"[\$#%❯]\s*$",
];

impl PromptDetector {
    pub fn new() -> Self {
        Self::with_patterns(DEFAULT_PATTERNS.iter().map(|s| s.to_string()).collect())
    }

    pub fn with_patterns(patterns: Vec<String>) -> Self {
        let compiled = patterns
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();
        Self {
            patterns: compiled,
            line_buf: Vec::new(),
        }
    }

    /// Feed a chunk of output bytes. Returns prompt events detected.
    pub fn feed(&mut self, data: &[u8]) -> Vec<PromptEvent> {
        let mut events = Vec::new();
        let mut offset = 0;

        for (i, &byte) in data.iter().enumerate() {
            self.line_buf.push(byte);

            if byte == b'\n' {
                // Complete line — check for prompt, then clear buffer
                // (Prompts rarely end with \n, but clear the buffer regardless)
                self.line_buf.clear();
                offset = i + 1;
            }
        }

        // Check trailing partial line (prompts typically don't end with \n)
        if !self.line_buf.is_empty() && self.is_prompt() {
            events.push(PromptEvent {
                line_start_offset: offset,
            });
            self.line_buf.clear();
        }

        events
    }

    /// Strip ANSI escape sequences and check if the line buffer matches
    /// any prompt pattern.
    fn is_prompt(&self) -> bool {
        let stripped = strip_ansi(&self.line_buf);
        let text = String::from_utf8_lossy(&stripped);
        // Require at least a few chars to avoid false positives on bare "$"
        if text.trim().len() < 2 {
            return false;
        }
        self.patterns.iter().any(|re| re.is_match(&text))
    }
}

/// Strip ANSI CSI escape sequences (\x1b[...X) from bytes.
fn strip_ansi(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == b'[' {
            // Skip CSI sequence: \x1b[ params final_byte
            i += 2;
            while i < data.len() && !data[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < data.len() {
                i += 1; // skip final byte
            }
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}
```

Add to `crates/omnish-daemon/src/lib.rs`:

```rust
pub mod prompt_detector;
```

Add `regex` dependency to `crates/omnish-daemon/Cargo.toml`:

```toml
regex = "1"
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-daemon prompt_detector`
Expected: PASS (8 tests)

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/prompt_detector.rs crates/omnish-daemon/src/lib.rs crates/omnish-daemon/Cargo.toml
git commit -m "feat(daemon): add PromptDetector state machine for command segmentation"
```

---

### Task 4: CommandTracker — builds CommandRecords from I/O stream

Sits between `SessionManager.write_io()` and `StreamWriter`. Uses `PromptDetector` to know when commands start/end, accumulates metadata, produces `CommandRecord` entries.

**Files:**
- Create: `crates/omnish-daemon/src/command_tracker.rs`
- Modify: `crates/omnish-daemon/src/lib.rs`

**Step 1: Write the failing tests**

Tests in `crates/omnish-daemon/src/command_tracker.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_tracker() -> CommandTracker {
        CommandTracker::new("sess1".into(), None)
    }

    #[test]
    fn test_first_prompt_starts_tracking() {
        let mut tracker = make_tracker();
        // Initial prompt — no command yet, just starts tracking
        let cmds = tracker.feed_output(b"user@host:~$ ", 1000, 0);
        assert!(cmds.is_empty(), "first prompt should not produce a command");
        assert!(tracker.tracking(), "should be tracking after first prompt");
    }

    #[test]
    fn test_simple_command_recorded() {
        let mut tracker = make_tracker();
        // Initial prompt
        tracker.feed_output(b"user@host:~$ ", 1000, 0);
        // User input
        tracker.feed_input(b"ls -la\r\n", 1001);
        // Command output + next prompt
        let cmds = tracker.feed_output(
            b"total 0\r\nfile.txt\r\nuser@host:~$ ",
            1002,
            100,
        );
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].command_line.as_deref(), Some("ls -la"));
        assert_eq!(cmds[0].started_at, 1000);
        assert_eq!(cmds[0].ended_at, Some(1002));
    }

    #[test]
    fn test_output_summary_head_tail() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"user@host:~$ ", 1000, 0);
        tracker.feed_input(b"long-cmd\r\n", 1001);

        // Generate 20 lines of output
        let mut output = String::new();
        for i in 0..20 {
            output.push_str(&format!("line {}\r\n", i));
        }
        output.push_str("user@host:~$ ");

        let cmds = tracker.feed_output(output.as_bytes(), 1002, 100);
        assert_eq!(cmds.len(), 1);
        let summary = &cmds[0].output_summary;
        // Should contain first and last lines
        assert!(summary.contains("line 0"), "summary should contain head");
        assert!(summary.contains("line 19"), "summary should contain tail");
    }

    #[test]
    fn test_command_id_sequential() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"$ ", 1000, 0);

        tracker.feed_input(b"cmd1\r\n", 1001);
        let cmds1 = tracker.feed_output(b"out1\r\n$ ", 1002, 50);
        assert_eq!(cmds1[0].command_id, "sess1:0");

        tracker.feed_input(b"cmd2\r\n", 1003);
        let cmds2 = tracker.feed_output(b"out2\r\n$ ", 1004, 100);
        assert_eq!(cmds2[0].command_id, "sess1:1");
    }

    #[test]
    fn test_no_command_without_prompt() {
        let mut tracker = make_tracker();
        // Output without any prompt
        let cmds = tracker.feed_output(b"some random output\r\n", 1000, 0);
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_cwd_from_session() {
        let mut tracker = CommandTracker::new(
            "sess1".into(),
            Some("/home/user/project".into()),
        );
        tracker.feed_output(b"$ ", 1000, 0);
        tracker.feed_input(b"make\r\n", 1001);
        let cmds = tracker.feed_output(b"done\r\n$ ", 1002, 100);
        assert_eq!(cmds[0].cwd.as_deref(), Some("/home/user/project"));
    }

    #[test]
    fn test_stream_offsets_recorded() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"$ ", 1000, 50);  // stream at byte 50
        tracker.feed_input(b"ls\r\n", 1001);
        let cmds = tracker.feed_output(b"out\r\n$ ", 1002, 200);  // stream at byte 200
        assert_eq!(cmds[0].stream_offset, 50);
        assert_eq!(cmds[0].stream_length, 200 - 50);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon command_tracker`
Expected: FAIL — module not found

**Step 3: Write minimal implementation**

Create `crates/omnish-daemon/src/command_tracker.rs`:

```rust
use crate::prompt_detector::{PromptDetector, PromptEvent};
use omnish_store::command::CommandRecord;

const SUMMARY_HEAD_LINES: usize = 5;
const SUMMARY_TAIL_LINES: usize = 5;

/// In-progress command being accumulated.
struct PendingCommand {
    seq: u32,
    started_at: u64,
    stream_offset: u64,
    input_buf: Vec<u8>,
    output_lines: Vec<String>,
}

/// Tracks command boundaries within a single session's I/O stream.
/// Uses `PromptDetector` to find prompt lines, then packages the
/// preceding I/O as a `CommandRecord`.
pub struct CommandTracker {
    session_id: String,
    cwd: Option<String>,
    detector: PromptDetector,
    pending: Option<PendingCommand>,
    next_seq: u32,
    seen_first_prompt: bool,
}

impl CommandTracker {
    pub fn new(session_id: String, cwd: Option<String>) -> Self {
        Self {
            session_id,
            cwd,
            detector: PromptDetector::new(),
            pending: None,
            next_seq: 0,
            seen_first_prompt: false,
        }
    }

    /// Whether we've seen at least one prompt and are actively tracking.
    pub fn tracking(&self) -> bool {
        self.seen_first_prompt
    }

    /// Feed user input bytes. Accumulates into pending command's input buffer.
    pub fn feed_input(&mut self, data: &[u8], _timestamp_ms: u64) {
        if let Some(ref mut pending) = self.pending {
            pending.input_buf.extend_from_slice(data);
        }
    }

    /// Feed shell output bytes. Detects prompts and finalizes commands.
    /// `stream_pos` is the current byte offset in stream.bin BEFORE this
    /// chunk was written.
    ///
    /// Returns any completed `CommandRecord`s.
    pub fn feed_output(
        &mut self,
        data: &[u8],
        timestamp_ms: u64,
        stream_pos: u64,
    ) -> Vec<CommandRecord> {
        // Accumulate output lines for pending command
        if let Some(ref mut pending) = self.pending {
            let text = String::from_utf8_lossy(data);
            for line in text.split('\n') {
                let line = line.trim_end_matches('\r');
                if !line.is_empty() {
                    pending.output_lines.push(line.to_string());
                }
            }
        }

        let events = self.detector.feed(data);
        let mut completed = Vec::new();

        for _event in events {
            if !self.seen_first_prompt {
                // First prompt — start tracking, no command to finalize
                self.seen_first_prompt = true;
                self.pending = Some(PendingCommand {
                    seq: self.next_seq,
                    started_at: timestamp_ms,
                    stream_offset: stream_pos,
                    input_buf: Vec::new(),
                    output_lines: Vec::new(),
                });
                continue;
            }

            // Finalize the pending command
            if let Some(pending) = self.pending.take() {
                let command_line = extract_command_line(&pending.input_buf);
                let output_summary = make_summary(&pending.output_lines);
                let stream_length = stream_pos - pending.stream_offset;

                completed.push(CommandRecord {
                    command_id: format!("{}:{}", self.session_id, pending.seq),
                    session_id: self.session_id.clone(),
                    command_line,
                    cwd: self.cwd.clone(),
                    started_at: pending.started_at,
                    ended_at: Some(timestamp_ms),
                    output_summary,
                    stream_offset: pending.stream_offset,
                    stream_length,
                });
                self.next_seq += 1;
            }

            // Start tracking next command
            self.pending = Some(PendingCommand {
                seq: self.next_seq,
                started_at: timestamp_ms,
                stream_offset: stream_pos,
                input_buf: Vec::new(),
                output_lines: Vec::new(),
            });
        }

        completed
    }
}

/// Extract the command line text from raw input bytes.
/// Strips trailing \r\n and takes only the first line.
fn extract_command_line(input: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(input);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Take first line only (in case of multi-line input)
    Some(trimmed.lines().next().unwrap_or("").to_string())
}

/// Build a head+tail summary from output lines.
fn make_summary(lines: &[String]) -> String {
    if lines.len() <= SUMMARY_HEAD_LINES + SUMMARY_TAIL_LINES {
        return lines.join("\n");
    }
    let head: Vec<&str> = lines[..SUMMARY_HEAD_LINES].iter().map(|s| s.as_str()).collect();
    let tail: Vec<&str> = lines[lines.len() - SUMMARY_TAIL_LINES..]
        .iter()
        .map(|s| s.as_str())
        .collect();
    format!(
        "{}\n... ({} lines omitted) ...\n{}",
        head.join("\n"),
        lines.len() - SUMMARY_HEAD_LINES - SUMMARY_TAIL_LINES,
        tail.join("\n"),
    )
}
```

Add to `crates/omnish-daemon/src/lib.rs`:

```rust
pub mod command_tracker;
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-daemon command_tracker`
Expected: PASS (7 tests)

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/command_tracker.rs crates/omnish-daemon/src/lib.rs
git commit -m "feat(daemon): add CommandTracker for building CommandRecords from I/O"
```

---

### Task 5: Integrate CommandTracker into SessionManager

Wire `CommandTracker` into `ActiveSession` so commands are recorded as I/O flows through `write_io()`. Persist `commands.json` when commands complete and when sessions end.

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs`
- Test: `crates/omnish-daemon/tests/daemon_test.rs`

**Step 1: Write the failing test**

Add to `crates/omnish-daemon/tests/daemon_test.rs`:

```rust
#[tokio::test]
async fn test_command_recording_through_session_manager() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("sess1", HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("cwd".to_string(), "/home/user".to_string()),
    ])).await.unwrap();

    // Simulate: prompt → input → output → prompt
    mgr.write_io("sess1", 1000, 1, b"user@host:~$ ").await.unwrap();
    mgr.write_io("sess1", 1001, 0, b"ls -la\r\n").await.unwrap();
    mgr.write_io("sess1", 1002, 1, b"total 0\r\nfile.txt\r\nuser@host:~$ ").await.unwrap();

    let commands = mgr.get_commands("sess1").await.unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].command_line.as_deref(), Some("ls -la"));
    assert_eq!(commands[0].session_id, "sess1");
    assert_eq!(commands[0].cwd.as_deref(), Some("/home/user"));
}

#[tokio::test]
async fn test_commands_persisted_on_session_end() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("sess1", HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("cwd".to_string(), "/tmp".to_string()),
    ])).await.unwrap();

    mgr.write_io("sess1", 1000, 1, b"$ ").await.unwrap();
    mgr.write_io("sess1", 1001, 0, b"echo hi\r\n").await.unwrap();
    mgr.write_io("sess1", 1002, 1, b"hi\r\n$ ").await.unwrap();

    mgr.end_session("sess1").await.unwrap();

    // After session ends, commands.json should exist on disk
    // Find session directory
    let mut session_dirs: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(session_dirs.len(), 1);

    let session_dir = session_dirs.remove(0).path();
    let commands = omnish_store::command::CommandRecord::load_all(&session_dir).unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].command_line.as_deref(), Some("echo hi"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon test_command_recording test_commands_persisted`
Expected: FAIL — no method `get_commands` on `SessionManager`

**Step 3: Write implementation**

Modify `crates/omnish-daemon/src/session_mgr.rs`:

Add imports at top:
```rust
use crate::command_tracker::CommandTracker;
use omnish_store::command::CommandRecord;
```

Add `CommandTracker` to `ActiveSession`:
```rust
struct ActiveSession {
    meta: SessionMeta,
    stream_writer: StreamWriter,
    command_tracker: CommandTracker,
    commands: Vec<CommandRecord>,
    dir: PathBuf,
}
```

In `register()`, after creating `stream_writer`, create the tracker:
```rust
let cwd = attrs.get("cwd").cloned();
let command_tracker = CommandTracker::new(session_id.to_string(), cwd);
```

And add `command_tracker` and `commands: Vec::new()` to the `ActiveSession` struct literal.

In `write_io()`, after `stream_writer.write_entry()`, feed the tracker:
```rust
pub async fn write_io(
    &self,
    session_id: &str,
    timestamp_ms: u64,
    direction: u8,
    data: &[u8],
) -> Result<()> {
    let mut sessions = self.sessions.lock().await;
    if let Some(session) = sessions.get_mut(session_id) {
        let pos_before = session.stream_writer.position();
        session.stream_writer.write_entry(timestamp_ms, direction, data)?;

        if direction == 1 {
            // Output from shell — feed to command tracker
            let completed = session.command_tracker.feed_output(data, timestamp_ms, pos_before);
            if !completed.is_empty() {
                session.commands.extend(completed);
                CommandRecord::save_all(&session.commands, &session.dir)?;
            }
        } else {
            // Input from user
            session.command_tracker.feed_input(data, timestamp_ms);
        }
    }
    Ok(())
}
```

In `end_session()`, persist commands before removing:
```rust
pub async fn end_session(&self, session_id: &str) -> Result<()> {
    let mut sessions = self.sessions.lock().await;
    if let Some(mut session) = sessions.remove(session_id) {
        session.meta.ended_at = Some(chrono::Utc::now().to_rfc3339());
        session.meta.save(&session.dir)?;
        CommandRecord::save_all(&session.commands, &session.dir)?;
    }
    Ok(())
}
```

Add `get_commands()` method:
```rust
pub async fn get_commands(&self, session_id: &str) -> Result<Vec<CommandRecord>> {
    let sessions = self.sessions.lock().await;
    if let Some(session) = sessions.get(session_id) {
        Ok(session.commands.clone())
    } else {
        Ok(Vec::new())
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-daemon`
Expected: PASS (all existing + 2 new tests)

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/session_mgr.rs crates/omnish-daemon/tests/daemon_test.rs
git commit -m "feat(daemon): integrate CommandTracker into SessionManager"
```

---

### Task 6: Read command output by offset from stream.bin

Add a function to read a specific byte range from `stream.bin`, needed for the future `get_commands` tool call that fetches full command output.

**Files:**
- Modify: `crates/omnish-store/src/stream.rs`
- Test: `crates/omnish-store/tests/store_test.rs`

**Step 1: Write the failing test**

Add to `crates/omnish-store/tests/store_test.rs`:

```rust
use omnish_store::stream::read_range;

#[test]
fn test_read_range_from_stream() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("stream.bin");

    let mut writer = StreamWriter::create(&path).unwrap();
    let pos0 = writer.position();
    writer.write_entry(1000, 0, b"ls\n").unwrap();
    let pos1 = writer.position();
    writer.write_entry(1001, 1, b"file.txt\n").unwrap();
    let pos2 = writer.position();

    // Read only the second entry's range
    let entries = read_range(&path, pos1, pos2 - pos1).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].data, b"file.txt\n");
    assert_eq!(entries[0].direction, 1);

    // Read both entries
    let all = read_range(&path, pos0, pos2 - pos0).unwrap();
    assert_eq!(all.len(), 2);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-store test_read_range`
Expected: FAIL — `read_range` not found

**Step 3: Write minimal implementation**

Add to `crates/omnish-store/src/stream.rs`:

```rust
use std::io::Seek;

pub fn read_range(path: &Path, offset: u64, length: u64) -> Result<Vec<StreamEntry>> {
    let mut file = File::open(path)?;
    file.seek(std::io::SeekFrom::Start(offset))?;
    let mut data = vec![0u8; length as usize];
    file.read_exact(&mut data)?;

    let mut entries = Vec::new();
    let mut pos = 0;
    while pos + 13 <= data.len() {
        let timestamp_ms = u64::from_be_bytes(data[pos..pos + 8].try_into()?);
        let direction = data[pos + 8];
        let data_len = u32::from_be_bytes(data[pos + 9..pos + 13].try_into()?) as usize;
        if pos + 13 + data_len > data.len() {
            break;
        }
        let entry_data = data[pos + 13..pos + 13 + data_len].to_vec();
        entries.push(StreamEntry {
            timestamp_ms,
            direction,
            data: entry_data,
        });
        pos += 13 + data_len;
    }
    Ok(entries)
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-store test_read_range`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/omnish-store/src/stream.rs crates/omnish-store/tests/store_test.rs
git commit -m "feat(store): add read_range for offset-based stream access"
```

---

### Task 7: Full integration test — end-to-end command recording

A comprehensive test simulating a realistic multi-command session.

**Files:**
- Test: `crates/omnish-daemon/tests/daemon_test.rs`

**Step 1: Write the test**

```rust
#[tokio::test]
async fn test_multi_command_session_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("e2e", HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("cwd".to_string(), "/home/user/project".to_string()),
    ])).await.unwrap();

    // Command 1: initial prompt + ls
    mgr.write_io("e2e", 1000, 1, b"user@host:~/project$ ").await.unwrap();
    mgr.write_io("e2e", 1001, 0, b"ls\r\n").await.unwrap();
    mgr.write_io("e2e", 1002, 1, b"Cargo.toml\r\nsrc/\r\nuser@host:~/project$ ").await.unwrap();

    // Command 2: cargo build
    mgr.write_io("e2e", 1003, 0, b"cargo build\r\n").await.unwrap();
    mgr.write_io("e2e", 1004, 1, b"   Compiling omnish v0.1.0\r\n    Finished dev\r\nuser@host:~/project$ ").await.unwrap();

    // Command 3: cargo test (still running — no closing prompt)
    mgr.write_io("e2e", 1005, 0, b"cargo test\r\n").await.unwrap();
    mgr.write_io("e2e", 1006, 1, b"running 5 tests\r\n").await.unwrap();

    let commands = mgr.get_commands("e2e").await.unwrap();

    // Only 2 completed commands (command 3 is still running)
    assert_eq!(commands.len(), 2);

    assert_eq!(commands[0].command_id, "e2e:0");
    assert_eq!(commands[0].command_line.as_deref(), Some("ls"));

    assert_eq!(commands[1].command_id, "e2e:1");
    assert_eq!(commands[1].command_line.as_deref(), Some("cargo build"));
    assert!(commands[1].output_summary.contains("Compiling"));

    // End session — should persist including any pending
    mgr.end_session("e2e").await.unwrap();
}
```

**Step 2: Run test**

Run: `cargo test -p omnish-daemon test_multi_command_session_e2e`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/omnish-daemon/tests/daemon_test.rs
git commit -m "test(daemon): add end-to-end multi-command session test"
```
