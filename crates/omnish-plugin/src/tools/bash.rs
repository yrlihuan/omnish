use omnish_llm::tool::ToolResult;
use std::io::Read;
use std::process::Command;

/// Maximum output characters to return from a bash command.
const MAX_OUTPUT_CHARS: usize = 30_000;
/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Maximum command timeout in seconds.
const MAX_TIMEOUT_SECS: u64 = 900;
/// Hard cap on bytes captured per stream. The output is char-truncated to
/// MAX_OUTPUT_CHARS anyway; this bounds the plugin's own memory when a command
/// streams a huge amount to stdout/stderr. 1 MiB comfortably exceeds
/// MAX_OUTPUT_CHARS (even at 4 bytes/char) so truncation stays accurate.
const MAX_CAPTURE_BYTES: usize = 1_048_576;

/// Drain a child pipe on its own thread so a command that writes more than the
/// OS pipe buffer (~64 KiB) never blocks on write. Reads to EOF (letting the
/// child finish) but only retains up to MAX_CAPTURE_BYTES.
fn spawn_drain<R: Read + Send + 'static>(reader: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() < MAX_CAPTURE_BYTES {
                        let take = n.min(MAX_CAPTURE_BYTES - buf.len());
                        buf.extend_from_slice(&chunk[..take]);
                    }
                    // Past the cap: keep reading (to unblock the child) but discard.
                }
                Err(_) => break,
            }
        }
        buf
    })
}

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

        // Drain stdout/stderr on separate threads while we wait. Without this,
        // a command that writes past the ~64 KiB pipe buffer blocks on write and
        // never exits, so the wait loop below would spin until the timeout.
        let out_thread = child.stdout.take().map(spawn_drain);
        let err_thread = child.stderr.take().map(spawn_drain);

        let timeout = std::time::Duration::from_secs(timeout_secs);
        let start = std::time::Instant::now();
        let exit_status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        // Threads finish once the killed child's pipes close.
                        if let Some(t) = out_thread { let _ = t.join(); }
                        if let Some(t) = err_thread { let _ = t.join(); }
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
        };

        let stdout_bytes = out_thread.map(|t| t.join().unwrap_or_default()).unwrap_or_default();
        let stderr_bytes = err_thread.map(|t| t.join().unwrap_or_default()).unwrap_or_default();
        let stdout = String::from_utf8_lossy(&stdout_bytes);
        let stderr = String::from_utf8_lossy(&stderr_bytes);

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

        let exit_code = exit_status.code().unwrap_or(-1);
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

    /// Regression: a command that writes far more than the OS pipe buffer must
    /// not deadlock. Before draining stdout/stderr concurrently, the child would
    /// block on write and the tool would spin until the timeout. With a short
    /// timeout, a broken impl would return "timed out"; the fix returns quickly
    /// with truncated output.
    #[test]
    fn test_large_output_no_deadlock() {
        let tool = BashTool::new();
        // ~500 KiB of stdout, well past the ~64 KiB pipe buffer.
        let result = tool.run("head -c 500000 /dev/zero | tr '\\0' 'x'", 10, None, None);
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(!result.content.contains("timed out"), "deadlocked: {}", result.content);
        assert!(result.content.contains("truncated"), "expected truncation note");
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
