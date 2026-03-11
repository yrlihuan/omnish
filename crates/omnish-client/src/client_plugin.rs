//! Lightweight plugin subprocess manager for client-side tool execution.
//! Spawns `omnish-plugin <name>` and communicates via JSON-RPC stdin/stdout.

use omnish_plugin::PluginProcess;
use std::collections::HashMap;
use std::sync::Mutex;

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
            "edit" => "edit",
            "read" => "read",
            "write" => "write",
            _ => return (format!("Unknown client tool: {tool_name}"), true),
        };

        let mut processes = self.processes.lock().unwrap();
        let proc = processes.entry(plugin_name.to_string()).or_insert_with(|| {
            let dir_name = format!("builtin.{}", plugin_name);
            let data_dir = omnish_common::config::omnish_dir()
                .join("data")
                .join(&dir_name);
            match PluginProcess::spawn(&self.plugin_bin, &[plugin_name], plugin_name, &data_dir) {
                Ok(mut p) => {
                    // Send initialize to verify it works
                    match p.send_request("initialize", serde_json::json!({})) {
                        Ok(_) => p,
                        Err(e) => panic!("Failed to initialize plugin '{plugin_name}': {e}"),
                    }
                }
                Err(e) => panic!("Failed to spawn omnish-plugin {plugin_name}: {e}"),
            }
        });
        proc.execute_tool(tool_name, input)
    }
}
