use omnish_context::StreamReader;
use omnish_llm::tool::{ToolDef, ToolResult};
use omnish_store::command::CommandRecord;
use std::sync::Arc;

/// Maximum lines to return from get_output to prevent huge responses.
const MAX_OUTPUT_LINES: usize = 500;
/// Maximum bytes to return from get_output.
const MAX_OUTPUT_BYTES: usize = 50_000;

pub struct CommandQueryTool {
    commands: Vec<CommandRecord>,
    stream_reader: Arc<dyn StreamReader>,
}

impl CommandQueryTool {
    pub fn new(
        commands: Vec<CommandRecord>,
        stream_reader: Arc<dyn StreamReader>,
    ) -> Self {
        Self { commands, stream_reader }
    }

    pub fn list_history(&self, count: usize) -> String {
        let commands = &self.commands;
        if commands.is_empty() {
            return "No commands in history.".to_string();
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let start = commands.len().saturating_sub(count);
        let mut lines = Vec::new();
        for (i, cmd) in commands[start..].iter().enumerate() {
            let cmd_line = match cmd.command_line.as_deref() {
                Some(line) if !line.is_empty() => line,
                _ => continue,
            };
            let seq = start + i + 1; // 1-based
            let exit = cmd.exit_code.map(|c| format!("exit {}", c)).unwrap_or_default();
            let ago = format_ago(now_ms, cmd.started_at);
            lines.push(format!("[seq={}] {}  ({}, {})", seq, cmd_line, exit, ago));
        }
        lines.join("\n")
    }

    /// Build a system-reminder string for the chat user message.
    /// Includes current time, working directory, git status, platform info, and last N commands.
    /// `live_cwd` overrides the command-record cwd (from session probe).
    pub fn build_system_reminder(&self, count: usize, live_cwd: Option<&str>) -> String {
        let commands = &self.commands;

        // Current time with timezone
        let now = chrono::Local::now();
        let time_str = now.format("%Y-%m-%d %H:%M:%S %z").to_string();
        let today = now.format("%Y-%m-%d").to_string();

        // Current directory: prefer live cwd from session probe, fall back to last command's cwd
        let cwd = live_cwd
            .or_else(|| commands.last().and_then(|c| c.cwd.as_deref()))
            .unwrap_or("(unknown)");

        // Check if cwd is a git repo
        let is_git_repo = std::process::Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(cwd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let is_git_repo_str = if is_git_repo { "Yes" } else { "No" };

        // Platform info
        let platform = std::env::consts::OS;
        let os_version = std::process::Command::new("uname")
            .arg("-r")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        // Last N commands (skip entries with no command_line)
        let start = commands.len().saturating_sub(count);
        let mut cmd_lines = Vec::new();
        for (i, cmd) in commands[start..].iter().enumerate() {
            let cmd_line = match cmd.command_line.as_deref() {
                Some(line) if !line.is_empty() => line,
                _ => continue,
            };
            let seq = start + i + 1;
            let failed = match cmd.exit_code {
                Some(code) if code != 0 => " [FAILED]",
                _ => "",
            };
            cmd_lines.push(format!("[seq={}] {}{}", seq, cmd_line, failed));
        }

        let cmds = if cmd_lines.is_empty() {
            "(none)".to_string()
        } else {
            cmd_lines.join("\n")
        };

        format!(
            "<system-reminder>\nTIME: {}\n\nWORKING DIR: {}\n\nIs directory a git repo: {}\n\nPlatform: {}\n\nOS Version: {}\n\nToday's date: {}\n\nLAST {} COMMANDS:\n{}\n</system-reminder>",
            time_str, cwd, is_git_repo_str, platform, os_version, today, count, cmds
        )
    }

    fn get_output(&self, seq: usize) -> String {
        let commands = &self.commands;
        if seq == 0 || seq > commands.len() {
            return format!("Error: seq {} out of range (1-{})", seq, commands.len());
        }
        let cmd = &commands[seq - 1];
        if cmd.stream_length == 0 {
            return "(no output recorded)".to_string();
        }
        match self.stream_reader.read_command_output(cmd.stream_offset, cmd.stream_length) {
            Ok(entries) => {
                let mut raw = Vec::new();
                for entry in &entries {
                    if entry.direction == 1 { // Output direction
                        raw.extend_from_slice(&entry.data);
                    }
                }
                let text = omnish_context::strip_ansi(&raw);
                // Skip first line (echoed command)
                let text = match text.find('\n') {
                    Some(pos) => text[pos + 1..].trim_start().to_string(),
                    None => text,
                };
                // Truncate by lines and bytes
                let mut result = String::new();
                for (line_count, line) in text.lines().enumerate() {
                    if line_count >= MAX_OUTPUT_LINES || result.len() + line.len() > MAX_OUTPUT_BYTES {
                        result.push_str(&format!("\n... (truncated, {} total lines)", text.lines().count()));
                        break;
                    }
                    if line_count > 0 { result.push('\n'); }
                    result.push_str(line);
                }
                result
            }
            Err(e) => format!("Error reading output: {}", e),
        }
    }

    /// Full detail view of a single command for `/debug command <seq>`.
    pub fn get_command_detail(&self, seq: usize) -> String {
        let commands = &self.commands;
        if seq == 0 || seq > commands.len() {
            return format!("Error: seq {} out of range (1-{})", seq, commands.len());
        }
        let cmd = &commands[seq - 1];
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut lines = Vec::new();
        lines.push(format!("[seq={}] {}", seq, cmd.command_line.as_deref().unwrap_or("(none)")));
        lines.push(format!("  cwd:    {}", cmd.cwd.as_deref().unwrap_or("(unknown)")));
        lines.push(format!("  exit:   {}", cmd.exit_code.map(|c| c.to_string()).unwrap_or("(none)".into())));
        lines.push(format!("  time:   {}", format_ago(now_ms, cmd.started_at)));
        if let Some(ended) = cmd.ended_at {
            let dur_ms = ended.saturating_sub(cmd.started_at);
            if dur_ms < 1000 {
                lines.push(format!("  dur:    {}ms", dur_ms));
            } else {
                lines.push(format!("  dur:    {:.1}s", dur_ms as f64 / 1000.0));
            }
        }
        lines.push(format!("  id:     {}", cmd.command_id));
        lines.push(String::new());
        lines.push("--- output ---".to_string());
        lines.push(self.get_output(seq));
        lines.join("\n")
    }

    pub fn definitions(&self) -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "omnish_list_history".to_string(),
                description: "List recent shell command history. \
                    The last 5 commands are provided in <system-reminder> at the end of each user message, \
                    so you do NOT need to call this unless you need older commands.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "count": {
                            "type": "integer",
                            "description": "Number of recent commands to list (default 20)"
                        }
                    }
                }),
            },
            ToolDef {
                name: "omnish_get_output".to_string(),
                description: "Get the full output of a specific shell command by its sequence number. \
                    Use omnish_list_history to find the sequence number first.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "seq": {
                            "type": "integer",
                            "description": "Command sequence number (from omnish_list_history or <system-reminder>)"
                        },
                        "command": {
                            "type": "string",
                            "description": "The command string at that seq (must match the recorded command)"
                        }
                    },
                    "required": ["seq", "command"]
                }),
            },
        ]
    }

    pub fn execute(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult {
        let tool_use_id = String::new(); // Filled by caller
        match tool_name {
            "omnish_list_history" => {
                let count = input["count"].as_u64().unwrap_or(20) as usize;
                let content = self.list_history(count);
                ToolResult { tool_use_id, content, is_error: false }
            }
            "omnish_get_output" => {
                let seq = input["seq"].as_u64().unwrap_or(0) as usize;
                if seq == 0 {
                    return ToolResult {
                        tool_use_id,
                        content: "Error: 'seq' is required".to_string(),
                        is_error: true,
                    };
                }
                let command = input["command"].as_str().unwrap_or("");
                if command.is_empty() {
                    return ToolResult {
                        tool_use_id,
                        content: "Error: 'command' is required".to_string(),
                        is_error: true,
                    };
                }
                // Validate command matches the recorded command at this seq
                if seq <= self.commands.len() {
                    let recorded = self.commands[seq - 1].command_line.as_deref().unwrap_or("");
                    if recorded != command {
                        return ToolResult {
                            tool_use_id,
                            content: format!(
                                "Error: command mismatch at seq {}.\n  expected: {}\n  got: {}",
                                seq, recorded, command
                            ),
                            is_error: true,
                        };
                    }
                }
                let content = self.get_output(seq);
                ToolResult { tool_use_id, content, is_error: false }
            }
            _ => ToolResult {
                tool_use_id,
                content: format!("Error: unknown tool '{}'", tool_name),
                is_error: true,
            },
        }
    }

    pub fn display_name(tool_name: &str) -> &'static str {
        match tool_name {
            "omnish_list_history" => "History",
            "omnish_get_output" => "GetOutput",
            _ => "CommandQuery",
        }
    }

    pub fn status_text(&self, tool_name: &str, input: &serde_json::Value) -> String {
        match tool_name {
            "omnish_list_history" => {
                let count = input["count"].as_u64().unwrap_or(20);
                format!("last {}", count)
            }
            "omnish_get_output" => {
                let seq = input["seq"].as_u64().unwrap_or(0);
                let command = input["command"].as_str().unwrap_or("");
                if command.is_empty() {
                    format!("[{}]", seq)
                } else {
                    format!("[{}] {}", seq, command)
                }
            }
            _ => String::new(),
        }
    }
}

fn format_ago(now_ms: u64, started_at: u64) -> String {
    let diff_s = now_ms.saturating_sub(started_at) / 1000;
    if diff_s < 60 { format!("{}s ago", diff_s) }
    else if diff_s < 3600 { format!("{}m ago", diff_s / 60) }
    else if diff_s < 86400 { format!("{}h ago", diff_s / 3600) }
    else { format!("{}d ago", diff_s / 86400) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnish_store::stream::StreamEntry;

    struct DummyReader;
    impl StreamReader for DummyReader {
        fn read_command_output(&self, _offset: u64, _length: u64) -> anyhow::Result<Vec<StreamEntry>> {
            Ok(vec![])
        }
    }

    fn make_cmd(cmd_line: &str, cwd: Option<&str>, exit_code: Option<i32>) -> CommandRecord {
        CommandRecord {
            command_id: String::new(),
            session_id: String::new(),
            command_line: Some(cmd_line.to_string()),
            cwd: cwd.map(|s| s.to_string()),
            started_at: 0,
            ended_at: None,
            output_summary: String::new(),
            stream_offset: 0,
            stream_length: 0,
            exit_code,
        }
    }

    fn make_tool(commands: Vec<CommandRecord>) -> CommandQueryTool {
        CommandQueryTool::new(commands, Arc::new(DummyReader))
    }

    #[test]
    fn test_cwd_prefers_live_cwd_over_command_record() {
        let tool = make_tool(vec![
            make_cmd("ls", Some("/home/user/old"), Some(0)),
        ]);
        let reminder = tool.build_system_reminder(5, Some("/home/user/live"));
        assert!(reminder.contains("WORKING DIR: /home/user/live"));
        assert!(!reminder.contains("/home/user/old"));
    }

    #[test]
    fn test_cwd_falls_back_to_last_command_cwd() {
        let tool = make_tool(vec![
            make_cmd("cd /tmp", Some("/tmp"), Some(0)),
            make_cmd("ls", Some("/home/user/proj"), Some(0)),
        ]);
        let reminder = tool.build_system_reminder(5, None);
        assert!(reminder.contains("WORKING DIR: /home/user/proj"));
    }

    #[test]
    fn test_cwd_unknown_when_no_source() {
        let tool = make_tool(vec![
            make_cmd("ls", None, Some(0)),
        ]);
        let reminder = tool.build_system_reminder(5, None);
        assert!(reminder.contains("WORKING DIR: (unknown)"));
    }

    #[test]
    fn test_cwd_unknown_when_no_commands_and_no_live() {
        let tool = make_tool(vec![]);
        let reminder = tool.build_system_reminder(5, None);
        assert!(reminder.contains("WORKING DIR: (unknown)"));
    }

    #[test]
    fn test_failed_command_shows_failed_marker() {
        let tool = make_tool(vec![
            make_cmd("cargo build", None, Some(0)),
            make_cmd("cargo test", None, Some(1)),
        ]);
        let reminder = tool.build_system_reminder(5, None);
        assert!(reminder.contains("[seq=1] cargo build\n"));
        assert!(reminder.contains("[seq=2] cargo test [FAILED]"));
    }

    #[test]
    fn test_reminder_limits_to_last_n_commands() {
        let commands: Vec<_> = (1..=10)
            .map(|i| make_cmd(&format!("cmd{}", i), None, Some(0)))
            .collect();
        let tool = make_tool(commands);
        let reminder = tool.build_system_reminder(3, None);
        assert!(!reminder.contains("cmd7"));
        assert!(reminder.contains("[seq=8] cmd8"));
        assert!(reminder.contains("[seq=9] cmd9"));
        assert!(reminder.contains("[seq=10] cmd10"));
    }

    #[test]
    fn test_reminder_contains_time_and_structure() {
        let tool = make_tool(vec![make_cmd("ls", Some("/tmp"), Some(0))]);
        let reminder = tool.build_system_reminder(5, None);
        assert!(reminder.starts_with("<system-reminder>"));
        assert!(reminder.ends_with("</system-reminder>"));
        assert!(reminder.contains("TIME: "));
        assert!(reminder.contains("WORKING DIR: /tmp"));
        assert!(reminder.contains("LAST 5 COMMANDS:"));
    }
}
