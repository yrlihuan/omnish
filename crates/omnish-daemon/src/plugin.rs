use omnish_llm::tool::{ToolDef, ToolResult};
use omnish_plugin::PluginProcess;
use serde::Deserialize;
use std::sync::Mutex;

// Re-export Plugin trait and PluginType from omnish-plugin for backward compatibility.
pub use omnish_plugin::{Plugin, PluginType};

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
    /// Names starting with "builtin." are reserved for built-in plugins and skipped.
    pub fn load_external_plugins(&mut self, enabled: &[String]) {
        let plugins_dir = omnish_common::config::omnish_dir().join("plugins");
        for name in enabled {
            if name.starts_with("builtin.") {
                tracing::warn!("Skipping reserved plugin name '{}' (builtin.* is reserved)", name);
                continue;
            }
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

// --- ExternalPlugin ---

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

pub struct ExternalPlugin {
    plugin_name: String,
    plugin_type: PluginType,
    system_prompt_text: Option<String>,
    process: Mutex<PluginProcess>,
    tool_defs: Vec<ToolDef>,
}

impl ExternalPlugin {
    /// Spawn a built-in plugin subprocess with extra arguments.
    /// Uses `builtin.<name>` for data and prompt directories.
    pub fn spawn_builtin(name: &str, executable: &std::path::Path, args: &[&str]) -> Option<Self> {
        Self::spawn_inner(name, executable, args, true)
    }

    /// Spawn an external plugin subprocess and initialize it.
    /// Returns None if the plugin fails to start or initialize.
    pub fn spawn(name: &str, executable: &std::path::Path) -> Option<Self> {
        Self::spawn_inner(name, executable, &[], false)
    }

    /// Load customized prompts from the plugin's directory under `~/.omnish/plugins/`.
    /// - `PROMPT.md` replaces the built-in system_prompt entirely.
    /// - `PROMPT_<text>.md` files are appended as extra fragments.
    fn load_custom_prompts(dir_name: &str, builtin_prompt: Option<String>) -> Option<String> {
        let prompt_dir = omnish_common::config::omnish_dir().join("plugins").join(dir_name);
        if !prompt_dir.is_dir() {
            return builtin_prompt;
        }

        // Check for PROMPT.md (replaces builtin)
        let main_prompt_path = prompt_dir.join("PROMPT.md");
        let base = if main_prompt_path.is_file() {
            match std::fs::read_to_string(&main_prompt_path) {
                Ok(content) => {
                    let content = content.trim().to_string();
                    if content.is_empty() {
                        builtin_prompt
                    } else {
                        tracing::info!("Plugin '{}': loaded custom PROMPT.md", dir_name);
                        Some(content)
                    }
                }
                Err(e) => {
                    tracing::warn!("Plugin '{}': failed to read PROMPT.md: {}", dir_name, e);
                    builtin_prompt
                }
            }
        } else {
            builtin_prompt
        };

        // Collect PROMPT_*.md files (appended as extra fragments)
        let mut extras = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&prompt_dir) {
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let fname = fname.to_string_lossy();
                if fname.starts_with("PROMPT_") && fname.ends_with(".md") && entry.path().is_file()
                {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        let content = content.trim().to_string();
                        if !content.is_empty() {
                            tracing::info!("Plugin '{}': loaded custom {}", dir_name, fname);
                            extras.push(content);
                        }
                    }
                }
            }
        }
        extras.sort(); // deterministic order by filename

        if extras.is_empty() {
            return base;
        }

        let combined = match base {
            Some(b) => {
                let mut parts = vec![b];
                parts.extend(extras);
                parts.join("\n\n")
            }
            None => extras.join("\n\n"),
        };
        Some(combined)
    }

    fn spawn_inner(name: &str, executable: &std::path::Path, args: &[&str], builtin: bool) -> Option<Self> {
        let dir_name = if builtin {
            format!("builtin.{}", name)
        } else {
            name.to_string()
        };
        let data_dir = omnish_common::config::omnish_dir().join("data").join(&dir_name);

        let mut process = match PluginProcess::spawn(executable, args, name, &data_dir) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Failed to spawn plugin '{}': {}", name, e);
                return None;
            }
        };

        // Send initialize request
        match process.send_request("initialize", serde_json::json!({})) {
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
                    let system_prompt_text =
                        Self::load_custom_prompts(&dir_name, init.system_prompt);
                    Some(Self {
                        plugin_name: name.to_string(),
                        plugin_type: ptype,
                        system_prompt_text,
                        process: Mutex::new(process),
                        tool_defs: init.tools,
                    })
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
        let mut proc = self.process.lock().unwrap();
        let (content, is_error) = proc.execute_tool(tool_name, input);
        ToolResult {
            tool_use_id: String::new(),
            content,
            is_error,
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
        let mut proc = self.process.lock().unwrap();
        match proc.send_request("tool/status_text", params) {
            Ok(result) => result.as_str().unwrap_or("").to_string(),
            Err(_) => format!("执行 {}...", tool_name),
        }
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
