use anyhow::Result;
use async_trait::async_trait;
use omnish_store::command::CommandRecord;

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
    use omnish_store::stream::StreamEntry;

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
