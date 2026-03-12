use omnish_llm::tool::ToolDef;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

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
    sandboxed: bool,
}

/// A plugin loaded from a tool.json file.
#[derive(Debug)]
struct PluginInfo {
    dir_name: String,
    plugin_type: PluginType,
    tools: Vec<ToolEntry>,
}

/// Cached prompt.json overrides, updated on file changes.
struct PromptCache {
    /// tool_name → effective description (base with override/append applied)
    descriptions: HashMap<String, String>,
    system_prompts: Vec<String>,
}

/// Metadata-only plugin manager. Loads tool definitions from JSON files.
/// Watches prompt.json files for changes via inotify.
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
                tools.push(ToolEntry {
                    def: ToolDef {
                        name: te.name,
                        description: te.description.into_string(),
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

        let mgr = Self {
            plugins_dir: plugins_dir.to_path_buf(),
            plugins,
            tool_index,
            prompt_cache: RwLock::new(PromptCache {
                descriptions: HashMap::new(),
                system_prompts: Vec::new(),
            }),
        };
        mgr.reload_prompts();
        mgr
    }

    /// Re-read all prompt.json files and update the prompt cache.
    pub fn reload_prompts(&self) {
        let mut descriptions = HashMap::new();
        let mut system_prompts = Vec::new();

        for plugin in &self.plugins {
            let prompt_json = self.plugins_dir.join(&plugin.dir_name).join("prompt.json");
            let prompt_overrides = if prompt_json.is_file() {
                match std::fs::read_to_string(&prompt_json) {
                    Ok(c) => match serde_json::from_str::<PromptJsonFile>(&c) {
                        Ok(p) => Some(p),
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
            };

            if let Some(ref pf) = prompt_overrides {
                if let Some(ref sp) = pf.system_prompt {
                    system_prompts.push(sp.clone().into_string());
                }
            }

            for te in &plugin.tools {
                let mut desc = te.def.description.clone();
                if let Some(ref pf) = prompt_overrides {
                    if let Some(ovr) = pf.tools.get(&te.def.name) {
                        if let Some(ref d) = ovr.description {
                            desc = d.clone().into_string();
                        } else if let Some(ref a) = ovr.append {
                            desc.push('\n');
                            desc.push_str(&a.clone().into_string());
                        }
                    }
                }
                descriptions.insert(te.def.name.clone(), desc);
            }
        }

        tracing::info!("Reloaded prompt overrides ({} tools, {} system prompts)",
            descriptions.len(), system_prompts.len());

        let mut cache = self.prompt_cache.write().unwrap();
        cache.descriptions = descriptions;
        cache.system_prompts = system_prompts;
    }

    /// Collect all tool definitions from all plugins (with prompt overrides applied).
    pub fn all_tools(&self) -> Vec<ToolDef> {
        let cache = self.prompt_cache.read().unwrap();
        self.plugins
            .iter()
            .flat_map(|p| p.tools.iter().map(|t| {
                let mut def = t.def.clone();
                if let Some(desc) = cache.descriptions.get(&def.name) {
                    def.description = desc.clone();
                }
                def
            }))
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
    pub fn extra_system_prompts(&self) -> Vec<String> {
        self.prompt_cache.read().unwrap().system_prompts.clone()
    }

    /// Return whether the tool should be sandboxed.
    pub fn tool_sandboxed(&self, tool_name: &str) -> Option<bool> {
        self.tool_index
            .get(tool_name)
            .map(|&(pi, ti)| self.plugins[pi].tools[ti].sandboxed)
    }

    /// Spawn an async task that watches prompt.json files for changes.
    /// Uses inotify on Linux; polling fallback on other platforms.
    /// Calls `reload_prompts()` when any prompt.json is created or modified.
    #[cfg(target_os = "linux")]
    pub async fn watch_prompts(self: &std::sync::Arc<Self>) {
        use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};
        use std::os::fd::AsFd;
        use tokio::io::unix::AsyncFd;
        use tokio::io::Interest;

        let inotify = match Inotify::init(InitFlags::IN_NONBLOCK) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("Failed to init inotify: {}", e);
                return;
            }
        };

        let watch_flags = AddWatchFlags::IN_CREATE
            | AddWatchFlags::IN_CLOSE_WRITE
            | AddWatchFlags::IN_MOVED_TO;

        // Watch the plugins root directory for new subdirectories
        if let Err(e) = inotify.add_watch(&self.plugins_dir, watch_flags) {
            tracing::warn!("Failed to watch {}: {}", self.plugins_dir.display(), e);
            return;
        }

        // Watch each existing plugin subdirectory
        if let Ok(rd) = std::fs::read_dir(&self.plugins_dir) {
            for entry in rd.filter_map(|e| e.ok()) {
                if entry.path().is_dir() {
                    let _ = inotify.add_watch(&entry.path(), watch_flags);
                }
            }
        }

        let async_fd = match AsyncFd::with_interest(inotify.as_fd().try_clone_to_owned().unwrap(), Interest::READABLE) {
            Ok(fd) => fd,
            Err(e) => {
                tracing::warn!("Failed to create AsyncFd for inotify: {}", e);
                return;
            }
        };

        // Keep inotify alive for the lifetime of this task
        let _inotify = inotify;

        tracing::info!("Watching prompt.json files for changes in {}", self.plugins_dir.display());

        loop {
            let mut guard = match async_fd.readable().await {
                Ok(g) => g,
                Err(e) => {
                    tracing::warn!("inotify readable error: {}", e);
                    break;
                }
            };

            // Read and consume all inotify events
            let mut should_reload = false;
            loop {
                match _inotify.read_events() {
                    Ok(events) => {
                        for event in &events {
                            let name = event.name
                                .as_ref()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();

                            // New subdirectory in plugins dir — add watch
                            if event.mask.contains(AddWatchFlags::IN_CREATE)
                                || event.mask.contains(AddWatchFlags::IN_MOVED_TO)
                            {
                                let new_path = self.plugins_dir.join(&name);
                                if new_path.is_dir() {
                                    let _ = _inotify.add_watch(&new_path, watch_flags);
                                    tracing::info!("Watching new plugin dir: {}", name);
                                }
                            }

                            // prompt.json created or modified
                            if name == "prompt.json" {
                                should_reload = true;
                            }
                        }
                        if events.is_empty() {
                            break;
                        }
                    }
                    Err(nix::errno::Errno::EAGAIN) => break,
                    Err(e) => {
                        tracing::warn!("inotify read error: {}", e);
                        break;
                    }
                }
            }

            guard.clear_ready();

            if should_reload {
                tracing::info!("prompt.json changed, reloading...");
                self.reload_prompts();
            }
        }
    }

    /// Fallback: poll prompt.json files for changes every 5 seconds.
    #[cfg(not(target_os = "linux"))]
    pub async fn watch_prompts(self: &std::sync::Arc<Self>) {
        use std::collections::HashMap;

        tracing::info!("Polling prompt.json files for changes in {}", self.plugins_dir.display());

        let mut mtimes: HashMap<PathBuf, std::time::SystemTime> = HashMap::new();

        // Seed initial modification times
        for plugin in &self.plugins {
            let path = self.plugins_dir.join(&plugin.dir_name).join("prompt.json");
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(mtime) = meta.modified() {
                    mtimes.insert(path, mtime);
                }
            }
        }

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            let mut changed = false;
            for plugin in &self.plugins {
                let path = self.plugins_dir.join(&plugin.dir_name).join("prompt.json");
                if let Ok(meta) = std::fs::metadata(&path) {
                    if let Ok(mtime) = meta.modified() {
                        match mtimes.get(&path) {
                            Some(prev) if *prev == mtime => {}
                            _ => {
                                mtimes.insert(path, mtime);
                                changed = true;
                            }
                        }
                    }
                } else {
                    // File removed — if we had it before, that's a change
                    if mtimes.remove(&path).is_some() {
                        changed = true;
                    }
                }
            }

            if changed {
                tracing::info!("prompt.json changed, reloading...");
                self.reload_prompts();
            }
        }
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

    #[test]
    fn test_reload_prompts_picks_up_changes() {
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
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.all_tools()[0].description, "Original");

        // Write prompt.json and reload
        write_prompt_json(tmp.path(), "builtin", r#"{
            "tools": { "bash": { "description": "Updated" } }
        }"#);
        mgr.reload_prompts();
        assert_eq!(mgr.all_tools()[0].description, "Updated");

        // Remove prompt.json override by writing empty overrides
        write_prompt_json(tmp.path(), "builtin", r#"{ "tools": {} }"#);
        mgr.reload_prompts();
        assert_eq!(mgr.all_tools()[0].description, "Original");
    }
}
