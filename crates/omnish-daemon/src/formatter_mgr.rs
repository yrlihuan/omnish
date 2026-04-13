use omnish_plugin::formatter::{
    DefaultFormatter, EditFormatter, FormatInput, FormatOutput, ReadFormatter, ToolFormatter,
};
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

struct ExternalFormatter {
    tx: mpsc::Sender<(serde_json::Value, oneshot::Sender<ExternalResponse>)>,
}

#[derive(serde::Deserialize)]
struct ExternalResponse {
    summary: Option<String>,
    compact: Vec<String>,
    full: Vec<String>,
}

impl ExternalFormatter {
    async fn start(binary: &str) -> Result<Self, std::io::Error> {
        use std::process::Stdio;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (tx, mut rx) =
            mpsc::channel::<(serde_json::Value, oneshot::Sender<ExternalResponse>)>(32);

        // Retry on ETXTBSY — transient race between file close and execve
        // that can occur when the binary was just written (e.g. in tests).
        let mut child = {
            let mut attempts = 0;
            loop {
                match tokio::process::Command::new(binary)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    Ok(c) => break c,
                    Err(e) if e.raw_os_error() == Some(26) && attempts < 3 => { // ETXTBSY
                        attempts += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        };

        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut reader = BufReader::new(stdout).lines();

        tokio::spawn(async move {
            while let Some((req, reply)) = rx.recv().await {
                let mut line = serde_json::to_string(&req).unwrap();
                line.push('\n');
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }

                match reader.next_line().await {
                    Ok(Some(resp_line)) => {
                        match serde_json::from_str::<ExternalResponse>(&resp_line) {
                            Ok(resp) => {
                                let _ = reply.send(resp);
                            }
                            Err(e) => {
                                tracing::warn!("formatter response parse error: {}", e);
                                let _ = reply.send(ExternalResponse {
                                    summary: Some(format!("Formatter error: {}", e)),
                                    compact: vec![],
                                    full: vec![],
                                });
                            }
                        }
                    }
                    _ => break,
                }
            }
            let _ = child.kill().await;
        });

        Ok(Self { tx })
    }

    async fn format(&self, formatter_name: &str, input: &FormatInput) -> FormatOutput {
        let req = serde_json::json!({
            "formatter": formatter_name,
            "tool_name": input.tool_name,
            "params": input.params,
            "output": input.output,
            "is_error": input.is_error,
        });
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.tx.send((req, reply_tx)).await.is_err() {
            return FormatOutput {
                result_compact: vec!["Formatter unavailable".into()],
                result_full: vec!["Formatter unavailable".into()],
            };
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await {
            Ok(Ok(resp)) => {
                let mut compact = Vec::new();
                let mut full = Vec::new();
                if let Some(ref s) = resp.summary {
                    compact.push(s.clone());
                    full.push(s.clone());
                }
                compact.extend(resp.compact);
                full.extend(resp.full);
                FormatOutput {
                    result_compact: compact,
                    result_full: full,
                }
            }
            _ => FormatOutput {
                result_compact: vec!["Formatter timeout".into()],
                result_full: vec!["Formatter timeout".into()],
            },
        }
    }
}

pub struct FormatterManager {
    builtins: HashMap<String, Box<dyn ToolFormatter>>,
    externals: HashMap<String, ExternalFormatter>,
}

impl Default for FormatterManager {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatterManager {
    pub fn new() -> Self {
        let mut builtins: HashMap<String, Box<dyn ToolFormatter>> = HashMap::new();
        builtins.insert("default".into(), Box::new(DefaultFormatter));
        builtins.insert("read".into(), Box::new(ReadFormatter));
        builtins.insert("edit".into(), Box::new(EditFormatter));
        builtins.insert("write".into(), Box::new(EditFormatter));
        Self {
            builtins,
            externals: HashMap::new(),
        }
    }

    pub async fn register_external(
        &mut self,
        name: &str,
        binary: &str,
    ) -> Result<(), std::io::Error> {
        match ExternalFormatter::start(binary).await {
            Ok(ext) => {
                self.externals.insert(name.to_string(), ext);
                Ok(())
            }
            Err(e) => {
                tracing::warn!("failed to start formatter '{}' ({}): {}", name, binary, e);
                Err(e)
            }
        }
    }

    pub async fn format(&self, formatter_name: &str, input: &FormatInput) -> FormatOutput {
        // Check external first
        if let Some(ext) = self.externals.get(formatter_name) {
            return ext.format(formatter_name, input).await;
        }
        // Fall back to built-in
        let fmt = self
            .builtins
            .get(formatter_name)
            .or_else(|| self.builtins.get("default"))
            .unwrap();
        fmt.format(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_builtin_formatter_default() {
        let mgr = FormatterManager::new();
        let input = FormatInput {
            tool_name: "unknown_tool".into(),
            params: serde_json::json!({}),
            output: "hello\nworld".into(),
            is_error: false,
        };
        let out = mgr.format("default", &input).await;
        assert!(!out.result_compact.is_empty());
    }

    #[tokio::test]
    async fn test_builtin_formatter_edit() {
        let mgr = FormatterManager::new();
        let input = FormatInput {
            tool_name: "edit".into(),
            params: serde_json::json!({"file_path": "/tmp/test.txt", "old_string": "hello", "new_string": "goodbye"}),
            output: "Edited /tmp/test.txt\n---\n1:  before\n2:-hello\n2:+goodbye\n3:  after".into(),
            is_error: false,
        };
        let out = mgr.format("edit", &input).await;
        assert!(out.result_compact[0].contains("Edited 1 line"));
    }

    #[tokio::test]
    async fn test_unknown_formatter_falls_back_to_default() {
        let mgr = FormatterManager::new();
        let input = FormatInput {
            tool_name: "test".into(),
            params: serde_json::json!({}),
            output: "some output".into(),
            is_error: false,
        };
        let out = mgr.format("nonexistent", &input).await;
        assert!(!out.result_compact.is_empty());
    }

    /// Write a test script, sync to disk, and set executable permission.
    /// The sync prevents ETXTBSY when spawn() races with the write close.
    fn write_test_script(path: &std::path::Path, content: &str) {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.sync_all().unwrap();
        drop(f);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn test_external_formatter_echo() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("test_fmt");
        write_test_script(
            &script,
            r#"#!/bin/bash
while IFS= read -r line; do
    echo '{"summary":"test summary","compact":["compact line"],"full":["full line 1","full line 2"]}'
done
"#,
        );

        let mut mgr = FormatterManager::new();
        mgr.register_external("test_fmt", script.to_str().unwrap())
            .await
            .unwrap();

        let input = FormatInput {
            tool_name: "test_tool".into(),
            params: serde_json::json!({}),
            output: "raw output".into(),
            is_error: false,
        };
        let out = mgr.format("test_fmt", &input).await;
        assert_eq!(out.result_compact, vec!["test summary", "compact line"]);
        assert_eq!(
            out.result_full,
            vec!["test summary", "full line 1", "full line 2"]
        );
    }

    #[tokio::test]
    async fn test_external_formatter_sequential() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("counter_fmt");
        write_test_script(
            &script,
            r#"#!/bin/bash
n=0
while IFS= read -r line; do
    n=$((n + 1))
    echo "{\"summary\":\"call $n\",\"compact\":[\"call $n\"],\"full\":[\"call $n\"]}"
done
"#,
        );

        let mut mgr = FormatterManager::new();
        mgr.register_external("counter", script.to_str().unwrap())
            .await
            .unwrap();

        let input = FormatInput {
            tool_name: "t".into(),
            params: serde_json::json!({}),
            output: "x".into(),
            is_error: false,
        };
        let out1 = mgr.format("counter", &input).await;
        let out2 = mgr.format("counter", &input).await;
        assert_eq!(out1.result_compact, vec!["call 1", "call 1"]);
        assert_eq!(out2.result_compact, vec!["call 2", "call 2"]);
    }
}
