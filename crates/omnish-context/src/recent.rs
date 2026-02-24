use async_trait::async_trait;
use omnish_store::command::CommandRecord;

use crate::format_utils::{assign_term_labels, format_relative_time, truncate_lines};
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
        // Filter out empty commands (Enter with no input) so they don't consume slots
        let meaningful: Vec<_> = commands.iter()
            .filter(|c| c.command_line.is_some())
            .collect();
        if meaningful.len() > self.max {
            meaningful[meaningful.len() - self.max..].to_vec()
        } else {
            meaningful
        }
    }
}

/// Formats commands grouped by session, with the current session last.
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

        // Collect session IDs in first-appearance order
        let mut session_order: Vec<String> = Vec::new();
        for cmd in commands {
            if !session_order.contains(&cmd.session_id) {
                session_order.push(cmd.session_id.clone());
            }
        }

        // Move current session to end so it appears last (closest to the LLM prompt)
        if let Some(pos) = session_order
            .iter()
            .position(|s| s == &self.current_session_id)
        {
            let current = session_order.remove(pos);
            session_order.push(current);
        }

        let mut sections = Vec::new();
        for session_id in &session_order {
            let label = labels.get(session_id).unwrap();
            let is_current = session_id == &self.current_session_id;
            let header = if is_current {
                format!("--- {} (current) ---", label)
            } else {
                format!("--- {} ---", label)
            };

            let mut group_lines = vec![header];
            for cmd in commands.iter().filter(|c| &c.session_id == session_id) {
                let time_str = format_relative_time(cmd.started_at, self.now_ms);
                let cmd_line = cmd.command_line.as_deref().unwrap_or("(unknown)");
                let output = truncate_lines(&cmd.output, MAX_OUTPUT_LINES, HEAD_LINES, TAIL_LINES);

                let failed_tag = match cmd.exit_code {
                    Some(code) if code != 0 => format!("  [FAILED: {}]", code),
                    _ => String::new(),
                };
                if output.is_empty() {
                    group_lines.push(format!("[{}] $ {}{}", time_str, cmd_line, failed_tag));
                } else {
                    group_lines.push(format!("[{}] $ {}{}\n{}", time_str, cmd_line, failed_tag, output));
                }
            }

            sections.push(group_lines.join("\n\n"));
        }

        sections.join("\n\n")
    }
}

/// Formats commands interleaved by time, sorted by started_at.
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
            let time_str = format_relative_time(cmd.started_at, self.now_ms);
            let label = labels.get(&cmd.session_id).unwrap();
            let is_current = cmd.session_id == self.current_session_id;
            let label_str = if is_current {
                format!("{}*", label)
            } else {
                label.clone()
            };
            let cmd_line = cmd.command_line.as_deref().unwrap_or("(unknown)");
            let output = truncate_lines(&cmd.output, MAX_OUTPUT_LINES, HEAD_LINES, TAIL_LINES);

            let failed_tag = match cmd.exit_code {
                Some(code) if code != 0 => format!("  [FAILED: {}]", code),
                _ => String::new(),
            };
            if output.is_empty() {
                sections.push(format!("[{}] {} $ {}{}", time_str, label_str, cmd_line, failed_tag));
            } else {
                sections.push(format!("[{}] {} $ {}{}\n{}", time_str, label_str, cmd_line, failed_tag, output));
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
            session_id: session_id.to_string(),
            command_line: cmd_line.map(|s| s.to_string()),
            cwd: None,
            started_at: 1000 + seq as u64 * 100,
            ended_at: Some(1000 + seq as u64 * 100 + 50),
            output_summary: String::new(),
            stream_offset: 0,
            stream_length: 100,
            exit_code: None,
        }
    }

    fn make_ctx(session_id: &str, cmd_line: &str, started_at: u64, output: &str) -> CommandContext {
        CommandContext {
            session_id: session_id.to_string(),
            command_line: Some(cmd_line.to_string()),
            cwd: None,
            started_at,
            ended_at: Some(started_at + 50),
            output: output.to_string(),
            exit_code: None,
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
        let commands = vec![make_ctx("sess-a", "ls", 30000, "file1.txt")];
        let formatter = GroupedFormatter::new("sess-a", 60000);
        let result = formatter.format(&commands);
        assert!(result.contains("--- term A (current) ---"));
        assert!(result.contains("[30s ago] $ ls"));
    }

    #[test]
    fn test_grouped_multi_session() {
        let commands = vec![
            make_ctx("sess-a", "ls", 28000, "file1.txt"),
            make_ctx("sess-b", "npm start", 25000, "Server running"),
        ];
        let formatter = GroupedFormatter::new("sess-a", 30000);
        let result = formatter.format(&commands);
        // Current session should be last (closest to LLM prompt)
        let pos_a = result.find("--- term A (current) ---").unwrap();
        let pos_b = result.find("--- term B ---").unwrap();
        assert!(pos_b < pos_a);
        assert!(result.contains("term A"));
        assert!(result.contains("term B"));
    }

    #[test]
    fn test_grouped_empty() {
        let formatter = GroupedFormatter::new("sess-a", 30000);
        let result = formatter.format(&[]);
        assert_eq!(result, "");
    }

    // --- InterleavedFormatter tests ---

    #[test]
    fn test_interleaved_sorted_by_time() {
        let commands = vec![
            make_ctx("sess-a", "ls", 28000, "file1.txt"),
            make_ctx("sess-b", "npm start", 25000, "Server running"),
            make_ctx("sess-a", "pwd", 29970, "/home"),
        ];
        let formatter = InterleavedFormatter::new("sess-a", 30000);
        let result = formatter.format(&commands);
        // npm start (5s ago) should come before ls (2s ago) which should come before pwd (30s...wait)
        // started_at: npm=25000, ls=28000, pwd=29970; now=30000
        // npm: 5s ago, ls: 2s ago, pwd: 30s ago? No: 30000-29970=30ms = 0s ago
        let pos_npm = result.find("npm start").unwrap();
        let pos_ls = result.find("$ ls").unwrap();
        let pos_pwd = result.find("$ pwd").unwrap();
        assert!(pos_npm < pos_ls);
        assert!(pos_ls < pos_pwd);
    }

    #[test]
    fn test_interleaved_marks_current() {
        let commands = vec![
            make_ctx("sess-a", "ls", 28000, "file1.txt"),
            make_ctx("sess-b", "npm start", 25000, "Server running"),
        ];
        let formatter = InterleavedFormatter::new("sess-a", 30000);
        let result = formatter.format(&commands);
        assert!(result.contains("term A*"));
        assert!(result.contains("term B $"));
    }

    #[test]
    fn test_interleaved_empty() {
        let formatter = InterleavedFormatter::new("sess-a", 30000);
        let result = formatter.format(&[]);
        assert_eq!(result, "");
    }

    // --- Integration tests: build_context ---

    #[tokio::test]
    async fn test_build_context_grouped() {
        let strategy = RecentCommands::new();
        let formatter = GroupedFormatter::new("sess", 30000);
        let reader = MockReader::new(vec![make_output_entry("file1.txt\n")]);
        let cmds = vec![make_cmd(0, "sess", Some("ls"))];
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader)
            .await
            .unwrap();
        assert!(result.contains("--- term A (current) ---"));
        assert!(result.contains("$ ls"));
    }

    #[tokio::test]
    async fn test_build_context_interleaved() {
        let strategy = RecentCommands::new();
        let formatter = InterleavedFormatter::new("sess", 30000);
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
        let formatter = GroupedFormatter::new("sess", 30000);
        let reader = MockReader::new(vec![
            make_input_entry("ls\r"),
            make_output_entry("file1.txt\n"),
        ]);
        let cmds = vec![make_cmd(0, "sess", Some("ls"))];
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader)
            .await
            .unwrap();
        // Should only contain output, not input
        assert!(result.contains("file1.txt"));
        assert!(!result.contains("ls\r"));
    }

    #[tokio::test]
    async fn test_build_context_empty() {
        let strategy = RecentCommands::new();
        let formatter = GroupedFormatter::new("sess", 30000);
        let reader = MockReader::empty();
        let result = crate::build_context(&strategy, &formatter, &[], &reader)
            .await
            .unwrap();
        assert_eq!(result, "");
    }
}
