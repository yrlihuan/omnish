use crate::plugin::PluginType;
use omnish_llm::tool::ToolDef;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// A function that produces custom status text for a tool call.
pub type CustomStatusFn = Arc<dyn Fn(&str, &Value) -> String + Send + Sync>;

/// Metadata for a registered tool.
pub struct ToolMeta {
    pub name: String,
    pub display_name: String,
    pub formatter: String,
    pub status_template: String,
    pub custom_status: Option<CustomStatusFn>,
    pub plugin_type: Option<PluginType>,
    pub plugin_name: Option<String>,
}

/// Unified registry for tool metadata, definitions, and runtime overrides.
#[derive(Default)]
pub struct ToolRegistry {
    /// Static tool metadata, set at startup.
    tools: HashMap<String, ToolMeta>,
    /// Static tool definitions, set at startup.
    defs: HashMap<String, ToolDef>,
    /// Runtime-updatable description overrides (tool_name -> description).
    descriptions: RwLock<HashMap<String, String>>,
    /// Runtime-updatable parameter overrides (tool_name -> param_name -> value).
    override_params: RwLock<HashMap<String, HashMap<String, Value>>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self {
            tools: HashMap::new(),
            defs: HashMap::new(),
            descriptions: RwLock::new(HashMap::new()),
            override_params: RwLock::new(HashMap::new()),
        }
    }
}

impl ToolRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register tool metadata. Called at startup before the registry is shared.
    pub fn register(&mut self, meta: ToolMeta) {
        self.tools.insert(meta.name.clone(), meta);
    }

    /// Register a tool definition. Called at startup before the registry is shared.
    pub fn register_def(&mut self, def: ToolDef) {
        self.defs.insert(def.name.clone(), def);
    }

    /// Return the display name for a tool, falling back to the tool name itself.
    pub fn display_name<'a>(&'a self, tool_name: &'a str) -> &'a str {
        self.tools
            .get(tool_name)
            .map(|m| m.display_name.as_str())
            .unwrap_or(tool_name)
    }

    /// Return the formatter name for a tool, falling back to "default".
    pub fn formatter_name(&self, tool_name: &str) -> &str {
        self.tools
            .get(tool_name)
            .map(|m| m.formatter.as_str())
            .unwrap_or("default")
    }

    /// Return the status template for a tool, falling back to "".
    pub fn status_template(&self, tool_name: &str) -> &str {
        self.tools
            .get(tool_name)
            .map(|m| m.status_template.as_str())
            .unwrap_or("")
    }

    /// Produce status text for a tool call. Uses custom_status if set,
    /// otherwise interpolates the status_template with values from input.
    pub fn status_text(&self, tool_name: &str, input: &Value) -> String {
        if let Some(meta) = self.tools.get(tool_name) {
            if let Some(ref custom) = meta.custom_status {
                return custom(tool_name, input);
            }
            interpolate_template(&meta.status_template, input)
        } else {
            String::new()
        }
    }

    /// Return the plugin type for a tool, if registered.
    pub fn plugin_type(&self, tool_name: &str) -> Option<PluginType> {
        self.tools.get(tool_name).and_then(|m| m.plugin_type)
    }

    /// Return the plugin name for a tool, if registered.
    pub fn plugin_name(&self, tool_name: &str) -> Option<&str> {
        self.tools
            .get(tool_name)
            .and_then(|m| m.plugin_name.as_deref())
    }

    /// Check whether a tool is registered.
    pub fn is_known(&self, tool_name: &str) -> bool {
        self.tools.contains_key(tool_name)
    }

    /// Return override params for a tool (cloned from the RwLock).
    pub fn override_params(&self, tool_name: &str) -> Option<HashMap<String, Value>> {
        let guard = self.override_params.read().unwrap();
        guard.get(tool_name).cloned()
    }

    /// Collect all tool definitions with description overrides applied.
    pub fn all_defs(&self) -> Vec<ToolDef> {
        let descs = self.descriptions.read().unwrap();
        self.defs
            .values()
            .map(|def| {
                let mut d = def.clone();
                if let Some(desc) = descs.get(&d.name) {
                    d.description = desc.clone();
                }
                d
            })
            .collect()
    }

    /// Update runtime overrides (descriptions and params). Takes &self because
    /// it only writes to the interior RwLock fields.
    pub fn update_overrides(
        &self,
        descriptions: HashMap<String, String>,
        override_params: HashMap<String, HashMap<String, Value>>,
    ) {
        {
            let mut guard = self.descriptions.write().unwrap();
            *guard = descriptions;
        }
        {
            let mut guard = self.override_params.write().unwrap();
            *guard = override_params;
        }
    }
}

/// Replace `{field_name}` placeholders in template with values from the JSON input.
fn interpolate_template(template: &str, input: &Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = input.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{}}}", key);
            let replacement = match value {
                Value::String(s) => s.clone(),
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

    fn make_meta(name: &str) -> ToolMeta {
        ToolMeta {
            name: name.to_string(),
            display_name: name.to_string(),
            formatter: "default".to_string(),
            status_template: String::new(),
            custom_status: None,
            plugin_type: None,
            plugin_name: None,
        }
    }

    #[test]
    fn test_register_and_lookup_display_name() {
        let mut reg = ToolRegistry::new();
        let mut meta = make_meta("bash");
        meta.display_name = "Bash Shell".to_string();
        reg.register(meta);

        assert_eq!(reg.display_name("bash"), "Bash Shell");
        // Unknown tool falls back to tool_name
        assert_eq!(reg.display_name("unknown"), "unknown");
    }

    #[test]
    fn test_formatter_lookup() {
        let mut reg = ToolRegistry::new();
        let mut meta = make_meta("code_edit");
        meta.formatter = "diff".to_string();
        reg.register(meta);

        assert_eq!(reg.formatter_name("code_edit"), "diff");
        // Unknown tool falls back to "default"
        assert_eq!(reg.formatter_name("unknown"), "default");
    }

    #[test]
    fn test_status_text_with_template() {
        let mut reg = ToolRegistry::new();
        let mut meta = make_meta("bash");
        meta.status_template = "running: {command}".to_string();
        reg.register(meta);

        let input = serde_json::json!({"command": "ls -la"});
        assert_eq!(reg.status_text("bash", &input), "running: ls -la");

        // Missing field leaves placeholder
        let input2 = serde_json::json!({"timeout": 30});
        assert_eq!(reg.status_text("bash", &input2), "running: {command}");

        // Unknown tool returns empty string
        assert_eq!(reg.status_text("unknown", &input), "");
    }

    #[test]
    fn test_status_text_with_custom_fn() {
        let mut reg = ToolRegistry::new();
        let mut meta = make_meta("web_search");
        meta.status_template = "searching: {query}".to_string();
        meta.custom_status = Some(Arc::new(|tool_name, input| {
            let query = input
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("[{}] custom search: {}", tool_name, query)
        }));
        reg.register(meta);

        let input = serde_json::json!({"query": "rust async"});
        // custom_status takes precedence over template
        assert_eq!(
            reg.status_text("web_search", &input),
            "[web_search] custom search: rust async"
        );
    }

    #[test]
    fn test_plugin_type_lookup() {
        let mut reg = ToolRegistry::new();

        let mut meta1 = make_meta("daemon_tool");
        meta1.plugin_type = Some(PluginType::DaemonTool);
        meta1.plugin_name = Some("my_plugin".to_string());
        reg.register(meta1);

        let mut meta2 = make_meta("client_tool");
        meta2.plugin_type = Some(PluginType::ClientTool);
        reg.register(meta2);

        assert_eq!(reg.plugin_type("daemon_tool"), Some(PluginType::DaemonTool));
        assert_eq!(reg.plugin_type("client_tool"), Some(PluginType::ClientTool));
        assert_eq!(reg.plugin_type("unknown"), None);

        assert_eq!(reg.plugin_name("daemon_tool"), Some("my_plugin"));
        assert_eq!(reg.plugin_name("client_tool"), None);

        assert!(reg.is_known("daemon_tool"));
        assert!(!reg.is_known("unknown"));
    }

    #[test]
    fn test_override_updates() {
        let mut reg = ToolRegistry::new();
        reg.register(make_meta("bash"));
        reg.register_def(ToolDef {
            name: "bash".to_string(),
            description: "Original description".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        });

        // Before overrides
        let defs = reg.all_defs();
        let bash_def = defs.iter().find(|d| d.name == "bash").unwrap();
        assert_eq!(bash_def.description, "Original description");
        assert!(reg.override_params("bash").is_none());

        // Apply overrides
        let mut descs = HashMap::new();
        descs.insert("bash".to_string(), "Updated description".to_string());

        let mut params = HashMap::new();
        let mut bash_params = HashMap::new();
        bash_params.insert("timeout".to_string(), serde_json::json!(30));
        params.insert("bash".to_string(), bash_params);

        reg.update_overrides(descs, params);

        // After overrides
        let defs = reg.all_defs();
        let bash_def = defs.iter().find(|d| d.name == "bash").unwrap();
        assert_eq!(bash_def.description, "Updated description");

        let op = reg.override_params("bash").unwrap();
        assert_eq!(op["timeout"], serde_json::json!(30));

        // Clear overrides
        reg.update_overrides(HashMap::new(), HashMap::new());
        let defs = reg.all_defs();
        let bash_def = defs.iter().find(|d| d.name == "bash").unwrap();
        assert_eq!(bash_def.description, "Original description");
        assert!(reg.override_params("bash").is_none());
    }
}
