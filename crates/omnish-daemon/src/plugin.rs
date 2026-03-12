use omnish_llm::tool::ToolDef;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Classifies whether a plugin's tools run on the daemon or the client side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    DaemonTool,
    ClientTool,
}

/// A single tool entry parsed from tool.json.
#[derive(Debug, Clone)]
struct ToolEntry {
    def: ToolDef,
    status_template: String,
    sandboxed: bool,
}

/// A plugin loaded from a tool.json file.
#[derive(Debug)]
struct PluginInfo {
    dir_name: String,
    plugin_type: PluginType,
    tools: Vec<ToolEntry>,
}

/// Metadata-only plugin manager. Loads tool definitions from JSON files.
/// Does not spawn or manage any processes.
pub struct PluginManager {
    plugins: Vec<PluginInfo>,
    /// Maps tool_name → (plugin_index, tool_index) for fast lookup.
    tool_index: HashMap<String, (usize, usize)>,
    /// Extra system prompt fragments from prompt.json files.
    system_prompts: Vec<String>,
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
    #[serde(default = "default_sandboxed")]
    sandboxed: bool,
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

fn default_sandboxed() -> bool {
    true
}

/// prompt.json: user-specified overrides for tool descriptions and system prompt.
#[derive(Deserialize)]
struct PromptJsonFile {
    #[serde(default)]
    system_prompt: Option<DescriptionValue>,
    #[serde(default)]
    tools: HashMap<String, PromptToolOverride>,
}

#[derive(Deserialize)]
struct PromptToolOverride {
    /// Replaces the tool description entirely.
    #[serde(default)]
    description: Option<DescriptionValue>,
    /// Appended to the tool description (ignored if `description` is set).
    #[serde(default)]
    append: Option<DescriptionValue>,
}

impl PluginManager {
    /// Load all plugins from the given directory.
    /// Each subdirectory containing a `tool.json` is treated as a plugin.
    pub fn load(plugins_dir: &Path) -> Self {
        let mut plugins = Vec::new();
        let mut tool_index = HashMap::new();
        let mut system_prompts = Vec::new();

        let mut entries: Vec<_> = match std::fs::read_dir(plugins_dir) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
            Err(_) => Vec::new(),
        };
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let tool_json = path.join("tool.json");
            if !tool_json.is_file() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().to_string();
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

            // Load prompt.json overrides (optional)
            let prompt_overrides = {
                let prompt_json = path.join("prompt.json");
                if prompt_json.is_file() {
                    match std::fs::read_to_string(&prompt_json) {
                        Ok(c) => match serde_json::from_str::<PromptJsonFile>(&c) {
                            Ok(p) => {
                                tracing::info!("Loaded prompt.json for plugin '{}'", dir_name);
                                Some(p)
                            }
                            Err(e) => {
                                tracing::warn!("Malformed {}: {}", prompt_json.display(), e);
                                None
                            }
                        },
                        Err(e) => {
                            tracing::warn!("Failed to read {}: {}", prompt_json.display(), e);
                            None
                        }
                    }
                } else {
                    None
                }
            };

            // Collect system_prompt from prompt.json
            if let Some(ref pf) = prompt_overrides {
                if let Some(ref sp) = pf.system_prompt {
                    system_prompts.push(sp.clone().into_string());
                }
            }

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

                // Apply prompt.json overrides to description
                let mut description = te.description.into_string();
                if let Some(ref pf) = prompt_overrides {
                    if let Some(ovr) = pf.tools.get(&te.name) {
                        if let Some(ref desc) = ovr.description {
                            description = desc.clone().into_string();
                        } else if let Some(ref append) = ovr.append {
                            description.push('\n');
                            description.push_str(&append.clone().into_string());
                        }
                    }
                }

                let tool_idx = tools.len();
                tool_index.insert(te.name.clone(), (plugin_idx, tool_idx));
                tools.push(ToolEntry {
                    def: ToolDef {
                        name: te.name,
                        description,
                        input_schema: te.input_schema,
                    },
                    status_template: te.status_template,
                    sandboxed: te.sandboxed,
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

        Self {
            plugins,
            tool_index,
            system_prompts,
        }
    }

    /// Collect all tool definitions from all plugins.
    pub fn all_tools(&self) -> Vec<ToolDef> {
        self.plugins
            .iter()
            .flat_map(|p| p.tools.iter().map(|t| t.def.clone()))
            .collect()
    }

    /// Get the status text for a tool call, interpolating {field} from input.
    pub fn tool_status_text(&self, tool_name: &str, input: &serde_json::Value) -> String {
        if let Some(&(pi, ti)) = self.tool_index.get(tool_name) {
            let template = &self.plugins[pi].tools[ti].status_template;
            interpolate_template(template, input)
        } else {
            format!("执行 {}...", tool_name)
        }
    }

    /// Return the plugin type that owns the given tool.
    pub fn tool_plugin_type(&self, tool_name: &str) -> Option<PluginType> {
        self.tool_index
            .get(tool_name)
            .map(|&(pi, _)| self.plugins[pi].plugin_type)
    }

    /// Return the plugin directory name for the given tool.
    pub fn tool_plugin_name(&self, tool_name: &str) -> Option<&str> {
        self.tool_index
            .get(tool_name)
            .map(|&(pi, _)| self.plugins[pi].dir_name.as_str())
    }

    /// Return extra system prompt fragments from prompt.json files.
    pub fn extra_system_prompts(&self) -> &[String] {
        &self.system_prompts
    }

    /// Return whether the tool should be sandboxed.
    pub fn tool_sandboxed(&self, tool_name: &str) -> Option<bool> {
        self.tool_index
            .get(tool_name)
            .map(|&(pi, ti)| self.plugins[pi].tools[ti].sandboxed)
    }
}

/// Replace `{field_name}` in template with values from the JSON input.
fn interpolate_template(template: &str, input: &serde_json::Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = input.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{}}}", key);
            let replacement = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }
    }
    result
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

    fn write_prompt_json(dir: &std::path::Path, name: &str, content: &str) {
        let plugin_dir = dir.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let mut f = std::fs::File::create(plugin_dir.join("prompt.json")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn test_load_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = PluginManager::load(tmp.path());
        assert!(mgr.all_tools().is_empty());
    }

    #[test]
    fn test_load_single_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Run commands",
                "input_schema": {"type": "object", "properties": {}, "required": []},
                "status_template": "执行: {command}",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.all_tools().len(), 1);
        assert_eq!(mgr.all_tools()[0].name, "bash");
    }

    #[test]
    fn test_tool_plugin_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Run",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.tool_plugin_name("bash"), Some("builtin"));
    }

    #[test]
    fn test_tool_plugin_type() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "myplugin", r#"{
            "plugin_type": "daemon_tool",
            "tools": [{
                "name": "query",
                "description": "Query stuff",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": false
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.tool_plugin_type("query"), Some(PluginType::DaemonTool));
    }

    #[test]
    fn test_status_text_interpolation() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Run",
                "input_schema": {"type": "object"},
                "status_template": "执行: {command}",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        let input = serde_json::json!({"command": "ls -la"});
        assert_eq!(mgr.tool_status_text("bash", &input), "执行: ls -la");
    }

    #[test]
    fn test_status_text_missing_field() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Run",
                "input_schema": {"type": "object"},
                "status_template": "执行: {command}",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        let input = serde_json::json!({"timeout": 30});
        assert_eq!(mgr.tool_status_text("bash", &input), "执行: {command}");
    }

    #[test]
    fn test_malformed_json_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "bad", "not json{{{");
        write_tool_json(tmp.path(), "good", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "read",
                "description": "Read",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.all_tools().len(), 1);
        assert_eq!(mgr.all_tools()[0].name, "read");
    }

    #[test]
    fn test_duplicate_tool_name_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "plugin_a", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "First",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        write_tool_json(tmp.path(), "plugin_b", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Duplicate",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.all_tools().len(), 1);
    }

    #[test]
    fn test_prompt_json_replace_description() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Original description",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        write_prompt_json(tmp.path(), "builtin", r#"{
            "tools": {
                "bash": {
                    "description": "Custom description"
                }
            }
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.all_tools()[0].description, "Custom description");
    }

    #[test]
    fn test_prompt_json_append_description() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Original",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        write_prompt_json(tmp.path(), "builtin", r#"{
            "tools": {
                "bash": {
                    "append": "Extra guideline"
                }
            }
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.all_tools()[0].description, "Original\nExtra guideline");
    }

    #[test]
    fn test_prompt_json_description_takes_priority_over_append() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Original",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        write_prompt_json(tmp.path(), "builtin", r#"{
            "tools": {
                "bash": {
                    "description": "Replaced",
                    "append": "Should be ignored"
                }
            }
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.all_tools()[0].description, "Replaced");
    }

    #[test]
    fn test_prompt_json_system_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Run",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        write_prompt_json(tmp.path(), "builtin", r#"{
            "system_prompt": ["You are a DevOps expert.", "Use kubectl when possible."],
            "tools": {}
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.extra_system_prompts().len(), 1);
        assert_eq!(mgr.extra_system_prompts()[0], "You are a DevOps expert.\nUse kubectl when possible.");
    }

    #[test]
    fn test_prompt_json_multiline_description() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Original",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        write_prompt_json(tmp.path(), "builtin", r#"{
            "tools": {
                "bash": {
                    "description": ["Line 1", "Line 2", "", "Line 4"]
                }
            }
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.all_tools()[0].description, "Line 1\nLine 2\n\nLine 4");
    }

    #[test]
    fn test_no_prompt_json_keeps_original() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Original description",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.all_tools()[0].description, "Original description");
        assert!(mgr.extra_system_prompts().is_empty());
    }

    #[test]
    fn test_tool_sandboxed() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [
                {"name": "bash", "description": "", "input_schema": {"type": "object"}, "status_template": "", "sandboxed": true},
                {"name": "edit", "description": "", "input_schema": {"type": "object"}, "status_template": "", "sandboxed": false}
            ]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.tool_sandboxed("bash"), Some(true));
        assert_eq!(mgr.tool_sandboxed("edit"), Some(false));
    }
}
