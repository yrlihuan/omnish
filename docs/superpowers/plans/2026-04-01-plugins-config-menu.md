# Plugins Config Menu Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add plugin enable/disable toggles and parameter configuration to the `/config` interactive menu.

**Architecture:** Plugins declare configurable parameters via `config_params` in tool.json. `build_config_items` generates Toggle/TextInput items per plugin. Config is stored under `[plugins.<name>]` in daemon.toml, replacing the old `[tools.<name>]` section.

**Tech Stack:** Rust, serde, toml_edit

---

### Task 1: Update DaemonConfig - merge `tools` into `plugins`

**Files:**
- Modify: `crates/omnish-common/src/config.rs`

- [ ] **Step 1: Replace PluginsConfig and tools field**

Remove the `PluginsConfig` struct and change the `plugins` and `tools` fields in `DaemonConfig`:

```rust
// DELETE the PluginsConfig struct entirely (lines 253-259)

// In DaemonConfig, replace:
//   pub plugins: PluginsConfig,
//   pub tools: HashMap<String, HashMap<String, serde_json::Value>>,
// With:
    /// Per-plugin configuration.
    /// [plugins.web_search]
    /// enabled = false
    /// api_key = "..."
    #[serde(default)]
    pub plugins: HashMap<String, HashMap<String, serde_json::Value>>,
```

Also remove `#[serde(skip_serializing)]` - the new `plugins` field should serialize normally.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p omnish-common --release 2>&1 | head -20`
Expected: Success (omnish-common itself should compile; downstream crates will fail - that's Task 2-3).

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-common/src/config.rs
git commit -m "refactor: merge tools into plugins in DaemonConfig (#484)"
```

---

### Task 2: Update plugin.rs - fix auto_install, change load() signature, add config_params

**Files:**
- Modify: `crates/omnish-daemon/src/plugin.rs`

- [ ] **Step 1: Update auto_install_bundled_plugins**

Change `tools_config` references to `plugins_config` - the parameter name and lookup key change from `"web_search"` tool to `"web_search"` plugin (same key, different semantic):

```rust
pub fn auto_install_bundled_plugins(
    plugins_dir: &Path,
    plugins_config: &HashMap<String, HashMap<String, serde_json::Value>>,
) {
    // web_search: install if [plugins.web_search] has api_key
    if let Some(ws_config) = plugins_config.get("web_search") {
        if ws_config.contains_key("api_key") {
            // ... rest unchanged ...
```

- [ ] **Step 2: Add ConfigParam and PluginConfigMeta structs**

Add these public structs near the top of the file (after `PluginType`):

```rust
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
```

- [ ] **Step 3: Extend ToolJsonFile to parse config_params**

Add the field to `ToolJsonFile`:

```rust
#[derive(Deserialize)]
struct ToolJsonFile {
    plugin_type: String,
    #[serde(default)]
    formatter_binary: Option<String>,
    #[serde(default)]
    config_params: Vec<ConfigParam>,
    tools: Vec<ToolJsonEntry>,
}
```

- [ ] **Step 4: Store config_params in PluginInfo**

Add field to `PluginInfo`:

```rust
#[derive(Debug)]
struct PluginInfo {
    dir_name: String,
    plugin_type: PluginType,
    tools: Vec<ToolEntry>,
    formatter_binary: Option<String>,
    config_params: Vec<ConfigParam>,
}
```

Update all `PluginInfo` construction sites to include `config_params`. For the builtin plugin (constructed when parsing `BUILTIN_TOOL_JSON`), use `config_params: parsed.config_params` (will be empty `[]`). For each scanned plugin directory, use `config_params: parsed.config_params`.

- [ ] **Step 5: Change load() to accept plugins_config and respect enabled flag**

```rust
pub fn load(
    plugins_dir: &Path,
    plugins_config: &HashMap<String, HashMap<String, serde_json::Value>>,
) -> Self {
```

In the directory scan loop, after parsing `ToolJsonFile`, add enabled check:

```rust
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
```

The rest of the load logic (tool registration, tool_index building) stays the same - disabled plugins have empty `tools` vec so they contribute nothing to the index.

- [ ] **Step 6: Add config_meta() method**

```rust
/// Returns plugin metadata for the config menu (all non-builtin plugins).
pub fn config_meta(&self) -> Vec<PluginConfigMeta> {
    self.plugins
        .iter()
        .filter(|p| p.dir_name != "builtin")
        .map(|p| PluginConfigMeta {
            name: p.dir_name.clone(),
            config_params: p.config_params.clone(),
        })
        .collect()
}
```

- [ ] **Step 7: Verify it compiles**

Run: `cargo check -p omnish-daemon --release 2>&1 | head -30`
Expected: Compilation errors in main.rs and server.rs (callsite mismatches) - fixed in Task 3.

- [ ] **Step 8: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs
git commit -m "feat: add config_params support and enabled flag to PluginManager (#484)"
```

---

### Task 3: Fix callsites - main.rs and server.rs

**Files:**
- Modify: `crates/omnish-daemon/src/main.rs`
- Modify: `crates/omnish-daemon/src/server.rs`

- [ ] **Step 1: Update main.rs**

Change `auto_install_bundled_plugins` call (around line 273):

```rust
omnish_daemon::plugin::auto_install_bundled_plugins(&plugins_dir, &config.plugins);
```

Change `PluginManager::load` call (around line 276):

```rust
let plugin_mgr = Arc::new(omnish_daemon::plugin::PluginManager::load(&plugins_dir, &config.plugins));
```

Change `DaemonServer::new` call (around line 392) - replace `config.tools` with `config.plugins`:

```rust
let server = DaemonServer::new(session_mgr, llm_backend, task_mgr, conv_mgr, plugin_mgr, tool_registry, config.plugins, server_opts, formatter_mgr, Arc::clone(&update_cache));
```

- [ ] **Step 2: Filter "enabled" in merge_tool_params calls in server.rs**

In the agent loop (two sites around lines 1416 and 1449), change from:

```rust
if let Some(config_params) = tool_params.get(&tc.name) {
    merge_tool_params(&mut merged_input, config_params);
}
```

To:

```rust
if let Some(config_params) = tool_params.get(&tc.name) {
    let filtered: HashMap<String, serde_json::Value> = config_params
        .iter()
        .filter(|(k, _)| k.as_str() != "enabled")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    merge_tool_params(&mut merged_input, &filtered);
}
```

Apply this change to BOTH the client-tool path and the daemon-tool path.

- [ ] **Step 3: Build the whole workspace**

Run: `cargo build --release 2>&1 | tail -5`
Expected: Successful build.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/main.rs crates/omnish-daemon/src/server.rs
git commit -m "refactor: update callsites for plugins config migration (#484)"
```

---

### Task 4: Generate plugin items in config_schema

**Files:**
- Modify: `crates/omnish-daemon/src/config_schema.rs`

- [ ] **Step 1: Change build_config_items signature**

```rust
pub fn build_config_items(
    config: &DaemonConfig,
    plugin_metas: &[crate::plugin::PluginConfigMeta],
) -> (Vec<ConfigItem>, Vec<ConfigHandlerInfo>) {
```

- [ ] **Step 2: Add plugin item generation at the end of build_config_items**

Before the final `(items, handlers)` return, add:

```rust
    // ── Plugin items ──────────────────────────────────────────
    for meta in plugin_metas {
        let plugin_cfg = config.plugins.get(&meta.name);

        // Enabled toggle (default: true)
        let enabled = plugin_cfg
            .and_then(|cfg| cfg.get("enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        items.push(ConfigItem {
            path: format!("plugins.{}.enabled", meta.name),
            label: "Enabled".to_string(),
            kind: ConfigItemKind::Toggle { value: enabled },
            prefills: Vec::new(),
        });

        // Config params declared in tool.json
        for param in &meta.config_params {
            let value = plugin_cfg
                .and_then(|cfg| cfg.get(&param.name))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            items.push(ConfigItem {
                path: format!("plugins.{}.{}", meta.name, param.name),
                label: param.label.clone(),
                kind: ConfigItemKind::TextInput { value },
                prefills: Vec::new(),
            });
        }
    }
```

- [ ] **Step 3: Handle plugins.*.enabled as boolean in apply_config_changes**

In `apply_config_changes`, update the kind inference for generic changes. Change the existing fallback:

```rust
        let kind = item.map(|s| s.kind.as_str()).unwrap_or_else(|| {
            if change.path.ends_with(".use_proxy") { "toggle" }
            else if change.path.starts_with("plugins.") && change.path.ends_with(".enabled") { "toggle" }
            else { "text" }
        });
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p omnish-daemon --release 2>&1 | head -20`
Expected: Compilation errors in server.rs where `build_config_items` is called without the new parameter - fixed in Task 5.

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/config_schema.rs
git commit -m "feat: generate plugin toggle and param items in config menu (#484)"
```

---

### Task 5: Wire up ConfigQuery with plugin metadata

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

- [ ] **Step 1: Update ConfigQuery handler**

In the `Message::ConfigQuery` arm (around line 824), pass plugin metadata:

```rust
Message::ConfigQuery => {
    let config = match omnish_common::config::load_daemon_config() {
        Ok(fresh) => {
            *opts.daemon_config.write().unwrap() = fresh.clone();
            fresh
        }
        Err(_) => opts.daemon_config.read().unwrap().clone(),
    };
    let plugin_metas = plugin_mgr.config_meta();
    let (items, handlers) = crate::config_schema::build_config_items(&config, &plugin_metas);
    let _ = tx.send(Message::ConfigResponse { items, handlers }).await;
}
```

- [ ] **Step 2: Build the whole workspace**

Run: `cargo build --release 2>&1 | tail -5`
Expected: Successful build.

- [ ] **Step 3: Run existing tests**

Run: `cargo test -p omnish-daemon --release 2>&1 | tail -20`
Expected: Some config_schema tests fail (they call `build_config_items` without plugin_metas). Fixed in Task 7.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "feat: pass plugin metadata to config menu builder (#484)"
```

---

### Task 6: Update web_search tool.json and daemon.toml

**Files:**
- Modify: `plugins/web_search/tool.json`
- Modify: `config/daemon.toml`

- [ ] **Step 1: Add config_params to web_search tool.json**

Add the `config_params` field after `"plugin_type"`:

```json
{
  "plugin_type": "daemon_tool",
  "config_params": [
    { "name": "api_key", "label": "API key" },
    { "name": "base_url", "label": "Base URL" }
  ],
  "formatter_binary": "web_search_formatter",
  "tools": [
```

- [ ] **Step 2: Migrate daemon.toml from [tools.*] to [plugins.*]**

Replace the `[tools.web_search]` section at the end of `config/daemon.toml`:

```toml
# Per-plugin configuration (values merged into tool call inputs)
# [plugins.web_search]
# api_key = "BSAxxxxxxxx"
# base_url = "https://api.search.brave.com/res/v1/web/search"
```

- [ ] **Step 3: Commit**

```bash
git add plugins/web_search/tool.json config/daemon.toml
git commit -m "feat: add config_params to web_search, migrate tools to plugins in daemon.toml (#484)"
```

---

### Task 7: Update tests

**Files:**
- Modify: `crates/omnish-daemon/src/config_schema.rs` (test module)

- [ ] **Step 1: Fix existing tests to pass empty plugin_metas**

Update all `build_config_items` calls in tests to pass `&[]`:

```rust
    #[test]
    fn test_build_config_items_includes_leaf_items() {
        let config = DaemonConfig::default();
        let (items, _handlers) = build_config_items(&config, &[]);
        assert!(items.iter().any(|i| i.path == "proxy.http_proxy"));
        assert!(items.iter().any(|i| i.path == "llm.use_cases.completion"));
        assert!(items.iter().any(|i| i.path == "llm.backends.__new__.name"));
    }

    #[test]
    fn test_build_config_items_returns_handlers() {
        let config = DaemonConfig::default();
        let (_items, handlers) = build_config_items(&config, &[]);
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].path, "llm.backends.__new__");
        assert_eq!(handlers[0].handler, "add_backend");
        assert_eq!(handlers[0].label, "Add backend");
    }

    #[test]
    fn test_build_config_items_generates_existing_backend_items() {
        let mut config = DaemonConfig::default();
        config.llm.backends.insert("claude".to_string(), omnish_common::config::LlmBackendConfig {
            backend_type: "anthropic".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            api_key_cmd: Some("pass show claude-key".to_string()),
            base_url: None,
            use_proxy: false,
            context_window: None,
            max_content_chars: None,
        });
        let (items, _handlers) = build_config_items(&config, &[]);
        // ... rest unchanged ...
    }
```

- [ ] **Step 2: Add test for plugin config items**

```rust
    #[test]
    fn test_build_config_items_generates_plugin_items() {
        let config = DaemonConfig::default();
        let metas = vec![crate::plugin::PluginConfigMeta {
            name: "web_search".to_string(),
            config_params: vec![crate::plugin::ConfigParam {
                name: "api_key".to_string(),
                label: "API key".to_string(),
                kind: "text".to_string(),
            }],
        }];
        let (items, _handlers) = build_config_items(&config, &metas);

        // Enabled toggle
        let toggle = items.iter().find(|i| i.path == "plugins.web_search.enabled").unwrap();
        match &toggle.kind {
            ConfigItemKind::Toggle { value } => assert!(value, "default should be enabled"),
            _ => panic!("expected Toggle"),
        }

        // API key text input
        let api_key = items.iter().find(|i| i.path == "plugins.web_search.api_key").unwrap();
        match &api_key.kind {
            ConfigItemKind::TextInput { value } => assert_eq!(value, ""),
            _ => panic!("expected TextInput"),
        }
    }

    #[test]
    fn test_build_config_items_plugin_reads_existing_values() {
        let mut config = DaemonConfig::default();
        let mut ws_cfg = HashMap::new();
        ws_cfg.insert("enabled".to_string(), serde_json::Value::Bool(false));
        ws_cfg.insert("api_key".to_string(), serde_json::Value::String("sk-test".to_string()));
        config.plugins.insert("web_search".to_string(), ws_cfg);

        let metas = vec![crate::plugin::PluginConfigMeta {
            name: "web_search".to_string(),
            config_params: vec![crate::plugin::ConfigParam {
                name: "api_key".to_string(),
                label: "API key".to_string(),
                kind: "text".to_string(),
            }],
        }];
        let (items, _handlers) = build_config_items(&config, &metas);

        let toggle = items.iter().find(|i| i.path == "plugins.web_search.enabled").unwrap();
        match &toggle.kind {
            ConfigItemKind::Toggle { value } => assert!(!value, "should be disabled"),
            _ => panic!("expected Toggle"),
        }

        let api_key = items.iter().find(|i| i.path == "plugins.web_search.api_key").unwrap();
        match &api_key.kind {
            ConfigItemKind::TextInput { value } => assert_eq!(value, "sk-test"),
            _ => panic!("expected TextInput"),
        }
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p omnish-daemon --release 2>&1 | tail -20`
Expected: All tests pass.

- [ ] **Step 4: Full workspace build**

Run: `cargo build --release 2>&1 | tail -5`
Expected: Successful build.

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/config_schema.rs
git commit -m "test: update and add config_schema tests for plugin items (#484)"
```
