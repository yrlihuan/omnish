use omnish_llm::tool::ToolResult;
use std::process::Command;

/// Maximum output characters to return from a bash command.
const MAX_OUTPUT_CHARS: usize = 30_000;
/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Maximum command timeout in seconds.
const MAX_TIMEOUT_SECS: u64 = 900;

#[derive(Default)]
pub struct BashTool;

impl BashTool {
    pub fn new() -> Self {
        Self
    }

    fn run(&self, command: &str, timeout_secs: u64, cwd: Option<&str>, shell: Option<&str>) -> ToolResult {
        let shell = shell.unwrap_or("bash");
        let mut cmd = Command::new(shell);
        cmd.arg("-c")
            .arg(command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut cwd_note = String::new();
        if let Some(cwd) = cwd {
            if std::path::Path::new(cwd).is_dir() {
                cmd.current_dir(cwd);
            } else {
                let fallback = std::env::var("HOME")
                    .ok()
                    .filter(|h| std::path::Path::new(h).is_dir())
                    .unwrap_or_else(|| "/".to_string());
                cwd_note = format!(
                    "[Note: working directory '{}' no longer exists, using '{}' instead]\n",
                    cwd, fallback
                );
                cmd.current_dir(&fallback);
            }
        }
        let mut child = match cmd.spawn()
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
            content.push_str(&truncate_output(&stderr));
        }

        if content.is_empty() {
            content = "(no output)".to_string();
        }

        let exit_code = output.status.code().unwrap_or(-1);
        if exit_code != 0 {
            content.push_str(&format!("\n[exit code: {}]", exit_code));
        }

        if !cwd_note.is_empty() {
            content = format!("{}{}", cwd_note, content);
        }

        ToolResult {
            tool_use_id: String::new(),
            content,
            is_error: exit_code != 0,
        }
    }

    pub fn execute(&self, input: &serde_json::Value) -> ToolResult {
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
            .map(|ms| (ms / 1000).min(MAX_TIMEOUT_SECS))
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        let cwd = input["cwd"].as_str();
        let shell = input["shell"].as_str();
        self.run(command, timeout, cwd, shell)
    }
}

fn truncate_output(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_string();
    }
    let truncated: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    format!(
        "{}\n... (truncated at {} characters, {} total)",
        truncated,
        MAX_OUTPUT_CHARS,
        text.chars().count()
    )
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
        let long = "x".repeat(35_000);
        let truncated = truncate_output(&long);
        assert!(truncated.contains("truncated"));
        assert!(truncated.contains("30000 characters"));
    }

    #[test]
    fn test_timeout() {
        let tool = BashTool::new();
        let result = tool.run("sleep 10", 1, None, None);
        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
    }

    #[test]
    fn test_invalid_cwd_fallback() {
        let tool = BashTool::new();
        let result = tool.run("echo ok", 10, Some("/nonexistent_dir_12345"), None);
        assert!(!result.is_error);
        assert!(result.content.contains("no longer exists"));
        assert!(result.content.contains("ok"));
    }
}
