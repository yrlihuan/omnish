use omnish_llm::tool::{ToolDef, ToolResult};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

/// Unified plugin interface for both official (inline) and external (subprocess) plugins.
pub trait Plugin: Send + Sync {
    /// Plugin name (for logging and identification).
    fn name(&self) -> &str;
    /// Tool definitions this plugin provides (sent to LLM).
    fn tools(&self) -> Vec<ToolDef>;
    /// Execute a tool by name with the given input.
    fn call_tool(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult;
}

/// Manages all registered plugins (official + external).
pub struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self { plugins: vec![] }
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
    stdin: Mutex<std::io::BufWriter<ChildStdin>>,
    stdout: Mutex<BufReader<ChildStdout>>,
    child: Mutex<Child>,
    tool_defs: Vec<ToolDef>,
    next_id: Mutex<u64>,
}

impl ExternalPlugin {
    /// Spawn a plugin subprocess and initialize it.
    /// Returns None if the plugin fails to start or initialize.
    pub fn spawn(name: &str, executable: &std::path::Path) -> Option<Self> {
        let mut child = match Command::new(executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to spawn plugin '{}': {}", name, e);
                return None;
            }
        };

        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;

        let mut plugin = Self {
            plugin_name: name.to_string(),
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
                    tracing::info!(
                        "Plugin '{}' initialized with {} tools",
                        name,
                        init.tools.len()
                    );
                    plugin.tool_defs = init.tools;
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
