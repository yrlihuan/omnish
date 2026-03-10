//! Lightweight plugin subprocess manager for client-side tool execution.
//! Spawns `omnish-plugin <name>` and communicates via JSON-RPC stdin/stdout.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    method: String,
    id: u64,
    params: serde_json::Value,
}

#[derive(Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    id: u64,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ExecuteResult {
    content: String,
    #[serde(default)]
    is_error: bool,
}

struct PluginProcess {
    stdin: std::io::BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    child: Child,
    next_id: u64,
}

impl PluginProcess {
    fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;

        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            method: method.to_string(),
            id,
            params,
        };

        let msg = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        writeln!(self.stdin, "{}", msg).map_err(|e| format!("write to plugin: {e}"))?;
        self.stdin.flush().map_err(|e| format!("flush to plugin: {e}"))?;

        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .map_err(|e| format!("read from plugin: {e}"))?;

        let resp: JsonRpcResponse =
            serde_json::from_str(&line).map_err(|e| format!("parse response: {e}"))?;

        if resp.id != id {
            return Err(format!("response id mismatch: expected {id}, got {}", resp.id));
        }

        if let Some(err) = resp.error {
            return Err(format!("plugin error: {err}"));
        }

        resp.result.ok_or_else(|| "empty result".to_string())
    }

    fn execute_tool(&mut self, tool_name: &str, input: &serde_json::Value) -> (String, bool) {
        let params = serde_json::json!({
            "name": tool_name,
            "input": input,
        });
        match self.send_request("tool/execute", params) {
            Ok(result) => match serde_json::from_value::<ExecuteResult>(result) {
                Ok(exec) => (exec.content, exec.is_error),
                Err(e) => (format!("Invalid plugin response: {e}"), true),
            },
            Err(e) => (format!("Plugin error: {e}"), true),
        }
    }

    fn shutdown(&mut self) {
        let _ = self.send_request("shutdown", serde_json::json!({}));
        std::thread::sleep(std::time::Duration::from_secs(1));
        let _ = self.child.kill();
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Manages client-side plugin subprocesses.
/// Spawns `omnish-plugin <name>` on first use and reuses the long-running process.
pub struct ClientPluginManager {
    plugin_bin: std::path::PathBuf,
    processes: Mutex<HashMap<String, PluginProcess>>,
}

impl ClientPluginManager {
    pub fn new() -> Self {
        let plugin_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("omnish-plugin")))
            .unwrap_or_else(|| std::path::PathBuf::from("omnish-plugin"));
        Self {
            plugin_bin,
            processes: Mutex::new(HashMap::new()),
        }
    }

    /// Execute a tool via the plugin subprocess. Spawns the process on first call.
    pub fn execute_tool(&self, tool_name: &str, input: &serde_json::Value) -> (String, bool) {
        // Map tool name to plugin name (for now, all known tools → their plugin)
        let plugin_name = match tool_name {
            "bash" => "bash",
            _ => return (format!("Unknown client tool: {tool_name}"), true),
        };

        let mut processes = self.processes.lock().unwrap();
        let proc = processes.entry(plugin_name.to_string()).or_insert_with(|| {
            match Self::spawn_plugin(&self.plugin_bin, plugin_name) {
                Some(p) => p,
                None => {
                    // Return a dummy that will produce errors
                    panic!("Failed to spawn omnish-plugin {plugin_name}");
                }
            }
        });
        proc.execute_tool(tool_name, input)
    }

    fn spawn_plugin(bin: &std::path::Path, name: &str) -> Option<PluginProcess> {
        let mut child = Command::new(bin)
            .arg(name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .ok()?;

        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;

        let mut proc = PluginProcess {
            stdin: std::io::BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            child,
            next_id: 1,
        };

        // Send initialize to verify it works
        match proc.send_request("initialize", serde_json::json!({})) {
            Ok(_) => Some(proc),
            Err(e) => {
                eprintln!("Failed to initialize plugin '{name}': {e}");
                None
            }
        }
    }
}
