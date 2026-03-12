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
            let seq = start + i + 1; // 1-based
            let cmd_line = cmd.command_line.as_deref().unwrap_or("(unknown)");
            let exit = cmd.exit_code.map(|c| format!("exit {}", c)).unwrap_or_default();
            let ago = format_ago(now_ms, cmd.started_at);
            lines.push(format!("[seq={}] {}  ({}, {})", seq, cmd_line, exit, ago));
        }
        lines.join("\n")
    }

    /// Build a system-reminder string for the chat user message.
    /// Includes current time, working directory, and last N commands.
    /// `live_cwd` overrides the command-record cwd (from session probe).
    pub fn build_system_reminder(&self, count: usize, live_cwd: Option<&str>) -> String {
        let commands = &self.commands;

        // Current time with timezone
        let now = chrono::Local::now();
        let time_str = now.format("%Y-%m-%d %H:%M:%S %z").to_string();

        // Current directory: prefer live cwd from session probe, fall back to last command's cwd
        let cwd = live_cwd
            .or_else(|| commands.last().and_then(|c| c.cwd.as_deref()))
            .unwrap_or("(unknown)");

        // Last N commands
        let start = commands.len().saturating_sub(count);
        let mut cmd_lines = Vec::new();
        for (i, cmd) in commands[start..].iter().enumerate() {
            let seq = start + i + 1;
            let cmd_line = cmd.command_line.as_deref().unwrap_or("(unknown)");
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
            "<system-reminder>\nTIME: {}\n\nWORKING DIR: {}\n\nLAST {} COMMANDS:\n{}\n</system-reminder>",
            time_str, cwd, count, cmds
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

    pub fn definition(&self) -> ToolDef {
        ToolDef {
            name: "command_query".to_string(),
            description: "Query shell command history and get full command output. \
                Use get_output(seq) to retrieve the full output of a specific command. \
                The last 5 commands are provided in <system-reminder> at the end of each user message, \
                so you do NOT need to call list_history unless you need older commands.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list_history", "get_output"],
                        "description": "Action to perform"
                    },
                    "seq": {
                        "type": "integer",
                        "description": "Command sequence number (required for get_output, obtained from list_history)"
                    },
                    "count": {
                        "type": "integer",
                        "description": "Number of recent commands to list (default 20, only for list_history)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    pub fn execute(&self, input: &serde_json::Value) -> ToolResult {
        let action = input["action"].as_str().unwrap_or("");
        let tool_use_id = String::new(); // Filled by caller

        match action {
            "list_history" => {
                let count = input["count"].as_u64().unwrap_or(20) as usize;
                let content = self.list_history(count);
                ToolResult { tool_use_id, content, is_error: false }
            }
            "get_output" => {
                let seq = input["seq"].as_u64().unwrap_or(0) as usize;
                if seq == 0 {
                    return ToolResult {
                        tool_use_id,
                        content: "Error: 'seq' is required for get_output".to_string(),
                        is_error: true,
                    };
                }
                let content = self.get_output(seq);
                ToolResult { tool_use_id, content, is_error: false }
            }
            _ => ToolResult {
                tool_use_id,
                content: format!("Error: unknown action '{}'", action),
                is_error: true,
            },
        }
    }

    pub fn status_text(&self, _tool_name: &str, input: &serde_json::Value) -> String {
        match input["action"].as_str() {
            Some("list_history") => "查询命令历史...".to_string(),
            Some("get_output") => format!(
                "获取命令输出 [{}]...",
                input["seq"].as_u64().unwrap_or(0)
            ),
            _ => "查询命令...".to_string(),
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
