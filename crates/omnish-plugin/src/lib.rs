//! Shared plugin infrastructure: trait, subprocess spawning, Landlock sandbox, JSON-RPC.
//!
//! This crate provides:
//! - `Plugin` trait and `PluginType` enum (the unified plugin interface)
//! - `PluginProcess` (spawn and communicate with plugin subprocesses)
//! - `BashTool` (built-in bash tool plugin)
//! - JSON-RPC types and Landlock sandbox shared by spawner and subprocess sides

pub mod tools;

#[cfg(target_os = "linux")]
use landlock::{
    path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    ABI,
};
use omnish_llm::tool::{ToolDef, ToolResult};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

// --- Plugin trait ---

/// Classifies whether a plugin's tools run on the daemon or the client side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    DaemonTool,
    ClientTool,
}

/// Unified plugin interface for both official (inline) and external (subprocess) plugins.
pub trait Plugin: Send + Sync {
    /// Plugin name (for logging and identification).
    fn name(&self) -> &str;
    /// Where this plugin's tools execute. Defaults to `DaemonTool`.
    fn plugin_type(&self) -> PluginType {
        PluginType::DaemonTool
    }
    /// Tool definitions this plugin provides (sent to LLM).
    fn tools(&self) -> Vec<ToolDef>;
    /// Execute a tool by name with the given input.
    fn call_tool(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult;
    /// System prompt fragment to be merged into the LLM system prompt.
    fn system_prompt(&self) -> Option<String> {
        None
    }
    /// Status text shown to the user while a tool call is executing.
    fn status_text(&self, tool_name: &str, _input: &serde_json::Value) -> String {
        format!("执行 {}...", tool_name)
    }
}

// --- JSON-RPC types (used by both spawner and subprocess sides) ---

#[derive(Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub id: u64,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub struct ExecuteResult {
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

// --- Landlock sandbox (Linux only) ---

/// Apply Landlock filesystem sandbox: read everywhere, write only to `data_dir` and `/tmp`.
/// Called inside `pre_exec` (between fork and exec), so only affects the child process.
#[cfg(target_os = "linux")]
pub fn apply_sandbox(data_dir: &std::path::Path) -> Result<(), String> {
    let abi = ABI::V1;
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(|e| format!("landlock handle_access: {e}"))?
        .create()
        .map_err(|e| format!("landlock create: {e}"))?
        .add_rules(path_beneath_rules(&["/"], AccessFs::from_read(abi)))
        .map_err(|e| format!("landlock add read rules: {e}"))?
        .add_rules(path_beneath_rules(
            &[data_dir, std::path::Path::new("/tmp")],
            AccessFs::from_all(abi),
        ))
        .map_err(|e| format!("landlock add write rules: {e}"))?
        .restrict_self()
        .map_err(|e| format!("landlock restrict_self: {e}"))?;
    match status.ruleset {
        RulesetStatus::FullyEnforced | RulesetStatus::PartiallyEnforced => Ok(()),
        RulesetStatus::NotEnforced => Err("Landlock not supported on this kernel".into()),
    }
}

/// No-op sandbox on non-Linux platforms.
#[cfg(not(target_os = "linux"))]
pub fn apply_sandbox(_data_dir: &std::path::Path) -> Result<(), String> {
    Ok(())
}

// --- Plugin process ---

/// A spawned plugin subprocess with JSON-RPC communication over stdin/stdout.
pub struct PluginProcess {
    stdin: std::io::BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    child: Child,
    next_id: u64,
}

impl PluginProcess {
    /// Spawn a plugin subprocess with Landlock sandbox and `prctl(PR_SET_NAME)`.
    ///
    /// - `executable`: path to the plugin binary
    /// - `args`: extra command-line arguments
    /// - `name`: plugin name (used for process name and error messages)
    /// - `data_dir`: writable directory for the plugin (also used for sandbox scope)
    pub fn spawn(
        executable: &std::path::Path,
        args: &[&str],
        name: &str,
        data_dir: &std::path::Path,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| format!("create plugin data dir {}: {e}", data_dir.display()))?;

        let data_dir_clone = data_dir.to_path_buf();
        let plugin_name = name.to_string();
        #[cfg(target_os = "linux")]
        let process_name = format!("omnish-plugin({})", name);
        let mut cmd = Command::new(executable);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        // SAFETY: pre_exec runs between fork and exec in the child process.
        // We only call Landlock syscalls and prctl which are async-signal-safe equivalent.
        unsafe {
            cmd.pre_exec(move || {
                apply_sandbox(&data_dir_clone).map_err(|e| {
                    eprintln!("Plugin '{}' sandbox failed: {}", plugin_name, e);
                    std::io::Error::new(std::io::ErrorKind::PermissionDenied, e)
                })?;
                #[cfg(target_os = "linux")]
                {
                    let name_bytes = process_name.as_bytes();
                    let name_ptr = name_bytes.as_ptr() as *const libc::c_char;
                    libc::prctl(libc::PR_SET_NAME, name_ptr, 0, 0, 0);
                }
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn plugin '{}': {e}", name))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;

        Ok(Self {
            stdin: std::io::BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            child,
            next_id: 1,
        })
    }

    /// Send a JSON-RPC request and wait for the response.
    pub fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;

        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
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
            return Err(format!(
                "response id mismatch: expected {id}, got {}",
                resp.id
            ));
        }

        if let Some(err) = resp.error {
            return Err(format!("plugin error: {err}"));
        }

        resp.result.ok_or_else(|| "empty result".to_string())
    }

    /// Execute a tool via the plugin subprocess.
    pub fn execute_tool(&mut self, tool_name: &str, input: &serde_json::Value) -> (String, bool) {
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

    /// Send shutdown request and kill the process.
    pub fn shutdown(&mut self) {
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
