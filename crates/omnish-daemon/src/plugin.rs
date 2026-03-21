use omnish_llm::tool::ToolDef;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Classifies whether a plugin's tools run on the daemon or the client side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    DaemonTool,
    ClientTool,
}

/// A single tool entry parsed from tool.json (base definition, immutable).
#[derive(Debug, Clone)]
struct ToolEntry {
    def: ToolDef,
    status_template: String,
    display_name: String,
    formatter: String,
}

/// A plugin loaded from a tool.json file.
#[derive(Debug)]
struct PluginInfo {
    dir_name: String,
    plugin_type: PluginType,
    tools: Vec<ToolEntry>,
}

/// Cached tool.override.json overrides, updated on file changes.
struct PromptCache {
    /// tool_name → effective description (base with override/append applied)
    descriptions: HashMap<String, String>,
    /// tool_name → override params from tool.override.json
    override_params: HashMap<String, HashMap<String, serde_json::Value>>,
}

/// Metadata-only plugin manager. Loads tool definitions from JSON files.
/// Watches tool.override.json files for changes via inotify/polling.
pub struct PluginManager {
    plugins_dir: PathBuf,
    plugins: Vec<PluginInfo>,
    /// Maps tool_name → (plugin_index, tool_index) for fast lookup.
    tool_index: HashMap<String, (usize, usize)>,
    /// Prompt overrides, updated on file changes.
    prompt_cache: RwLock<PromptCache>,
}

#[derive(Deserialize)]
struct ToolJsonFile {
    plugin_type: String,
    tools: Vec<ToolJsonEntry>,
}

#[derive(Deserialize)]
struct ToolJsonEntry {
    name: String,
    /// Accepts either a single string or an array of strings (joined with "\n").
    description: DescriptionValue,
    input_schema: serde_json::Value,
    #[serde(default)]
    status_template: String,
    /// Ignored — all tools are sandboxed. Kept for backwards compatibility with existing tool.json files.
    #[serde(default)]
    #[allow(dead_code)]
    sandboxed: bool,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    formatter: Option<String>,
}

/// Description can be a plain string or an array of lines for readability.
#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum DescriptionValue {
    Single(String),
    Lines(Vec<String>),
}

impl DescriptionValue {
    fn into_string(self) -> String {
        match self {
            Self::Single(s) => s,
            Self::Lines(lines) => lines.join("\n"),
        }
    }
}

/// tool.override.json: user-specified overrides for tool descriptions.
#[derive(Deserialize)]
struct ToolOverrideFile {
    #[serde(default)]
    tools: HashMap<String, ToolOverrideEntry>,
}

#[derive(Deserialize)]
struct ToolOverrideEntry {
    /// Replaces the tool description entirely.
    #[serde(default)]
    description: Option<DescriptionValue>,
    /// Appended to the tool description (ignored if `description` is set).
    #[serde(default)]
    append: Option<DescriptionValue>,
    /// Extra parameters merged into tool call input at execution time.
    #[serde(default)]
    params: Option<HashMap<String, serde_json::Value>>,
}

/// Built-in tool definitions embedded at compile time.
/// Guarantees tools are always available even without on-disk assets.
const BUILTIN_TOOL_JSON: &str = include_str!("../../omnish-plugin/assets/tool.json");

impl PluginManager {
    /// Load all plugins from the given directory.
    /// Each subdirectory containing a `tool.json` is treated as a plugin.
    /// Built-in tools are always loaded from embedded data if not found on disk.
    pub fn load(plugins_dir: &Path) -> Self {
        let mut plugins = Vec::new();
        let mut tool_index = HashMap::new();

        let mut entries: Vec<_> = match std::fs::read_dir(plugins_dir) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
            Err(_) => Vec::new(),
        };
        entries.sort_by_key(|e| e.file_name());

        // Always load built-in tools from embedded data
        match serde_json::from_str::<ToolJsonFile>(BUILTIN_TOOL_JSON) {
            Ok(parsed) => {
                let plugin_type = match parsed.plugin_type.as_str() {
                    "client_tool" => PluginType::ClientTool,
                    _ => PluginType::DaemonTool,
                };
                let plugin_idx = plugins.len();
                let mut tools = Vec::new();
                for te in parsed.tools {
                    let tool_idx = tools.len();
                    tool_index.insert(te.name.clone(), (plugin_idx, tool_idx));
                    let display_name = te.display_name.clone().unwrap_or_else(|| te.name.clone());
                    let formatter = te.formatter.clone().unwrap_or_else(|| "default".to_string());
                    tools.push(ToolEntry {
                        def: ToolDef {
                            name: te.name,
                            description: te.description.into_string(),
                            input_schema: te.input_schema,
                        },
                        status_template: te.status_template,
                        display_name,
                        formatter,
                    });
                }
                tracing::info!("Loaded builtin plugin with {} tools", tools.len());
                plugins.push(PluginInfo {
                    dir_name: "builtin".to_string(),
                    plugin_type,
                    tools,
                });
            }
            Err(e) => {
                tracing::error!("Failed to parse embedded builtin tool.json: {}", e);
            }
        }

        for entry in entries {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().to_string();
            // Skip "builtin" — always loaded from embedded data above
            if dir_name == "builtin" {
                continue;
            }
            let tool_json = path.join("tool.json");
            if !tool_json.is_file() {
                continue;
            }
            let content = match std::fs::read_to_string(&tool_json) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to read {}: {}", tool_json.display(), e);
                    continue;
                }
            };
            let parsed: ToolJsonFile = match serde_json::from_str(&content) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Malformed {}: {}", tool_json.display(), e);
                    continue;
                }
            };

            let plugin_type = match parsed.plugin_type.as_str() {
                "client_tool" => PluginType::ClientTool,
                _ => PluginType::DaemonTool,
            };

            let plugin_idx = plugins.len();
            let mut tools = Vec::new();
            for te in parsed.tools {
                if tool_index.contains_key(&te.name) {
                    tracing::warn!(
                        "Duplicate tool name '{}' in {}, skipping",
                        te.name,
                        tool_json.display()
                    );
                    continue;
                }
                let tool_idx = tools.len();
                tool_index.insert(te.name.clone(), (plugin_idx, tool_idx));
                let display_name = te.display_name.clone().unwrap_or_else(|| te.name.clone());
                let formatter = te.formatter.clone().unwrap_or_else(|| "default".to_string());
                tools.push(ToolEntry {
                    def: ToolDef {
                        name: te.name,
                        description: te.description.into_string(),
                        input_schema: te.input_schema,
                    },
                    status_template: te.status_template,
                    display_name,
                    formatter,
                });
            }

            tracing::info!(
                "Loaded plugin '{}' with {} tools",
                dir_name,
                tools.len()
            );
            plugins.push(PluginInfo {
                dir_name,
                plugin_type,
                tools,
            });
        }

        let mgr = Self {
            plugins_dir: plugins_dir.to_path_buf(),
            plugins,
            tool_index,
            prompt_cache: RwLock::new(PromptCache {
                descriptions: HashMap::new(),
                override_params: HashMap::new(),
            }),
        };
        mgr.reload_overrides();
        mgr
    }

    /// Re-read all tool.override.json files and update the prompt cache.
    /// Returns the computed (descriptions, override_params) so callers can propagate them.
    pub fn reload_overrides(&self) -> (HashMap<String, String>, HashMap<String, HashMap<String, serde_json::Value>>) {
        let mut descriptions = HashMap::new();
        let mut override_params = HashMap::new();

        for plugin in &self.plugins {
            let override_path = self.plugins_dir.join(&plugin.dir_name).join("tool.override.json");
            let overrides = if override_path.is_file() {
                match std::fs::read_to_string(&override_path) {
                    Ok(c) => match serde_json::from_str::<ToolOverrideFile>(&c) {
                        Ok(p) => Some(p),
                        Err(e) => {
                            tracing::warn!("Malformed {}: {}", override_path.display(), e);
                            None
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Failed to read {}: {}", override_path.display(), e);
                        None
                    }
                }
            } else {
                None
            };

            for te in &plugin.tools {
                let mut desc = te.def.description.clone();
                if let Some(ref of_) = overrides {
                    if let Some(ovr) = of_.tools.get(&te.def.name) {
                        if let Some(ref d) = ovr.description {
                            desc = d.clone().into_string();
                        } else if let Some(ref a) = ovr.append {
                            desc.push('\n');
                            desc.push_str(&a.clone().into_string());
                        }
                        if let Some(ref p) = ovr.params {
                            override_params.insert(te.def.name.clone(), p.clone());
                        }
                    }
                }
                descriptions.insert(te.def.name.clone(), desc);
            }
        }

        tracing::info!("Reloaded tool overrides ({} tools)", descriptions.len());

        let mut cache = self.prompt_cache.write().unwrap();
        cache.descriptions = descriptions.clone();
        cache.override_params = override_params.clone();
        (descriptions, override_params)
    }

    /// Return the executable path for the plugin that owns the given tool.
    pub fn plugin_executable(&self, tool_name: &str) -> Option<std::path::PathBuf> {
        self.tool_index.get(tool_name).map(|&(pi, _)| {
            let dir_name = &self.plugins[pi].dir_name;
            self.plugins_dir.join(dir_name).join(dir_name)
        })
    }

    /// Register all plugin tools into a ToolRegistry.
    /// Includes tool metadata (display_name, formatter, status_template)
    /// and tool definitions (for LLM prompt construction).
    pub fn register_all(&self, registry: &mut crate::tool_registry::ToolRegistry) {
        for plugin in &self.plugins {
            for te in &plugin.tools {
                registry.register(crate::tool_registry::ToolMeta {
                    name: te.def.name.clone(),
                    display_name: te.display_name.clone(),
                    formatter: te.formatter.clone(),
                    status_template: te.status_template.clone(),
                    custom_status: None,
                    plugin_type: Some(plugin.plugin_type),
                    plugin_name: Some(plugin.dir_name.clone()),
                });
                registry.register_def(te.def.clone());
            }
        }
        // Apply current overrides
        let cache = self.prompt_cache.read().unwrap();
        registry.update_overrides(cache.descriptions.clone(), cache.override_params.clone());
    }

    /// Start watching plugin overrides using a shared file watcher receiver.
    pub async fn watch_with(self: &Arc<Self>, mut rx: tokio::sync::watch::Receiver<()>, registry: std::sync::Arc<crate::tool_registry::ToolRegistry>) {
        tracing::info!("watching plugin overrides via shared file watcher: {}", self.plugins_dir.display());
        while rx.changed().await.is_ok() {
            tracing::info!("tool.override.json changed, reloading...");
            let (descs, params) = self.reload_overrides();
            registry.update_overrides(descs, params);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tool_json(dir: &std::path::Path, name: &str, content: &str) {
        let plugin_dir = dir.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let mut f = std::fs::File::create(plugin_dir.join("tool.json")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn write_tool_override(dir: &std::path::Path, name: &str, content: &str) {
        let plugin_dir = dir.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let mut f = std::fs::File::create(plugin_dir.join("tool.override.json")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    /// Number of tools embedded in BUILTIN_TOOL_JSON.
    const BUILTIN_COUNT: usize = 6;

    /// Helper: register all tools from a PluginManager and return the count of defs.
    fn count_registered_defs(mgr: &PluginManager) -> usize {
        use crate::tool_registry::ToolRegistry;
        let mut reg = ToolRegistry::new();
        mgr.register_all(&mut reg);
        reg.all_defs().len()
    }

    /// Helper: get description for a tool via ToolRegistry (after register_all + overrides).
    fn get_description(mgr: &PluginManager, name: &str) -> String {
        use crate::tool_registry::ToolRegistry;
        let mut reg = ToolRegistry::new();
        mgr.register_all(&mut reg);
        reg.all_defs().into_iter().find(|t| t.name == name).unwrap().description
    }

    #[test]
    fn test_load_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = PluginManager::load(tmp.path());
        // Only embedded builtin tools
        assert_eq!(count_registered_defs(&mgr), BUILTIN_COUNT);
    }

    #[test]
    fn test_load_single_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "myplugin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "my_tool",
                "description": "My tool",
                "input_schema": {"type": "object", "properties": {}, "required": []},
                "status_template": "run: {arg}",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        let mut reg = crate::tool_registry::ToolRegistry::new();
        mgr.register_all(&mut reg);
        let defs = reg.all_defs();
        assert_eq!(defs.len(), BUILTIN_COUNT + 1);
        assert!(defs.iter().any(|t| t.name == "my_tool"));
    }

    #[test]
    fn test_malformed_json_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "bad", "not json{{{");
        write_tool_json(tmp.path(), "good", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "custom_read",
                "description": "Read",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        let mut reg = crate::tool_registry::ToolRegistry::new();
        mgr.register_all(&mut reg);
        let defs = reg.all_defs();
        assert_eq!(defs.len(), BUILTIN_COUNT + 1);
        assert!(defs.iter().any(|t| t.name == "custom_read"));
    }

    #[test]
    fn test_duplicate_tool_name_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "plugin_a", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "dup_tool",
                "description": "First",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        write_tool_json(tmp.path(), "plugin_b", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "dup_tool",
                "description": "Duplicate",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        // BUILTIN_COUNT + 1 (only first dup_tool loaded, second skipped)
        assert_eq!(count_registered_defs(&mgr), BUILTIN_COUNT + 1);
    }

    #[test]
    fn test_prompt_json_replace_description() {
        let tmp = tempfile::tempdir().unwrap();
        // Override builtin "bash" description via tool.override.json
        write_tool_override(tmp.path(), "builtin", r#"{
            "tools": {
                "bash": {
                    "description": "Custom description"
                }
            }
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(get_description(&mgr, "bash"), "Custom description");
    }

    #[test]
    fn test_prompt_json_append_description() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_override(tmp.path(), "builtin", r#"{
            "tools": {
                "bash": {
                    "append": "Extra guideline"
                }
            }
        }"#);
        let mgr = PluginManager::load(tmp.path());
        let desc = get_description(&mgr, "bash");
        assert!(desc.ends_with("\nExtra guideline"));
    }

    #[test]
    fn test_prompt_json_description_takes_priority_over_append() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_override(tmp.path(), "builtin", r#"{
            "tools": {
                "bash": {
                    "description": "Replaced",
                    "append": "Should be ignored"
                }
            }
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(get_description(&mgr, "bash"), "Replaced");
    }

    #[test]
    fn test_prompt_json_multiline_description() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_override(tmp.path(), "builtin", r#"{
            "tools": {
                "bash": {
                    "description": ["Line 1", "Line 2", "", "Line 4"]
                }
            }
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(get_description(&mgr, "bash"), "Line 1\nLine 2\n\nLine 4");
    }

    #[test]
    fn test_no_prompt_json_keeps_original() {
        let tmp = tempfile::tempdir().unwrap();
        // No override file — embedded description is used
        let mgr = PluginManager::load(tmp.path());
        let desc = get_description(&mgr, "bash");
        assert!(desc.contains("bash"));  // embedded description mentions bash
    }

    #[test]
    fn test_plugin_executable() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "web_search", r#"{
            "plugin_type": "daemon_tool",
            "tools": [{
                "name": "web_search",
                "description": "Search",
                "input_schema": {"type": "object"},
                "status_template": ""
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        let exe = mgr.plugin_executable("web_search").unwrap();
        assert_eq!(exe, tmp.path().join("web_search").join("web_search"));
    }

    #[test]
    fn test_merge_precedence() {
        let mut input = serde_json::json!({"query": "test", "count": 3});
        let override_params: HashMap<String, serde_json::Value> = [
            ("count".to_string(), serde_json::json!(5)),
            ("api_key".to_string(), serde_json::json!("override_key")),
        ].into();
        let config_params: HashMap<String, serde_json::Value> = [
            ("api_key".to_string(), serde_json::json!("config_key")),
        ].into();

        if let Some(obj) = input.as_object_mut() {
            for (k, v) in &override_params { obj.insert(k.clone(), v.clone()); }
        }
        if let Some(obj) = input.as_object_mut() {
            for (k, v) in &config_params { obj.insert(k.clone(), v.clone()); }
        }

        assert_eq!(input["query"], "test");
        assert_eq!(input["count"], 5);
        assert_eq!(input["api_key"], "config_key");
    }

    #[test]
    fn test_reload_overrides_picks_up_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = PluginManager::load(tmp.path());
        let original = get_description(&mgr, "bash");

        // Write tool.override.json and reload
        write_tool_override(tmp.path(), "builtin", r#"{
            "tools": { "bash": { "description": "Updated" } }
        }"#);
        mgr.reload_overrides();
        assert_eq!(get_description(&mgr, "bash"), "Updated");
        assert_ne!(original, "Updated");

        // Remove tool.override.json override by writing empty overrides
        write_tool_override(tmp.path(), "builtin", r#"{ "tools": {} }"#);
        mgr.reload_overrides();
        assert_eq!(get_description(&mgr, "bash"), original);
    }

    #[test]
    fn test_register_all_populates_registry() {
        use crate::tool_registry::ToolRegistry;
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "myplugin", r#"{
            "plugin_type": "daemon_tool",
            "tools": [{
                "name": "my_tool",
                "description": "My tool",
                "input_schema": {"type": "object"},
                "status_template": "run: {arg}",
                "display_name": "MyTool",
                "formatter": "default"
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        let mut reg = ToolRegistry::new();
        mgr.register_all(&mut reg);
        // Custom plugin tool
        assert_eq!(reg.display_name("my_tool"), "MyTool");
        assert_eq!(reg.formatter_name("my_tool"), "default");
        // Built-in tool (from embedded tool.json — "bash" has display_name "Bash")
        assert_eq!(reg.display_name("bash"), "Bash");
        // Definitions should be registered
        let defs = reg.all_defs();
        assert!(defs.iter().any(|d| d.name == "bash"));
        assert!(defs.iter().any(|d| d.name == "my_tool"));
    }
}
