use landlock::{
    path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    ABI,
};
use omnish_llm::tool::{ToolDef, ToolResult};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

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

/// Manages all registered plugins (official + external).
#[derive(Default)]
pub struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin.
    pub fn register(&mut self, plugin: Box<dyn Plugin>) {
        tracing::info!(
            "Registered plugin '{}' with {} tools",
            plugin.name(),
            plugin.tools().len()
        );
        self.plugins.push(plugin);
    }

    /// Collect all tool definitions from all plugins.
    pub fn all_tools(&self) -> Vec<ToolDef> {
        self.plugins.iter().flat_map(|p| p.tools()).collect()
    }

    /// Get the status text for a tool call from the owning plugin.
    pub fn tool_status_text(&self, tool_name: &str, input: &serde_json::Value) -> String {
        for plugin in &self.plugins {
            if plugin.tools().iter().any(|t| t.name == tool_name) {
                return plugin.status_text(tool_name, input);
            }
        }
        format!("执行 {}...", tool_name)
    }

    /// Collect system prompt fragments from all plugins.
    pub fn all_system_prompts(&self) -> Vec<String> {
        self.plugins
            .iter()
            .filter_map(|p| p.system_prompt())
            .collect()
    }

    /// Find the plugin that owns the given tool name and execute it.
    pub fn call_tool(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult {
        for plugin in &self.plugins {
            if plugin.tools().iter().any(|t| t.name == tool_name) {
                return plugin.call_tool(tool_name, input);
            }
        }
        ToolResult {
            tool_use_id: String::new(),
            content: format!("Unknown tool: {}", tool_name),
            is_error: true,
        }
    }

    /// Return the plugin type that owns the given tool, if any.
    pub fn tool_plugin_type(&self, tool_name: &str) -> Option<PluginType> {
        for plugin in &self.plugins {
            if plugin.tools().iter().any(|t| t.name == tool_name) {
                return Some(plugin.plugin_type());
            }
        }
        None
    }

    /// Load external plugins from ~/.omnish/plugins/ based on enabled list.
    pub fn load_external_plugins(&mut self, enabled: &[String]) {
        let plugins_dir = omnish_common::config::omnish_dir().join("plugins");
        for name in enabled {
            let executable = plugins_dir.join(name).join(name);
            if !executable.exists() {
                tracing::warn!(
                    "Plugin '{}' executable not found at {}",
                    name,
                    executable.display()
                );
                continue;
            }
            if let Some(plugin) = ExternalPlugin::spawn(name, &executable) {
                self.register(Box::new(plugin));
            }
        }
    }
}

// --- JSON-RPC types ---

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
struct InitializeResult {
    #[allow(dead_code)]
    name: String,
    tools: Vec<ToolDef>,
    #[serde(default)]
    plugin_type: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
}

#[derive(Deserialize)]
struct ExecuteResult {
    content: String,
    #[serde(default)]
    is_error: bool,
}

// --- ExternalPlugin ---

pub struct ExternalPlugin {
    plugin_name: String,
    plugin_type: PluginType,
    system_prompt_text: Option<String>,
    stdin: Mutex<std::io::BufWriter<ChildStdin>>,
    stdout: Mutex<BufReader<ChildStdout>>,
    child: Mutex<Child>,
    tool_defs: Vec<ToolDef>,
    next_id: Mutex<u64>,
}

impl ExternalPlugin {
    /// Apply Landlock filesystem sandbox: read everywhere, write only to data_dir and /tmp.
    /// Called inside pre_exec (between fork and exec), so only affects the child process.
    fn apply_sandbox(data_dir: &std::path::Path) -> Result<(), String> {
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
            RulesetStatus::FullyEnforced => Ok(()),
            RulesetStatus::PartiallyEnforced => Ok(()),
            RulesetStatus::NotEnforced => Err("Landlock not supported on this kernel".into()),
        }
    }

    /// Spawn a plugin subprocess with extra arguments and initialize it.
    pub fn spawn_with_args(name: &str, executable: &std::path::Path, args: &[&str]) -> Option<Self> {
        Self::spawn_inner(name, executable, args)
    }

    /// Spawn a plugin subprocess and initialize it.
    /// Returns None if the plugin fails to start or initialize.
    pub fn spawn(name: &str, executable: &std::path::Path) -> Option<Self> {
        Self::spawn_inner(name, executable, &[])
    }

    fn spawn_inner(name: &str, executable: &std::path::Path, args: &[&str]) -> Option<Self> {
        // Create data directory for the plugin
        let data_dir = omnish_common::config::omnish_dir().join("data").join(name);
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            tracing::error!("Failed to create plugin data dir {}: {}", data_dir.display(), e);
            return None;
        }

        let data_dir_clone = data_dir.clone();
        let plugin_name = name.to_string();
        let mut cmd = Command::new(executable);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // SAFETY: pre_exec runs between fork and exec in the child process.
        // We only call Landlock syscalls which are async-signal-safe equivalent.
        unsafe {
            cmd.pre_exec(move || {
                Self::apply_sandbox(&data_dir_clone).map_err(|e| {
                    eprintln!("Plugin '{}' sandbox failed: {}", plugin_name, e);
                    std::io::Error::new(std::io::ErrorKind::PermissionDenied, e)
                })
            });
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Failed to spawn plugin '{}': {}", name, e);
                return None;
            }
        };

        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;

        let mut plugin = Self {
            plugin_name: name.to_string(),
            plugin_type: PluginType::DaemonTool,
            system_prompt_text: None,
            stdin: Mutex::new(std::io::BufWriter::new(stdin)),
            stdout: Mutex::new(BufReader::new(stdout)),
            child: Mutex::new(child),
            tool_defs: vec![],
            next_id: Mutex::new(1),
        };

        // Send initialize request
        match plugin.send_request("initialize", serde_json::json!({})) {
            Ok(result) => match serde_json::from_value::<InitializeResult>(result) {
                Ok(init) => {
                    let ptype = match init.plugin_type.as_deref() {
                        Some("client_tool") => PluginType::ClientTool,
                        _ => PluginType::DaemonTool,
                    };
                    tracing::info!(
                        "Plugin '{}' initialized with {} tools (type={:?})",
                        name,
                        init.tools.len(),
                        ptype,
                    );
                    plugin.tool_defs = init.tools;
                    plugin.plugin_type = ptype;
                    plugin.system_prompt_text = init.system_prompt;
                    Some(plugin)
                }
                Err(e) => {
                    tracing::warn!("Plugin '{}' initialize response invalid: {}", name, e);
                    None
                }
            },
            Err(e) => {
                tracing::warn!("Plugin '{}' initialize failed: {}", name, e);
                None
            }
        }
    }

    fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = {
            let mut next = self.next_id.lock().unwrap();
            let id = *next;
            *next += 1;
            id
        };

        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            method: method.to_string(),
            id,
            params,
        };

        let msg = serde_json::to_string(&req).map_err(|e| e.to_string())?;

        {
            let mut stdin = self.stdin.lock().unwrap();
            writeln!(stdin, "{}", msg).map_err(|e| format!("write to plugin: {}", e))?;
            stdin.flush().map_err(|e| format!("flush to plugin: {}", e))?;
        }

        let mut line = String::new();
        {
            let mut stdout = self.stdout.lock().unwrap();
            stdout
                .read_line(&mut line)
                .map_err(|e| format!("read from plugin: {}", e))?;
        }

        let resp: JsonRpcResponse =
            serde_json::from_str(&line).map_err(|e| format!("parse response: {}", e))?;

        if resp.id != id {
            return Err(format!(
                "response id mismatch: expected {}, got {}",
                id, resp.id
            ));
        }

        if let Some(err) = resp.error {
            return Err(format!("plugin error: {}", err));
        }

        resp.result.ok_or_else(|| "empty result".to_string())
    }

    /// Send shutdown and kill the process.
    pub fn shutdown(&self) {
        // Best-effort shutdown
        let _ = self.send_request("shutdown", serde_json::json!({}));
        std::thread::sleep(std::time::Duration::from_secs(1));
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

impl Plugin for ExternalPlugin {
    fn name(&self) -> &str {
        &self.plugin_name
    }

    fn plugin_type(&self) -> PluginType {
        self.plugin_type
    }

    fn tools(&self) -> Vec<ToolDef> {
        self.tool_defs.clone()
    }

    fn call_tool(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult {
        let params = serde_json::json!({
            "name": tool_name,
            "input": input,
        });

        match self.send_request("tool/execute", params) {
            Ok(result) => match serde_json::from_value::<ExecuteResult>(result) {
                Ok(exec) => ToolResult {
                    tool_use_id: String::new(),
                    content: exec.content,
                    is_error: exec.is_error,
                },
                Err(e) => ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Invalid plugin response: {}", e),
                    is_error: true,
                },
            },
            Err(e) => ToolResult {
                tool_use_id: String::new(),
                content: format!("Plugin error: {}", e),
                is_error: true,
            },
        }
    }

    fn system_prompt(&self) -> Option<String> {
        self.system_prompt_text.clone()
    }

    fn status_text(&self, tool_name: &str, input: &serde_json::Value) -> String {
        let params = serde_json::json!({
            "name": tool_name,
            "input": input,
        });
        match self.send_request("tool/status_text", params) {
            Ok(result) => result.as_str().unwrap_or("").to_string(),
            Err(_) => format!("执行 {}...", tool_name),
        }
    }
}

impl Drop for ExternalPlugin {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockPlugin {
        plugin_name: String,
        tool_defs: Vec<ToolDef>,
    }

    impl MockPlugin {
        fn new(name: &str, tools: Vec<(&str, &str)>) -> Self {
            Self {
                plugin_name: name.to_string(),
                tool_defs: tools
                    .into_iter()
                    .map(|(n, d)| ToolDef {
                        name: n.to_string(),
                        description: d.to_string(),
                        input_schema: serde_json::json!({"type": "object"}),
                    })
                    .collect(),
            }
        }
    }

    impl Plugin for MockPlugin {
        fn name(&self) -> &str {
            &self.plugin_name
        }
        fn tools(&self) -> Vec<ToolDef> {
            self.tool_defs.clone()
        }
        fn call_tool(&self, tool_name: &str, _input: &serde_json::Value) -> ToolResult {
            ToolResult {
                tool_use_id: String::new(),
                content: format!("mock result from {}", tool_name),
                is_error: false,
            }
        }
    }

    #[test]
    fn test_plugin_manager_empty() {
        let mgr = PluginManager::new();
        assert!(mgr.all_tools().is_empty());
    }

    #[test]
    fn test_plugin_manager_register_and_list_tools() {
        let mut mgr = PluginManager::new();
        mgr.register(Box::new(MockPlugin::new(
            "weather",
            vec![("get_weather", "Get weather"), ("get_forecast", "Get forecast")],
        )));
        mgr.register(Box::new(MockPlugin::new(
            "calc",
            vec![("calculate", "Calculate expression")],
        )));
        let tools = mgr.all_tools();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].name, "get_weather");
        assert_eq!(tools[2].name, "calculate");
    }

    #[test]
    fn test_plugin_manager_execute_routes_correctly() {
        let mut mgr = PluginManager::new();
        mgr.register(Box::new(MockPlugin::new(
            "weather",
            vec![("get_weather", "Get weather")],
        )));
        mgr.register(Box::new(MockPlugin::new(
            "calc",
            vec![("calculate", "Calculate")],
        )));

        let result = mgr.call_tool("get_weather", &serde_json::json!({}));
        assert!(!result.is_error);
        assert!(result.content.contains("get_weather"));

        let result = mgr.call_tool("calculate", &serde_json::json!({}));
        assert!(!result.is_error);
        assert!(result.content.contains("calculate"));
    }

    #[test]
    fn test_plugin_manager_unknown_tool() {
        let mgr = PluginManager::new();
        let result = mgr.call_tool("nonexistent", &serde_json::json!({}));
        assert!(result.is_error);
        assert!(result.content.contains("Unknown tool"));
    }
}
