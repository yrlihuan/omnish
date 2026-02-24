use async_trait::async_trait;
use omnish_store::command::CommandRecord;

use crate::format_utils::{assign_term_labels, format_relative_time, truncate_lines};
use crate::{CommandContext, ContextFormatter, ContextStrategy};

/// Selects the most recent N commands.
pub struct RecentCommands {
    max: usize,
}

impl RecentCommands {
    pub fn new(max: usize) -> Self {
        Self { max }
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
    head_lines: usize,
    tail_lines: usize,
}

impl GroupedFormatter {
    pub fn new(current_session_id: &str, now_ms: u64, head_lines: usize, tail_lines: usize) -> Self {
        Self {
            current_session_id: current_session_id.to_string(),
            now_ms,
            head_lines,
            tail_lines,
        }
    }
}

impl ContextFormatter for GroupedFormatter {
    fn format(&self, history: &[CommandContext], detailed: &[CommandContext]) -> String {
        if history.is_empty() && detailed.is_empty() {
            return String::new();
        }

        let mut sections = Vec::new();

        // History section: command-line only
        if !history.is_empty() {
            let mut history_lines = vec!["--- History ---".to_string()];
            for cmd in history {
                let cmd_line = cmd.command_line.as_deref().unwrap_or("(unknown)");
                history_lines.push(format!("$ {}", cmd_line));
            }
            sections.push(history_lines.join("\n"));
        }

        // Detailed section: grouped by session with full output
        if !detailed.is_empty() {
            let labels = assign_term_labels(detailed, &self.current_session_id);

            let mut session_order: Vec<String> = Vec::new();
            for cmd in detailed {
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

            for session_id in &session_order {
                let label = labels.get(session_id).unwrap();
                let is_current = session_id == &self.current_session_id;
                let header = if is_current {
                    format!("--- {} [current] ---", label)
                } else {
                    format!("--- {} ---", label)
                };

                let mut group_lines = vec![header];
                for cmd in detailed.iter().filter(|c| &c.session_id == session_id) {
                    let time_str = format_relative_time(cmd.started_at, self.now_ms);
                    let cmd_line = cmd.command_line.as_deref().unwrap_or("(unknown)");
                    let max_lines = self.head_lines + self.tail_lines;
                    let output = truncate_lines(&cmd.output, max_lines, self.head_lines, self.tail_lines);

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
        }

        sections.join("\n\n")
    }
}

/// Formats commands interleaved by time, sorted by started_at.
pub struct InterleavedFormatter {
    current_session_id: String,
    now_ms: u64,
    head_lines: usize,
    tail_lines: usize,
}

impl InterleavedFormatter {
    pub fn new(current_session_id: &str, now_ms: u64, head_lines: usize, tail_lines: usize) -> Self {
        Self {
            current_session_id: current_session_id.to_string(),
            now_ms,
            head_lines,
            tail_lines,
        }
    }
}

impl ContextFormatter for InterleavedFormatter {
    fn format(&self, history: &[CommandContext], detailed: &[CommandContext]) -> String {
        if history.is_empty() && detailed.is_empty() {
            return String::new();
        }

        let mut sections = Vec::new();

        // History section: command-line only
        if !history.is_empty() {
            let mut history_lines = vec!["--- History ---".to_string()];
            for cmd in history {
                let cmd_line = cmd.command_line.as_deref().unwrap_or("(unknown)");
                history_lines.push(format!("$ {}", cmd_line));
            }
            sections.push(history_lines.join("\n"));
        }

        // Detailed section: interleaved by time
        if !detailed.is_empty() {
            let labels = assign_term_labels(detailed, &self.current_session_id);

            let mut sorted: Vec<&CommandContext> = detailed.iter().collect();
            sorted.sort_by_key(|c| c.started_at);

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
                let max_lines = self.head_lines + self.tail_lines;
                let output = truncate_lines(&cmd.output, max_lines, self.head_lines, self.tail_lines);

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
        make_ctx_with_host(session_id, cmd_line, started_at, output, None)
    }

    fn make_ctx_with_host(session_id: &str, cmd_line: &str, started_at: u64, output: &str, hostname: Option<&str>) -> CommandContext {
        CommandContext {
            session_id: session_id.to_string(),
            hostname: hostname.map(|s| s.to_string()),
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
        let strategy = RecentCommands::new(10);
        let result = strategy.select_commands(&[]).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_select_max_recent() {
        let strategy = RecentCommands::new(10);
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
        let detailed = vec![make_ctx("sess-a", "ls", 30000, "file1.txt")];
        let formatter = GroupedFormatter::new("sess-a", 60000, 10, 10);
        let result = formatter.format(&[], &detailed);
        assert!(result.contains("--- term A [current] ---"));
        assert!(result.contains("[30s ago] $ ls"));
    }

    #[test]
    fn test_grouped_multi_session() {
        let detailed = vec![
            make_ctx("sess-a", "ls", 28000, "file1.txt"),
            make_ctx("sess-b", "npm start", 25000, "Server running"),
        ];
        let formatter = GroupedFormatter::new("sess-a", 30000, 10, 10);
        let result = formatter.format(&[], &detailed);
        // Current session should be last (closest to LLM prompt)
        let pos_a = result.find("--- term A [current] ---").unwrap();
        let pos_b = result.find("--- term B ---").unwrap();
        assert!(pos_b < pos_a);
        assert!(result.contains("term A"));
        assert!(result.contains("term B"));
    }

    #[test]
    fn test_grouped_with_hostname() {
        let detailed = vec![
            make_ctx_with_host("sess-a", "ls", 28000, "file1.txt", Some("workstation")),
            make_ctx_with_host("sess-b", "npm start", 25000, "Server running", Some("server01")),
        ];
        let formatter = GroupedFormatter::new("sess-a", 30000, 10, 10);
        let result = formatter.format(&[], &detailed);
        assert!(result.contains("--- workstation (term A) [current] ---"));
        assert!(result.contains("--- server01 (term B) ---"));
    }

    #[test]
    fn test_grouped_with_history() {
        let history = vec![
            make_ctx("sess-a", "cd /tmp", 10000, ""),
            make_ctx("sess-a", "mkdir foo", 11000, ""),
        ];
        let detailed = vec![make_ctx("sess-a", "ls", 30000, "file1.txt")];
        let formatter = GroupedFormatter::new("sess-a", 60000, 10, 10);
        let result = formatter.format(&history, &detailed);
        assert!(result.contains("--- History ---"));
        assert!(result.contains("$ cd /tmp"));
        assert!(result.contains("$ mkdir foo"));
        // History should appear before detailed
        let pos_history = result.find("--- History ---").unwrap();
        let pos_detailed = result.find("--- term A [current] ---").unwrap();
        assert!(pos_history < pos_detailed);
        // History section should not contain timestamps — extract just the history block
        let history_block = &result[..result.find("--- term A").unwrap()];
        assert!(!history_block.contains("ago]"));
    }

    #[test]
    fn test_grouped_empty() {
        let formatter = GroupedFormatter::new("sess-a", 30000, 10, 10);
        let result = formatter.format(&[], &[]);
        assert_eq!(result, "");
    }

    // --- InterleavedFormatter tests ---

    #[test]
    fn test_interleaved_sorted_by_time() {
        let detailed = vec![
            make_ctx("sess-a", "ls", 28000, "file1.txt"),
            make_ctx("sess-b", "npm start", 25000, "Server running"),
            make_ctx("sess-a", "pwd", 29970, "/home"),
        ];
        let formatter = InterleavedFormatter::new("sess-a", 30000, 10, 10);
        let result = formatter.format(&[], &detailed);
        let pos_npm = result.find("npm start").unwrap();
        let pos_ls = result.find("$ ls").unwrap();
        let pos_pwd = result.find("$ pwd").unwrap();
        assert!(pos_npm < pos_ls);
        assert!(pos_ls < pos_pwd);
    }

    #[test]
    fn test_interleaved_marks_current() {
        let detailed = vec![
            make_ctx("sess-a", "ls", 28000, "file1.txt"),
            make_ctx("sess-b", "npm start", 25000, "Server running"),
        ];
        let formatter = InterleavedFormatter::new("sess-a", 30000, 10, 10);
        let result = formatter.format(&[], &detailed);
        assert!(result.contains("term A*"));
        assert!(result.contains("term B $"));
    }

    #[test]
    fn test_interleaved_empty() {
        let formatter = InterleavedFormatter::new("sess-a", 30000, 10, 10);
        let result = formatter.format(&[], &[]);
        assert_eq!(result, "");
    }

    // --- Integration tests: build_context ---

    #[tokio::test]
    async fn test_build_context_grouped() {
        let strategy = RecentCommands::new(10);
        let formatter = GroupedFormatter::new("sess", 30000, 10, 10);
        // Mock: first line is command echo (stripped), second line is actual output
        let reader = MockReader::new(vec![make_output_entry("$ ls\nfile1.txt\n")]);
        let cmds = vec![make_cmd(0, "sess", Some("ls"))];
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader, &std::collections::HashMap::new(), 10)
            .await
            .unwrap();
        assert!(result.contains("--- term A [current] ---"));
        assert!(result.contains("$ ls"));
        assert!(result.contains("file1.txt"));
    }

    #[tokio::test]
    async fn test_build_context_interleaved() {
        let strategy = RecentCommands::new(10);
        let formatter = InterleavedFormatter::new("sess", 30000, 10, 10);
        let reader = MockReader::new(vec![make_output_entry("$ ls\nfile1.txt\n")]);
        let cmds = vec![make_cmd(0, "sess", Some("ls"))];
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader, &std::collections::HashMap::new(), 10)
            .await
            .unwrap();
        assert!(result.contains("term A*"));
        assert!(result.contains("$ ls"));
        assert!(result.contains("file1.txt"));
    }

    #[tokio::test]
    async fn test_build_context_filters_direction() {
        let strategy = RecentCommands::new(10);
        let formatter = GroupedFormatter::new("sess", 30000, 10, 10);
        let reader = MockReader::new(vec![
            make_input_entry("ls\r"),
            make_output_entry("$ ls\nfile1.txt\n"),
        ]);
        let cmds = vec![make_cmd(0, "sess", Some("ls"))];
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader, &std::collections::HashMap::new(), 10)
            .await
            .unwrap();
        assert!(result.contains("file1.txt"));
        assert!(!result.contains("ls\r"));
    }

    #[tokio::test]
    async fn test_build_context_strips_command_echo() {
        let strategy = RecentCommands::new(10);
        let formatter = GroupedFormatter::new("sess", 30000, 10, 10);
        // Simulates PTY output: prompt+command echo on first line, then actual output
        let reader = MockReader::new(vec![make_output_entry("user@host:~ $ echo hello\nhello\n")]);
        let cmds = vec![make_cmd(0, "sess", Some("echo hello"))];
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader, &std::collections::HashMap::new(), 10)
            .await
            .unwrap();
        assert!(result.contains("hello"));
        // The echoed command line should be stripped from output
        assert!(!result.contains("user@host"));
    }

    #[tokio::test]
    async fn test_build_context_empty() {
        let strategy = RecentCommands::new(10);
        let formatter = GroupedFormatter::new("sess", 30000, 10, 10);
        let reader = MockReader::empty();
        let result = crate::build_context(&strategy, &formatter, &[], &reader, &std::collections::HashMap::new(), 10)
            .await
            .unwrap();
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn test_build_context_splits_history_and_detailed() {
        let strategy = RecentCommands::new(20);  // select up to 20
        let formatter = GroupedFormatter::new("sess", 30000, 10, 10);
        let reader = MockReader::new(vec![make_output_entry("$ cmd\noutput\n")]);
        // 5 commands, detailed_count=2 -> 3 history + 2 detailed
        let cmds: Vec<_> = (0..5)
            .map(|i| make_cmd(i, "sess", Some(&format!("cmd{}", i))))
            .collect();
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader, &std::collections::HashMap::new(), 2)
            .await
            .unwrap();
        // History section should list first 3 commands
        assert!(result.contains("--- History ---"));
        assert!(result.contains("$ cmd0"));
        assert!(result.contains("$ cmd1"));
        assert!(result.contains("$ cmd2"));
        // Detailed section should have last 2
        assert!(result.contains("$ cmd3"));
        assert!(result.contains("$ cmd4"));
    }
}
