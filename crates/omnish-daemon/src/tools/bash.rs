use crate::plugin::Plugin;
use omnish_llm::tool::{Tool, ToolDef, ToolResult};
use std::process::Command;

/// Maximum output bytes to return from a bash command.
const MAX_OUTPUT_BYTES: usize = 50_000;
/// Maximum lines to return.
const MAX_OUTPUT_LINES: usize = 500;
/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

pub struct BashTool;

impl BashTool {
    pub fn new() -> Self {
        Self
    }

    fn run(&self, command: &str, timeout_secs: u64) -> ToolResult {
        let mut child = match Command::new("bash")
            .arg("-c")
            .arg(command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Failed to execute command: {}", e),
                    is_error: true,
                };
            }
        };

        let timeout = std::time::Duration::from_secs(timeout_secs);
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return ToolResult {
                            tool_use_id: String::new(),
                            content: format!(
                                "Command timed out after {}s",
                                timeout_secs
                            ),
                            is_error: true,
                        };
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => {
                    return ToolResult {
                        tool_use_id: String::new(),
                        content: format!("Error waiting for command: {}", e),
                        is_error: true,
                    };
                }
            }
        }

        let output = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => {
                return ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Failed to read command output: {}", e),
                    is_error: true,
                };
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut content = String::new();

        if !stdout.is_empty() {
            content.push_str(&truncate_output(&stdout));
        }

        if !stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("[stderr]\n");
            content.push_str(&truncate_output(&stderr));
        }

        if content.is_empty() {
            content = "(no output)".to_string();
        }

        let exit_code = output.status.code().unwrap_or(-1);
        if exit_code != 0 {
            content.push_str(&format!("\n[exit code: {}]", exit_code));
        }

        ToolResult {
            tool_use_id: String::new(),
            content,
            is_error: exit_code != 0,
        }
    }
}

fn truncate_output(text: &str) -> String {
    let mut result = String::new();
    for (line_count, line) in text.lines().enumerate() {
        if line_count >= MAX_OUTPUT_LINES || result.len() + line.len() > MAX_OUTPUT_BYTES {
            result.push_str(&format!(
                "\n... (truncated, {} total lines)",
                text.lines().count()
            ));
            break;
        }
        if line_count > 0 {
            result.push('\n');
        }
        result.push_str(line);
    }
    result
}

impl Tool for BashTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: "bash".to_string(),
            description: "Execute a bash command and return its output. Use this to run \
                shell commands, inspect files, check system state, or perform any operation \
                the user asks about. Commands run in the daemon's working directory."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn execute(&self, input: &serde_json::Value) -> ToolResult {
        let command = input["command"].as_str().unwrap_or("");
        if command.is_empty() {
            return ToolResult {
                tool_use_id: String::new(),
                content: "Error: 'command' is required".to_string(),
                is_error: true,
            };
        }
        let timeout = input["timeout"]
            .as_u64()
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        self.run(command, timeout)
    }
}

impl Plugin for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn plugin_type(&self) -> crate::plugin::PluginType {
        crate::plugin::PluginType::ClientTool
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![self.definition()]
    }

    fn call_tool(&self, _tool_name: &str, input: &serde_json::Value) -> ToolResult {
        self.execute(input)
    }

    fn system_prompt(&self) -> Option<String> {
        Some(
            "### bash\n\
             Execute bash commands on the user's machine:\n\
             - Use this to run commands, inspect files, check system state, etc.\n\
             - Commands run in the user's current working directory.\n\
             - The tool runs in a sandboxed environment with restricted write access.\n\
             - If a command fails with a permission error, do not retry. Explain the error to the user."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_echo() {
        let tool = BashTool::new();
        let result = tool.execute(&serde_json::json!({"command": "echo hello"}));
        assert!(!result.is_error);
        assert_eq!(result.content.trim(), "hello");
    }

    #[test]
    fn test_exit_code() {
        let tool = BashTool::new();
        let result = tool.execute(&serde_json::json!({"command": "exit 42"}));
        assert!(result.is_error);
        assert!(result.content.contains("exit code: 42"));
    }

    #[test]
    fn test_stderr() {
        let tool = BashTool::new();
        let result = tool.execute(&serde_json::json!({"command": "echo err >&2"}));
        assert!(!result.is_error);
        assert!(result.content.contains("[stderr]"));
        assert!(result.content.contains("err"));
    }

    #[test]
    fn test_empty_command() {
        let tool = BashTool::new();
        let result = tool.execute(&serde_json::json!({"command": ""}));
        assert!(result.is_error);
        assert!(result.content.contains("required"));
    }

    #[test]
    fn test_truncate_output() {
        let long = "x\n".repeat(600);
        let truncated = truncate_output(&long);
        assert!(truncated.contains("truncated"));
        assert!(truncated.lines().count() <= MAX_OUTPUT_LINES + 1);
    }

    #[test]
    fn test_timeout() {
        let tool = BashTool::new();
        let result = tool.run("sleep 10", 1);
        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
    }
}
