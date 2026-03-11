use omnish_plugin::Plugin;
use omnish_context::StreamReader;
use omnish_llm::tool::{Tool, ToolDef, ToolResult};
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
}

impl Tool for CommandQueryTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: "command_query".to_string(),
            description: "Query shell command history and get full command output. \
                Use get_output(seq) to retrieve the full output of a specific command. \
                The recent command list is provided at the end of the user's message in <system-reminder>, \
                so you do NOT need to call list_history — the command list is already provided.".to_string(),
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

    fn execute(&self, input: &serde_json::Value) -> ToolResult {
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
}

impl Plugin for CommandQueryTool {
    fn name(&self) -> &str {
        "command_query"
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![self.definition()]
    }

    fn call_tool(&self, _tool_name: &str, input: &serde_json::Value) -> ToolResult {
        self.execute(input)
    }

    fn status_text(&self, _tool_name: &str, input: &serde_json::Value) -> String {
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
