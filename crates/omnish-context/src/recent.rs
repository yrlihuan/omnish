use async_trait::async_trait;
use omnish_store::command::CommandRecord;

use crate::format_utils::{assign_term_labels, truncate_lines};
use crate::{CommandContext, ContextFormatter, ContextStrategy};

fn format_command_prefix(hostname: &Option<String>, cwd: &Option<String>) -> String {
    match (hostname, cwd) {
        (Some(host), Some(cwd)) => format!("{}:{}", host, cwd),
        (Some(host), None) => host.clone(),
        (None, Some(cwd)) => cwd.clone(),
        (None, None) => String::new(),
    }
}

/// Selects the most recent N commands.
pub struct RecentCommands {
    max: usize,
    current_session_id: Option<String>,
    min_current_session_commands: usize,
}

impl RecentCommands {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            current_session_id: None,
            min_current_session_commands: 0,
        }
    }

    pub fn with_current_session(mut self, session_id: &str, min_commands: usize) -> Self {
        self.current_session_id = Some(session_id.to_string());
        self.min_current_session_commands = min_commands;
        self
    }
}

#[async_trait]
impl ContextStrategy for RecentCommands {
    async fn select_commands<'a>(&self, commands: &'a [CommandRecord]) -> Vec<&'a CommandRecord> {
        // Filter out empty commands (Enter with no input) so they don't consume slots
        let meaningful: Vec<_> = commands.iter()
            .filter(|c| c.command_line.is_some())
            .collect();

        if meaningful.is_empty() {
            return Vec::new();
        }

        // If no current session or minimum is 0, use simple recent selection
        if self.current_session_id.is_none() || self.min_current_session_commands == 0 {
            if meaningful.len() > self.max {
                return meaningful[meaningful.len() - self.max..].to_vec();
            } else {
                return meaningful;
            }
        }

        let current_session_id = self.current_session_id.as_ref().unwrap();

        // Separate commands by session
        let mut current_session_commands: Vec<&CommandRecord> = Vec::new();
        let mut other_commands: Vec<&CommandRecord> = Vec::new();

        for cmd in &meaningful {
            if &cmd.session_id == current_session_id {
                current_session_commands.push(cmd);
            } else {
                other_commands.push(cmd);
            }
        }

        // Sort by started_at (already sorted in meaningful, but we separated)
        // Both vectors are already in order because meaningful is in order

        // Start with most recent commands overall
        let mut selected: Vec<&CommandRecord> = Vec::new();
        let recent_overall = if meaningful.len() > self.max {
            meaningful[meaningful.len() - self.max..].to_vec()
        } else {
            meaningful.clone()
        };

        // Count current session commands in recent overall
        let current_in_recent = recent_overall.iter()
            .filter(|cmd| &cmd.session_id == current_session_id)
            .count();

        if current_in_recent >= self.min_current_session_commands {
            // Already satisfied, return recent overall
            return recent_overall;
        }

        // Need more current session commands
        let needed = self.min_current_session_commands - current_in_recent;

        // Get additional current session commands (most recent ones not already in recent_overall)
        let mut additional_current: Vec<&CommandRecord> = Vec::new();
        for cmd in current_session_commands.iter().rev() {
            // Check if cmd is already in recent_overall using pointer equality
            let mut found = false;
            for recent_cmd in &recent_overall {
                if std::ptr::eq(*cmd, *recent_cmd) {
                    found = true;
                    break;
                }
            }
            if !found {
                additional_current.push(cmd);
                if additional_current.len() >= needed {
                    break;
                }
            }
        }

        // Combine: start with recent_overall, add additional_current
        selected.extend_from_slice(&recent_overall);
        selected.extend_from_slice(&additional_current);

        // If we exceeded max, remove oldest non-current commands
        if selected.len() > self.max {
            let to_remove = selected.len() - self.max;
            let mut removed = 0;
            selected.retain(|cmd| {
                if removed >= to_remove {
                    return true;
                }
                if &cmd.session_id != current_session_id {
                    removed += 1;
                    false
                } else {
                    true
                }
            });

            // If still too long (all are current session), truncate oldest
            if selected.len() > self.max {
                selected.drain(0..selected.len() - self.max);
            }
        }

        // Sort by started_at to maintain chronological order
        selected.sort_by_key(|cmd| cmd.started_at);
        selected
    }
}

/// Formats commands grouped by session, with the current session last.
pub struct GroupedFormatter {
    current_session_id: String,
    _now_ms: u64,
    head_lines: usize,
    tail_lines: usize,
}

impl GroupedFormatter {
    pub fn new(current_session_id: &str, now_ms: u64, head_lines: usize, tail_lines: usize) -> Self {
        Self {
            current_session_id: current_session_id.to_string(),
            _now_ms: now_ms,
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
                let prefix = format_command_prefix(&cmd.hostname, &cmd.cwd);
                let prefix_display = if prefix.is_empty() { String::new() } else { format!("{} ", prefix) };
                history_lines.push(format!("{}$ {}", prefix_display, cmd_line));
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
                let current_session_commands: Vec<&CommandContext> = detailed.iter()
                    .filter(|c| &c.session_id == session_id)
                    .collect();

                for cmd in &current_session_commands {
                    let cmd_line = cmd.command_line.as_deref().unwrap_or("(unknown)");
                    let prefix = format_command_prefix(&cmd.hostname, &cmd.cwd);
                    let max_lines = self.head_lines + self.tail_lines;
                    let output = truncate_lines(&cmd.output, max_lines, self.head_lines, self.tail_lines);

                    let failed_tag = match cmd.exit_code {
                        Some(code) if code != 0 => format!("  [FAILED: {}]", code),
                        _ => String::new(),
                    };
                    if output.is_empty() {
                        let prefix_display = if prefix.is_empty() { String::new() } else { format!("{} ", prefix) };
                        group_lines.push(format!("{}$ {}{}", prefix_display, cmd_line, failed_tag));
                    } else {
                        let prefix_display = if prefix.is_empty() { String::new() } else { format!("{} ", prefix) };
                        group_lines.push(format!("{}$ {}{}\n{}", prefix_display, cmd_line, failed_tag, output));
                    }
                }

                // For current session, display current path at the end
                if is_current {
                    // Find the most recent command's cwd (last in list since commands are sorted by started_at)
                    let current_path = current_session_commands.last()
                        .and_then(|cmd| cmd.cwd.as_ref())
                        .map(|cwd| cwd.as_str())
                        .unwrap_or("");

                    if !current_path.is_empty() {
                        group_lines.push(format!("Current path: {}", current_path));
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
    _now_ms: u64,
    head_lines: usize,
    tail_lines: usize,
}

impl InterleavedFormatter {
    pub fn new(current_session_id: &str, now_ms: u64, head_lines: usize, tail_lines: usize) -> Self {
        Self {
            current_session_id: current_session_id.to_string(),
            _now_ms: now_ms,
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
                let prefix = format_command_prefix(&cmd.hostname, &cmd.cwd);
                let prefix_display = if prefix.is_empty() { String::new() } else { format!("{} ", prefix) };
                history_lines.push(format!("{}$ {}", prefix_display, cmd_line));
            }
            sections.push(history_lines.join("\n"));
        }

        // Detailed section: interleaved by time
        if !detailed.is_empty() {
            let labels = assign_term_labels(detailed, &self.current_session_id);

            let mut sorted: Vec<&CommandContext> = detailed.iter().collect();
            sorted.sort_by_key(|c| c.started_at);

            for cmd in sorted {
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
                let prefix = format_command_prefix(&cmd.hostname, &cmd.cwd);
                let prefix_display = if prefix.is_empty() { String::new() } else { format!("{} ", prefix) };
                if output.is_empty() {
                    sections.push(format!("{} {}$ {}{}", label_str, prefix_display, cmd_line, failed_tag));
                } else {
                    sections.push(format!("{} {}$ {}{}\n{}", label_str, prefix_display, cmd_line, failed_tag, output));
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

    #[tokio::test]
    async fn test_select_min_current_session_commands() {
        // Create commands from two sessions: sess-a (current) and sess-b
        let mut cmds = Vec::new();
        // sess-a commands: 0, 2, 4, 6, 8, 10, 12, 14
        // sess-b commands: 1, 3, 5, 7, 9, 11, 13
        for i in 0..15 {
            let session = if i % 2 == 0 { "sess-a" } else { "sess-b" };
            cmds.push(make_cmd(i, session, Some(&format!("cmd{}", i))));
        }

        // Test 1: max=5, min_current=2, should include at least 2 from sess-a
        let strategy = RecentCommands::new(5)
            .with_current_session("sess-a", 2);
        let selected = strategy.select_commands(&cmds).await;
        assert_eq!(selected.len(), 5);
        let current_count = selected.iter().filter(|c| c.session_id == "sess-a").count();
        assert!(current_count >= 2, "Should have at least 2 current session commands, got {}", current_count);

        // Test 2: max=4, min_current=3, but only 2 sess-a commands in recent 4
        // Recent 4 overall: cmd11(sess-b), cmd12(sess-a), cmd13(sess-b), cmd14(sess-a)
        // Has 2 sess-a, need 3 -> should include older sess-a command
        let strategy = RecentCommands::new(4)
            .with_current_session("sess-a", 3);
        let selected = strategy.select_commands(&cmds).await;
        assert_eq!(selected.len(), 4);
        let current_count = selected.iter().filter(|c| c.session_id == "sess-a").count();
        assert_eq!(current_count, 3, "Should have exactly 3 current session commands, got {}", current_count);

        // Test 3: max=3, min_current=5, but only 4 sess-a commands total
        // Should include all 4 sess-a commands and truncate to max=3? Actually min_current=5 > total sess-a commands=4
        // Should include all 4 sess-a and fill with others up to max? max=3 < 4, so just 3 sess-a commands
        let strategy = RecentCommands::new(3)
            .with_current_session("sess-a", 5);
        let selected = strategy.select_commands(&cmds).await;
        // Can't satisfy min_current=5, will include as many sess-a as possible
        assert_eq!(selected.len(), 3);
        let current_count = selected.iter().filter(|c| c.session_id == "sess-a").count();
        // Should be all 3 from sess-a (most recent)
        assert_eq!(current_count, 3, "Should have all 3 current session commands, got {}", current_count);
    }

    // --- GroupedFormatter tests ---

    #[test]
    fn test_grouped_single_session() {
        let detailed = vec![make_ctx("sess-a", "ls", 30000, "file1.txt")];
        let formatter = GroupedFormatter::new("sess-a", 60000, 10, 10);
        let result = formatter.format(&[], &detailed);
        assert!(result.contains("--- term A [current] ---"));
        assert!(result.contains("$ ls"));
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

    #[test]
    fn test_grouped_displays_current_path_at_end() {
        // Create detailed commands with cwd
        let detailed = vec![
            CommandContext {
                session_id: "sess-a".into(),
                hostname: None,
                command_line: Some("ls".into()),
                cwd: Some("/home/user".into()),
                started_at: 1000,
                ended_at: Some(1050),
                output: "file.txt".into(),
                exit_code: Some(0),
            },
            CommandContext {
                session_id: "sess-a".into(),
                hostname: None,
                command_line: Some("cd /tmp".into()),
                cwd: Some("/home/user".into()),  // Command cwd before cd
                started_at: 2000,
                ended_at: Some(2050),
                output: "".into(),
                exit_code: Some(0),
            },
            // Most recent command with new cwd
            CommandContext {
                session_id: "sess-a".into(),
                hostname: None,
                command_line: Some("pwd".into()),
                cwd: Some("/tmp".into()),  // Current cwd after cd
                started_at: 3000,
                ended_at: Some(3050),
                output: "/tmp".into(),
                exit_code: Some(0),
            },
        ];

        let formatter = GroupedFormatter::new("sess-a", 4000, 10, 10);
        let result = formatter.format(&[], &detailed);

        // Should contain "Current path: /tmp" at the end of the session section
        let session_end_marker = "--- term A [current] ---";
        let session_start_pos = result.find(session_end_marker).unwrap();
        let session_section = &result[session_start_pos..];

        // Check that current path is displayed
        assert!(session_section.contains("Current path: /tmp"),
                "Session section should display current path: {}", session_section);

        // Should not contain path from older commands
        assert!(!session_section.contains("Current path: /home/user"),
                "Should only show most recent cwd");
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
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader, &std::collections::HashMap::new(), 10, 512)
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
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader, &std::collections::HashMap::new(), 10, 512)
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
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader, &std::collections::HashMap::new(), 10, 512)
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
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader, &std::collections::HashMap::new(), 10, 512)
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
        let result = crate::build_context(&strategy, &formatter, &[], &reader, &std::collections::HashMap::new(), 10, 512)
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
        let result = crate::build_context(&strategy, &formatter, &cmds, &reader, &std::collections::HashMap::new(), 2, 512)
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

    #[test]
    fn test_format_includes_cwd() {
        let context = CommandContext {
            session_id: "sess1".into(),
            hostname: None,
            command_line: Some("ls -la".into()),
            cwd: Some("/home/user/project".into()),
            started_at: 1000,
            ended_at: Some(1002),
            output: "total 0\nfile.txt".into(),
            exit_code: Some(0),
        };
        let commands = vec![context];
        let formatter = GroupedFormatter::new("sess1", 10000, 5, 5);
        let formatted = formatter.format(&[], &commands);
        assert!(formatted.contains("/home/user/project $ ls -la"),
                "Formatted output should include cwd: {}", formatted);
    }

    #[test]
    fn test_command_format_with_hostname_and_cwd() {
        // Test that command includes hostname:cwd prefix when both are present
        let context = CommandContext {
            session_id: "sess1".into(),
            hostname: Some("myhost".into()),
            command_line: Some("ls -la".into()),
            cwd: Some("/home/user/project".into()),
            started_at: 1000,
            ended_at: Some(1002),
            output: "".into(),
            exit_code: Some(0),
        };
        let commands = vec![context];
        let formatter = GroupedFormatter::new("sess1", 10000, 5, 5);
        let formatted = formatter.format(&[], &commands);
        // Should show "myhost:/home/user/project $ ls -la" (without time)
        assert!(formatted.contains("myhost:/home/user/project $ ls -la"),
                "Formatted output should include hostname:cwd prefix: {}", formatted);
        // Should NOT contain time prefix "[...]"
        assert!(!formatted.contains("ago]"),
                "Formatted output should not contain time: {}", formatted);
    }

    #[test]
    fn test_history_format_with_hostname_and_cwd() {
        // Test that history section includes hostname:cwd prefix
        let history = vec![
            CommandContext {
                session_id: "sess1".into(),
                hostname: Some("myhost".into()),
                command_line: Some("cd /tmp".into()),
                cwd: Some("/home/user".into()),
                started_at: 1000,
                ended_at: Some(1002),
                output: "".into(),
                exit_code: Some(0),
            },
        ];
        let formatter = GroupedFormatter::new("sess1", 10000, 5, 5);
        let formatted = formatter.format(&history, &[]);
        // History should show "myhost:/home/user $ cd /tmp"
        assert!(formatted.contains("myhost:/home/user $ cd /tmp"),
                "History should include hostname:cwd prefix: {}", formatted);
        assert!(formatted.contains("--- History ---"),
                "Should have history section: {}", formatted);
    }
}
