use omnish_llm::backend::CacheHint;
use omnish_llm::tool::ToolDef;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

/// Return type for `reload_overrides`: (changed, descriptions, override_params).
pub type OverrideReloadResult = (bool, HashMap<String, String>, HashMap<String, HashMap<String, serde_json::Value>>);

/// Classifies whether a plugin's tools run on the daemon or the client side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    DaemonTool,
    ClientTool,
}

/// A configurable parameter declared by a plugin in tool.json.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigParam {
    pub name: String,
    pub label: String,
    #[serde(default = "default_config_param_kind")]
    pub kind: String,
}

fn default_config_param_kind() -> String {
    "text".to_string()
}

/// Plugin metadata exposed for config menu generation.
#[derive(Debug, Clone)]
pub struct PluginConfigMeta {
    pub name: String,
    pub config_params: Vec<ConfigParam>,
}

/// A single tool entry parsed from tool.json (base definition, immutable).
#[derive(Debug, Clone)]
struct ToolEntry {
    def: ToolDef,
    status_template: String,
    display_name: String,
    formatter: String,
    summarization_prompt: Option<String>,
}

/// A plugin loaded from a tool.json file.
#[derive(Debug)]
struct PluginInfo {
    dir_name: String,
    plugin_type: PluginType,
    tools: Vec<ToolEntry>,
    formatter_binary: Option<String>,
    config_params: Vec<ConfigParam>,
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
/// All collections are behind RwLock to support runtime plugin reload.
pub struct PluginManager {
    plugins_dir: PathBuf,
    plugins: RwLock<Vec<PluginInfo>>,
    /// Maps tool_name → (plugin_index, tool_index) for fast lookup.
    tool_index: RwLock<HashMap<String, (usize, usize)>>,
    /// Prompt overrides, updated on file changes.
    prompt_cache: RwLock<PromptCache>,
    /// Latest plugins section of DaemonConfig, kept in sync with daemon.toml.
    /// `rescan` consults this to honour `enabled = false` when it discovers
    /// a new subdirectory, so a freshly-dropped plugin respects its config.
    plugins_config: RwLock<HashMap<String, omnish_common::config::ConfigMap>>,
    /// Plugin subdirectories we've already registered with the shared
    /// FileWatcher. Used by `rescan` to skip duplicate `watch()` calls -
    /// each call adds a `(path, sender)` entry, so repeated registration
    /// would slowly grow the watcher's list without bound.
    watched_subdirs: Mutex<HashSet<PathBuf>>,
}

#[derive(Deserialize)]
struct ToolJsonFile {
    plugin_type: String,
    #[serde(default)]
    formatter_binary: Option<String>,
    #[serde(default)]
    config_params: Vec<ConfigParam>,
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
    /// Ignored - all tools are sandboxed. Kept for backwards compatibility with existing tool.json files.
    #[serde(default)]
    #[allow(dead_code)]
    sandboxed: bool,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    formatter: Option<String>,
    /// LLM summarization prompt template for post-processing tool results (reserved for future use).
    #[serde(default)]
    #[allow(dead_code)]
    summarization_prompt: Option<String>,
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

/// Bundled plugin: web_search
const BUNDLED_WEB_SEARCH_TOOL_JSON: &str = include_str!("../../../plugins/web_search/tool.json");
const BUNDLED_WEB_SEARCH_SCRIPT: &str = include_str!("../../../plugins/web_search/web_search");

/// Bundled plugin: web_fetch
const BUNDLED_WEB_FETCH_TOOL_JSON: &str = include_str!("../../../plugins/web_fetch/tool.json");
const BUNDLED_WEB_FETCH_SCRIPT: &str = include_str!("../../../plugins/web_fetch/web_fetch");
const BUNDLED_WEB_FETCH_FORMATTER: &str = include_str!("../../../plugins/web_fetch/web_fetch_formatter");

/// Auto-install bundled plugins when their plugin config is present in daemon.toml.
/// For example, if `[plugins.web_search]` contains `api_key`, install the web_search
/// plugin to `~/.omnish/plugins/web_search/` so it's available without manual setup.
pub fn auto_install_bundled_plugins(
    plugins_dir: &Path,
    plugins_config: &HashMap<String, omnish_common::config::ConfigMap>,
) {
    // web_search: install if [plugins.web_search] has api_key
    if let Some(ws_config) = plugins_config.get("web_search") {
        if ws_config.contains_key("api_key") {
            let _ = (|| -> Result<(), std::io::Error> {
                let plugin_dir = plugins_dir.join("web_search");
                let tool_json = plugin_dir.join("tool.json");
                let script = plugin_dir.join("web_search");
                if !tool_json.exists() {
                    std::fs::create_dir_all(&plugin_dir)?;
                    std::fs::write(&tool_json, BUNDLED_WEB_SEARCH_TOOL_JSON)?;
                    std::fs::write(&script, BUNDLED_WEB_SEARCH_SCRIPT)?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
                    }
                    tracing::info!("Auto-installed bundled web_search plugin");
                }
                Ok(())
            })().map_err(|e| tracing::warn!("Failed to install web_search plugin: {e}"));
        }
    }

    // web_fetch: always install (no API key required)
    let _ = (|| -> Result<(), std::io::Error> {
        let plugin_dir = plugins_dir.join("web_fetch");
        let tool_json = plugin_dir.join("tool.json");
        let script = plugin_dir.join("web_fetch");
        let formatter = plugin_dir.join("web_fetch_formatter");
        if !tool_json.exists() {
            std::fs::create_dir_all(&plugin_dir)?;
            std::fs::write(&tool_json, BUNDLED_WEB_FETCH_TOOL_JSON)?;
            std::fs::write(&script, BUNDLED_WEB_FETCH_SCRIPT)?;
            std::fs::write(&formatter, BUNDLED_WEB_FETCH_FORMATTER)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
                let _ = std::fs::set_permissions(&formatter, std::fs::Permissions::from_mode(0o755));
            }
            tracing::info!("Auto-installed bundled web_fetch plugin");
        }
        Ok(())
    })().map_err(|e| tracing::warn!("Failed to install web_fetch plugin: {e}"));
}

impl PluginManager {
    /// Load all plugins from the given directory.
    /// Each subdirectory containing a `tool.json` is treated as a plugin.
    /// Built-in tools are always loaded from embedded data if not found on disk.
    pub fn load(
        plugins_dir: &Path,
        plugins_config: &HashMap<String, omnish_common::config::ConfigMap>,
    ) -> Self {
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
                            cache: CacheHint::None,
                        },
                        status_template: te.status_template,
                        display_name,
                        formatter,
                        summarization_prompt: te.summarization_prompt,
                    });
                }
                tracing::info!("Loaded builtin plugin with {} tools", tools.len());
                plugins.push(PluginInfo {
                    dir_name: "builtin".to_string(),
                    plugin_type,
                    tools,
                    formatter_binary: None,
                    config_params: parsed.config_params,
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
            // Skip "builtin" - always loaded from embedded data above
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

            // Check if plugin is explicitly disabled
            let disabled = plugins_config
                .get(&dir_name)
                .and_then(|cfg| cfg.get("enabled"))
                .and_then(|v| v.as_bool())
                .map(|b| !b)
                .unwrap_or(false);

            if disabled {
                // Still store PluginInfo for config_meta(), but with no tools
                plugins.push(PluginInfo {
                    dir_name,
                    plugin_type,
                    tools: Vec::new(),
                    formatter_binary: None,
                    config_params: parsed.config_params,
                });
                continue;
            }

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
                        cache: CacheHint::None,
                    },
                    status_template: te.status_template,
                    display_name,
                    formatter,
                    summarization_prompt: te.summarization_prompt,
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
                formatter_binary: parsed.formatter_binary,
                config_params: parsed.config_params,
            });
        }

        let mgr = Self {
            plugins_dir: plugins_dir.to_path_buf(),
            plugins: RwLock::new(plugins),
            tool_index: RwLock::new(tool_index),
            prompt_cache: RwLock::new(PromptCache {
                descriptions: HashMap::new(),
                override_params: HashMap::new(),
            }),
            plugins_config: RwLock::new(plugins_config.clone()),
            watched_subdirs: Mutex::new(HashSet::new()),
        };
        mgr.reload_overrides();
        mgr
    }

    /// Keep the cached plugins section in sync with daemon.toml. Called when
    /// the `[plugins]` section changes so `rescan` sees the latest enabled flags.
    pub fn update_plugins_config(&self, cfg: &HashMap<String, omnish_common::config::ConfigMap>) {
        *self.plugins_config.write().unwrap() = cfg.clone();
    }

    /// Re-scan `plugins_dir`, registering plugins for any new subdirectory
    /// and unregistering those whose directory has disappeared. Idempotent
    /// and cheap when nothing changed. Also subscribes the shared
    /// `FileWatcher` to every plugin subdirectory it sees, so subsequent
    /// file drops inside a freshly-created plugin dir trigger another
    /// rescan (inotify on `plugins_dir` is non-recursive, so without
    /// per-subdir watches a two-step `mkdir` + `cp tool.json` would be
    /// invisible).
    ///
    /// Returns true if the plugin list changed.
    pub fn rescan(
        &self,
        registry: &crate::tool_registry::ToolRegistry,
        file_watcher: Option<&crate::file_watcher::FileWatcher>,
    ) -> bool {
        // Distinguish "plugins_dir is unreadable" from "plugins_dir is empty":
        // the former must skip reconciliation, otherwise a transient permission
        // or filesystem error would mass-unload every loaded plugin.
        let on_disk: Vec<String> = match std::fs::read_dir(&self.plugins_dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != "builtin")
                .collect(),
            Err(e) => {
                tracing::warn!(
                    "rescan skipped: cannot read {}: {}",
                    self.plugins_dir.display(),
                    e
                );
                return false;
            }
        };

        if let Some(fw) = file_watcher {
            let mut seen = self.watched_subdirs.lock().unwrap();
            for name in &on_disk {
                let p = self.plugins_dir.join(name);
                if seen.insert(p.clone()) {
                    // Drop the returned receiver: FileWatcher dispatches any
                    // event to every registered directory sender, so the main
                    // plugins_dir subscriber will still wake on inner events.
                    let _ = fw.watch(p);
                }
            }
        }

        let on_disk_set: HashSet<&str> = on_disk.iter().map(String::as_str).collect();
        let plugins_config = self.plugins_config.read().unwrap().clone();

        // Forget `watched_subdirs` entries whose directory has disappeared.
        // Inotify already auto-removes the watch descriptor on IN_IGNORED,
        // so we just drop our dedup marker - otherwise a later rebuild of
        // the same-named plugin would never re-register an inotify watch.
        {
            let mut seen = self.watched_subdirs.lock().unwrap();
            seen.retain(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| on_disk_set.contains(n))
                    .unwrap_or(false)
            });
        }

        let mut changed = false;
        let mut plugins = self.plugins.write().unwrap();
        let mut tool_index = self.tool_index.write().unwrap();

        // Unload any loaded plugin whose directory is gone OR whose config
        // flipped to `enabled = false`. Handling both cases here keeps the
        // filesystem event path and the daemon.toml event path converging on
        // the same final state regardless of which one ran first.
        for plugin in plugins.iter_mut() {
            if plugin.dir_name == "builtin" {
                continue;
            }
            if plugin.tools.is_empty() {
                continue;
            }
            let dir_gone = !on_disk_set.contains(plugin.dir_name.as_str());
            let disabled = plugins_config
                .get(&plugin.dir_name)
                .and_then(|c| c.get("enabled"))
                .and_then(|v| v.as_bool())
                .map(|b| !b)
                .unwrap_or(false);
            if dir_gone || disabled {
                for te in &plugin.tools {
                    tool_index.remove(&te.def.name);
                }
                plugin.tools.clear();
                plugin.formatter_binary = None;
                registry.unregister_by_plugin(&plugin.dir_name);
                let reason = if dir_gone { "directory gone" } else { "config disabled" };
                tracing::info!("plugin '{}' unloaded ({})", plugin.dir_name, reason);
                changed = true;
            }
        }

        // Register new / revive ghost entries for plugins now present on disk.
        for name in &on_disk {
            let tool_json_path = self.plugins_dir.join(name).join("tool.json");
            if !tool_json_path.is_file() {
                continue;
            }

            let existing_idx = plugins.iter().position(|p| p.dir_name == *name);
            // Skip if already fully loaded. A ghost entry (empty tools) can
            // arise from two paths: a prior disable, or a previous rescan
            // that observed the dir while tool.json was missing.
            let is_ghost = existing_idx.is_none_or(|i| plugins[i].tools.is_empty());
            if !is_ghost {
                continue;
            }

            let disabled = plugins_config
                .get(name)
                .and_then(|c| c.get("enabled"))
                .and_then(|v| v.as_bool())
                .map(|b| !b)
                .unwrap_or(false);

            let content = match std::fs::read_to_string(&tool_json_path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("failed to read {}: {}", tool_json_path.display(), e);
                    continue;
                }
            };
            let parsed: ToolJsonFile = match serde_json::from_str(&content) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("malformed {}: {}", tool_json_path.display(), e);
                    continue;
                }
            };
            let plugin_type = match parsed.plugin_type.as_str() {
                "client_tool" => PluginType::ClientTool,
                _ => PluginType::DaemonTool,
            };

            // Disabled plugin: keep metadata only, no tools registered.
            if disabled {
                if existing_idx.is_none() {
                    plugins.push(PluginInfo {
                        dir_name: name.clone(),
                        plugin_type,
                        tools: Vec::new(),
                        formatter_binary: None,
                        config_params: parsed.config_params,
                    });
                    changed = true;
                }
                continue;
            }

            // Reserve the PluginInfo slot before any tool registration so
            // `plugin_idx` is the slot's final position. Building tools first
            // and pushing later would work today but makes the indices depend
            // on the `insert`/`push` ordering, which is easy to break later.
            let plugin_idx = match existing_idx {
                Some(idx) => idx,
                None => {
                    plugins.push(PluginInfo {
                        dir_name: name.clone(),
                        plugin_type,
                        tools: Vec::new(),
                        formatter_binary: None,
                        config_params: parsed.config_params.clone(),
                    });
                    plugins.len() - 1
                }
            };

            let mut tools = Vec::new();
            for te in parsed.tools {
                if tool_index.contains_key(&te.name) {
                    tracing::warn!(
                        "duplicate tool name '{}' in {}, skipping",
                        te.name,
                        tool_json_path.display()
                    );
                    continue;
                }
                let tool_idx = tools.len();
                tool_index.insert(te.name.clone(), (plugin_idx, tool_idx));
                let display_name = te.display_name.clone().unwrap_or_else(|| te.name.clone());
                let formatter = te
                    .formatter
                    .clone()
                    .unwrap_or_else(|| "default".to_string());
                registry.register(crate::tool_registry::ToolMeta {
                    name: te.name.clone(),
                    display_name: display_name.clone(),
                    formatter: formatter.clone(),
                    status_template: te.status_template.clone(),
                    custom_status: None,
                    plugin_type: Some(plugin_type),
                    plugin_name: Some(name.clone()),
                    summarization_prompt: te.summarization_prompt.clone(),
                });
                let def = ToolDef {
                    name: te.name,
                    description: te.description.into_string(),
                    input_schema: te.input_schema,
                    cache: CacheHint::None,
                };
                registry.register_def(def.clone());
                tools.push(ToolEntry {
                    def,
                    status_template: te.status_template,
                    display_name,
                    formatter,
                    summarization_prompt: te.summarization_prompt,
                });
            }

            plugins[plugin_idx].tools = tools;
            plugins[plugin_idx].formatter_binary = parsed.formatter_binary;
            plugins[plugin_idx].config_params = parsed.config_params;
            tracing::info!("plugin '{}' loaded via rescan", name);
            changed = true;
        }

        changed
    }

    /// Re-read all tool.override.json files and update the prompt cache.
    /// Returns (changed, descriptions, override_params).
    pub fn reload_overrides(&self) -> OverrideReloadResult {
        let mut descriptions = HashMap::new();
        let mut override_params = HashMap::new();

        let plugins = self.plugins.read().unwrap();
        for plugin in plugins.iter() {
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
        drop(plugins);

        let mut cache = self.prompt_cache.write().unwrap();
        let changed = cache.descriptions != descriptions || cache.override_params != override_params;
        if changed {
            tracing::info!("Reloaded tool overrides ({} tools)", descriptions.len());
            cache.descriptions = descriptions.clone();
            cache.override_params = override_params.clone();
        }
        (changed, descriptions, override_params)
    }

    /// Return the executable path for the plugin that owns the given tool.
    pub fn plugin_executable(&self, tool_name: &str) -> Option<std::path::PathBuf> {
        let tool_index = self.tool_index.read().unwrap();
        let plugins = self.plugins.read().unwrap();
        tool_index.get(tool_name).map(|&(pi, _)| {
            let dir_name = &plugins[pi].dir_name;
            self.plugins_dir.join(dir_name).join(dir_name)
        })
    }

    /// Return (formatter_name, binary_path) pairs for external formatters.
    /// Only returns entries where a tool has a non-default formatter AND the plugin has a formatter_binary.
    pub fn formatter_binaries(&self) -> Vec<(String, PathBuf)> {
        let mut result = Vec::new();
        let plugins = self.plugins.read().unwrap();
        for plugin in plugins.iter() {
            if let Some(ref binary) = plugin.formatter_binary {
                let binary_path = self.plugins_dir.join(&plugin.dir_name).join(binary);
                for tool in &plugin.tools {
                    if tool.formatter != "default" {
                        // Avoid duplicate registrations for the same formatter name
                        if !result.iter().any(|(name, _): &(String, PathBuf)| name == &tool.formatter) {
                            result.push((tool.formatter.clone(), binary_path.clone()));
                        }
                    }
                }
            }
        }
        result
    }

    /// Register all plugin tools into a ToolRegistry.
    /// Includes tool metadata (display_name, formatter, status_template)
    /// and tool definitions (for LLM prompt construction).
    pub fn register_all(&self, registry: &crate::tool_registry::ToolRegistry) {
        let plugins = self.plugins.read().unwrap();
        for plugin in plugins.iter() {
            for te in &plugin.tools {
                registry.register(crate::tool_registry::ToolMeta {
                    name: te.def.name.clone(),
                    display_name: te.display_name.clone(),
                    formatter: te.formatter.clone(),
                    status_template: te.status_template.clone(),
                    custom_status: None,
                    plugin_type: Some(plugin.plugin_type),
                    plugin_name: Some(plugin.dir_name.clone()),
                    summarization_prompt: te.summarization_prompt.clone(),
                });
                registry.register_def(te.def.clone());
            }
        }
        drop(plugins);
        // Apply current overrides
        let cache = self.prompt_cache.read().unwrap();
        registry.update_overrides(cache.descriptions.clone(), cache.override_params.clone());
    }

    /// Returns plugin metadata for the config menu (all non-builtin plugins).
    pub fn config_meta(&self) -> Vec<PluginConfigMeta> {
        self.plugins
            .read()
            .unwrap()
            .iter()
            .filter(|p| p.dir_name != "builtin")
            .map(|p| PluginConfigMeta {
                name: p.dir_name.clone(),
                config_params: p.config_params.clone(),
            })
            .collect()
    }

    /// Start watching plugin overrides using a shared file watcher receiver.
    /// On every debounced tick we first `rescan` the plugins directory so
    /// newly-dropped or removed plugin subdirectories take effect without a
    /// daemon restart, then refresh `tool.override.json` content.
    pub async fn watch_with(
        self: &Arc<Self>,
        mut rx: tokio::sync::watch::Receiver<()>,
        registry: std::sync::Arc<crate::tool_registry::ToolRegistry>,
        file_watcher: std::sync::Arc<crate::file_watcher::FileWatcher>,
    ) {
        tracing::info!("watching plugin overrides via shared file watcher: {}", self.plugins_dir.display());
        while rx.changed().await.is_ok() {
            // Debounce: wait for rapid inotify events to settle
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            // Drain events accumulated during debounce
            let _ = rx.borrow_and_update();

            self.rescan(&registry, Some(&file_watcher));
            let (changed, descs, params) = self.reload_overrides();
            if changed {
                registry.update_overrides(descs, params);
            }
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
        let reg = ToolRegistry::new();
        mgr.register_all(&reg);
        reg.all_defs().len()
    }

    /// Helper: get description for a tool via ToolRegistry (after register_all + overrides).
    fn get_description(mgr: &PluginManager, name: &str) -> String {
        use crate::tool_registry::ToolRegistry;
        let reg = ToolRegistry::new();
        mgr.register_all(&reg);
        reg.all_defs().into_iter().find(|t| t.name == name).unwrap().description
    }

    #[test]
    fn test_load_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
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
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
        let reg = crate::tool_registry::ToolRegistry::new();
        mgr.register_all(&reg);
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
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
        let reg = crate::tool_registry::ToolRegistry::new();
        mgr.register_all(&reg);
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
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
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
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
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
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
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
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
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
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
        assert_eq!(get_description(&mgr, "bash"), "Line 1\nLine 2\n\nLine 4");
    }

    #[test]
    fn test_no_prompt_json_keeps_original() {
        let tmp = tempfile::tempdir().unwrap();
        // No override file - embedded description is used
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
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
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
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
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
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
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
        let reg = ToolRegistry::new();
        mgr.register_all(&reg);
        // Custom plugin tool
        assert_eq!(reg.display_name("my_tool"), "MyTool");
        assert_eq!(reg.formatter_name("my_tool"), "default");
        // Built-in tool (from embedded tool.json - "bash" has display_name "Bash")
        assert_eq!(reg.display_name("bash"), "Bash");
        // Definitions should be registered
        let defs = reg.all_defs();
        assert!(defs.iter().any(|d| d.name == "bash"));
        assert!(defs.iter().any(|d| d.name == "my_tool"));
    }

    #[test]
    fn test_auto_install_web_search_when_api_key_present() {
        let tmp = tempfile::tempdir().unwrap();
        let mut tools_config = HashMap::new();
        let mut ws = HashMap::new();
        ws.insert("api_key".to_string(), serde_json::json!("test-key"));
        tools_config.insert("web_search".to_string(), omnish_common::config::ConfigMap::from(ws));

        auto_install_bundled_plugins(tmp.path(), &tools_config);

        assert!(tmp.path().join("web_search/tool.json").exists());
        assert!(tmp.path().join("web_search/web_search").exists());

        // Should be loadable
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
        assert!(mgr.plugin_executable("web_search").is_some());
    }

    #[test]
    fn test_auto_install_skipped_without_api_key() {
        let tmp = tempfile::tempdir().unwrap();
        let tools_config = HashMap::new();

        auto_install_bundled_plugins(tmp.path(), &tools_config);

        assert!(!tmp.path().join("web_search").exists());
    }

    #[test]
    fn test_auto_install_skipped_if_already_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let mut tools_config = HashMap::new();
        let mut ws = HashMap::new();
        ws.insert("api_key".to_string(), serde_json::json!("test-key"));
        tools_config.insert("web_search".to_string(), omnish_common::config::ConfigMap::from(ws));

        // Pre-create with custom content
        let plugin_dir = tmp.path().join("web_search");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("tool.json"), "custom").unwrap();

        auto_install_bundled_plugins(tmp.path(), &tools_config);

        // Should not overwrite
        let content = std::fs::read_to_string(plugin_dir.join("tool.json")).unwrap();
        assert_eq!(content, "custom");
    }

    const RESCAN_TOOL_JSON: &str = r#"{
        "plugin_type": "client_tool",
        "tools": [{
            "name": "rescan_tool",
            "description": "tool added post-startup",
            "input_schema": {"type": "object"},
            "status_template": ""
        }]
    }"#;

    #[test]
    fn test_rescan_picks_up_new_plugin_dir() {
        use crate::tool_registry::ToolRegistry;
        let tmp = tempfile::tempdir().unwrap();
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
        let reg = ToolRegistry::new();
        mgr.register_all(&reg);
        assert!(!reg.is_known("rescan_tool"));

        // Simulate a user dropping a new plugin directory.
        write_tool_json(tmp.path(), "newcomer", RESCAN_TOOL_JSON);
        let changed = mgr.rescan(&reg, None);
        assert!(changed);
        assert!(reg.is_known("rescan_tool"));
        assert!(reg.all_defs().iter().any(|d| d.name == "rescan_tool"));
    }

    #[test]
    fn test_rescan_removes_deleted_plugin_dir() {
        use crate::tool_registry::ToolRegistry;
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "goner", RESCAN_TOOL_JSON);
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
        let reg = ToolRegistry::new();
        mgr.register_all(&reg);
        assert!(reg.is_known("rescan_tool"));

        std::fs::remove_dir_all(tmp.path().join("goner")).unwrap();
        let changed = mgr.rescan(&reg, None);
        assert!(changed);
        assert!(!reg.is_known("rescan_tool"));
    }

    #[test]
    fn test_rescan_respects_disabled_config() {
        use crate::tool_registry::ToolRegistry;
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = HashMap::new();
        let mut entry = HashMap::new();
        entry.insert("enabled".to_string(), serde_json::json!(false));
        cfg.insert(
            "newcomer".to_string(),
            omnish_common::config::ConfigMap::from(entry),
        );
        let mgr = PluginManager::load(tmp.path(), &cfg);
        let reg = ToolRegistry::new();
        mgr.register_all(&reg);

        write_tool_json(tmp.path(), "newcomer", RESCAN_TOOL_JSON);
        mgr.rescan(&reg, None);
        assert!(!reg.is_known("rescan_tool"));

        // Config meta should still list the plugin so the config menu can
        // offer a toggle.
        let meta = mgr.config_meta();
        assert!(meta.iter().any(|m| m.name == "newcomer"));
    }

    #[test]
    fn test_rescan_preserves_plugins_when_dir_unreadable() {
        use crate::tool_registry::ToolRegistry;
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "present", RESCAN_TOOL_JSON);
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
        let reg = ToolRegistry::new();
        mgr.register_all(&reg);
        assert!(reg.is_known("rescan_tool"));

        // Simulate a transient I/O failure - plugins_dir disappears entirely.
        // rescan must refuse to infer "every plugin is gone" from a failed
        // read_dir and must leave the registry untouched.
        std::fs::remove_dir_all(tmp.path()).unwrap();
        let changed = mgr.rescan(&reg, None);
        assert!(!changed);
        assert!(
            reg.is_known("rescan_tool"),
            "rescan must not unload plugins on read_dir failure"
        );
    }

    #[test]
    fn test_rescan_unloads_when_config_disables_existing_plugin() {
        use crate::tool_registry::ToolRegistry;
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "flipper", RESCAN_TOOL_JSON);
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
        let reg = ToolRegistry::new();
        mgr.register_all(&reg);
        assert!(reg.is_known("rescan_tool"));

        // Flip config to disabled and rescan - this is the config-change path.
        let mut cfg = HashMap::new();
        let mut entry = HashMap::new();
        entry.insert("enabled".to_string(), serde_json::json!(false));
        cfg.insert(
            "flipper".to_string(),
            omnish_common::config::ConfigMap::from(entry),
        );
        mgr.update_plugins_config(&cfg);
        let changed = mgr.rescan(&reg, None);
        assert!(changed);
        assert!(!reg.is_known("rescan_tool"));

        // Flip it back and rescan - the ghost entry should be reloaded.
        let mut cfg = HashMap::new();
        let mut entry = HashMap::new();
        entry.insert("enabled".to_string(), serde_json::json!(true));
        cfg.insert(
            "flipper".to_string(),
            omnish_common::config::ConfigMap::from(entry),
        );
        mgr.update_plugins_config(&cfg);
        let changed = mgr.rescan(&reg, None);
        assert!(changed);
        assert!(reg.is_known("rescan_tool"));
    }

    #[test]
    fn test_rescan_is_idempotent_for_known_plugin() {
        use crate::tool_registry::ToolRegistry;
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "already_here", RESCAN_TOOL_JSON);
        let mgr = PluginManager::load(tmp.path(), &HashMap::new());
        let reg = ToolRegistry::new();
        mgr.register_all(&reg);
        let baseline = reg.all_defs().len();

        let changed = mgr.rescan(&reg, None);
        assert!(!changed);
        assert_eq!(reg.all_defs().len(), baseline);
    }
}
