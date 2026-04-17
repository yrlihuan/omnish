# Plugins Configuration in /config Menu

GitLab issue: #484

## Overview

Add plugin management to the `/config` interactive menu. Users can enable/disable plugins and configure plugin parameters (e.g. API keys) without manually editing daemon.toml.

## Design Decisions

- **Default-enable**: all discovered plugins are active unless explicitly disabled. No `plugins.enabled` list.
- **Unified config key**: plugin state and params live together under `[plugins.<name>]` in daemon.toml, replacing the old `[tools.<name>]` section.
- **Declared params**: plugins declare configurable parameters via `config_params` in tool.json. No freeform param entry.
- **No protocol changes**: uses existing ConfigItem/ConfigChange types.
- **No client changes**: the menu rendering is fully driven by the items the daemon returns.

## Data Model

### daemon.toml

```toml
[plugins.web_search]
enabled = false        # omit or true = active; false = skipped
api_key = "sk-..."
```

### DaemonConfig

Replace `PluginsConfig { enabled: Vec<String> }` and `tools: HashMap<String, HashMap<String, Value>>` with a single field:

```rust
pub plugins: HashMap<String, HashMap<String, serde_json::Value>>,
```

The `enabled` key is special-cased: absent or `true` means active, `false` means the plugin is not loaded. All other keys are passed to the plugin as runtime params (with `enabled` filtered out).

### tool.json extension

Add optional `config_params` at the plugin level:

```json
{
  "plugin_type": "client_tool",
  "config_params": [
    { "name": "api_key", "label": "API Key", "kind": "text" }
  ],
  "tools": [...]
}
```

`config_params` is per-plugin, not per-tool. `kind` supports `text` only for now.

### Plugin metadata structs

Exposed by PluginManager for config menu consumption:

```rust
pub struct PluginConfigMeta {
    pub name: String,                    // dir name, e.g. "web_search"
    pub config_params: Vec<ConfigParam>, // from tool.json
}

pub struct ConfigParam {
    pub name: String,   // "api_key"
    pub label: String,  // "API Key"
    pub kind: String,   // "text"
}
```

`PluginManager::config_meta()` returns metadata for all non-builtin plugins.

## Config Menu Generation

`build_config_items` signature changes:

```rust
pub fn build_config_items(
    config: &DaemonConfig,
    plugin_metas: &[PluginConfigMeta],
) -> (Vec<ConfigItem>, Vec<ConfigHandlerInfo>)
```

Generated menu structure:

```
Plugins
├── Web Search
│   ├── Enabled          [ON]       ← Toggle
│   └── API Key          "sk-..."   ← TextInput
├── Another Plugin
│   ├── Enabled          [ON]
│   └── ...
```

For each `PluginConfigMeta`:

1. Generate a Toggle at path `plugins.<name>.enabled`, current value from `config.plugins["<name>"]["enabled"]` (default true).
2. For each `config_param`, generate a TextInput at path `plugins.<name>.<param_name>`, current value from `config.plugins["<name>"]["<param_name>"]` (default empty).

Each item saves individually (no handler/form needed).

## Config Change Application

No new handler needed. Generic path in `apply_config_changes`:

- Path matching `plugins.*.enabled`: use `set_toml_value_nested_bool()`
- All other `plugins.*.*` paths: use `set_toml_value_nested()` (text)

## Plugin Loading Changes

`PluginManager::load()` signature changes:

```rust
pub fn load(
    plugins_dir: &Path,
    plugins_config: &HashMap<String, HashMap<String, serde_json::Value>>,
) -> Self
```

Changes:
1. Parse `config_params` from tool.json, store in `PluginInfo`.
2. Check `plugins_config[dir_name]["enabled"]` - if explicitly `false`, skip loading tools. Still parse `config_params` so the config menu can show the plugin.
3. Merge non-`enabled` params from `plugins_config` as override params (replaces current `tools` config role).

## Files Changed

| File | Change |
|------|--------|
| `omnish-common/src/config.rs` | Replace `PluginsConfig` with `plugins: HashMap<String, HashMap<String, Value>>`, remove `tools` field |
| `omnish-daemon/src/plugin.rs` | Add `ConfigParam`/`PluginConfigMeta` structs, parse `config_params` from tool.json, add `config_meta()`, accept plugins config in `load()`, respect `enabled` flag, use `plugins.*` params instead of `tools.*` |
| `omnish-daemon/src/config_schema.rs` | Extend `build_config_items` to accept `&[PluginConfigMeta]`, generate toggle + text items per plugin; handle `plugins.*.enabled` as boolean in `apply_config_changes` |
| `omnish-daemon/src/server.rs` | Pass plugin metadata when calling `build_config_items` |
| `omnish-daemon/src/main.rs` | Update `auto_install_bundled_plugins` and `load()` calls to use `config.plugins` |
| `plugins/web_search/tool.json` | Add `config_params` declaration |
| `config/daemon.toml` | Migrate `[tools.*]` examples to `[plugins.*]` |
