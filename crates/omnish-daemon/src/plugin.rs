use omnish_llm::tool::{ToolDef, ToolResult};

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
}
