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

    // --- Integration tests: build_context orchestrator ---

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
