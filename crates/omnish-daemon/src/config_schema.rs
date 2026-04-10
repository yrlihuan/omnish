//! Config schema parser and item builder.
//!
//! Parses the embedded config_schema.toml to build ConfigItem lists
//! from live DaemonConfig, and applies changes back to daemon.toml.

use std::path::Path;
use omnish_common::config::DaemonConfig;
use omnish_protocol::message::{ConfigItem, ConfigItemKind, ConfigChange, ConfigHandlerInfo};
use serde::Deserialize;

const SCHEMA_TOML: &str = include_str!("config_schema.toml");

#[derive(Debug, Deserialize)]
struct Schema {
    items: Vec<SchemaItem>,
}

#[derive(Debug, Deserialize)]
struct SchemaItem {
    path: String,
    label: String,
    kind: String,
    #[serde(default)]
    toml_key: Option<String>,
    #[serde(default)]
    options_from: Option<String>,
    #[serde(default)]
    options: Option<Vec<String>>,
    #[serde(default)]
    pub handler: Option<String>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

fn parse_schema() -> Vec<SchemaItem> {
    let schema: Schema = toml::from_str(SCHEMA_TOML)
        .expect("config_schema.toml is invalid");
    schema.items
}

/// Traverse a dot-separated path in a toml::Value tree.
fn resolve_value(doc: &toml::Value, path: &str) -> Option<String> {
    let mut val = doc;
    for seg in path.split('.') {
        val = val.get(seg)?;
    }
    val.as_str().map(|s| s.to_string())
        .or_else(|| val.as_bool().map(|b| b.to_string()))
        .or_else(|| val.as_integer().map(|i| i.to_string()))
}

/// Extract table keys from a dot-separated path.
fn resolve_options(doc: &toml::Value, table_path: &str) -> Vec<String> {
    let mut val = doc;
    for seg in table_path.split('.') {
        match val.get(seg) {
            Some(v) => val = v,
            None => return vec![],
        }
    }
    match val.as_table() {
        Some(table) => table.keys().cloned().collect(),
        None => vec![],
    }
}

/// Build ConfigItem list + handler info from live config using the embedded schema.
pub fn build_config_items(
    config: &DaemonConfig,
    plugin_metas: &[omnish_daemon::plugin::PluginConfigMeta],
) -> (Vec<ConfigItem>, Vec<ConfigHandlerInfo>) {
    let schema = parse_schema();
    let config_value = toml::Value::try_from(config)
        .expect("DaemonConfig must be Serializable");

    // Submenus and dynamic placeholders both contribute a labeled parent entry
    // to the handlers list so the client can resolve their display labels.
    let handlers: Vec<ConfigHandlerInfo> = schema.iter()
        .filter(|s| s.kind == "submenu" || s.kind == "dynamic")
        .map(|s| ConfigHandlerInfo {
            path: s.path.clone(),
            label: s.label.clone(),
            handler: s.handler.clone().unwrap_or_default(),
        })
        .collect();

    // Only submenus with an actual handler — label-only submenus don't form
    // a handler subtree, so their children are treated as independent items.
    let handler_paths: Vec<&str> = schema.iter()
        .filter(|s| s.handler.is_some())
        .map(|s| s.path.as_str())
        .collect();

    let mut items = Vec::new();
    for s in &schema {
        if s.kind == "submenu" {
            continue;
        }

        // Dynamic placeholder: expand runtime items at this position
        if s.kind == "dynamic" {
            match s.source.as_deref() {
                Some("plugins") => items.extend(build_plugin_items(config, plugin_metas)),
                Some(other) => tracing::warn!("unknown dynamic source: {}", other),
                None => tracing::warn!("dynamic item {} missing source field", s.path),
            }
            continue;
        }

        // sandbox._rules placeholder: inject rules JSON data item so the client
        // can read current global rules during placeholder expansion.
        if s.path == "sandbox._rules" {
            items.push(ConfigItem {
                path: "sandbox.__rules_json".to_string(),
                label: String::new(),
                kind: ConfigItemKind::Data { value: build_rules_json(&config.sandbox.plugins) },
                prefills: vec![],
            });
            // Fall through to emit the _client:sandbox_rules placeholder label
        }

        let under_handler = handler_paths.iter().any(|hp| s.path.starts_with(hp) && s.path != *hp);

        // Special handling for provider selector: populate from presets
        if s.path == "llm.backends.__new__.provider" {
            let chat = omnish_llm::presets::chat_providers();
            let mut options: Vec<String> = Vec::with_capacity(chat.len());
            options.push("custom".to_string());
            for p in chat {
                if p != "custom" {
                    options.push(p.clone());
                }
            }

            let mut prefills: Vec<(String, Vec<(String, String)>)> = Vec::new();
            for opt in &options {
                let fields = if let Some(preset) = omnish_llm::presets::get_provider(opt) {
                    vec![
                        ("Name".to_string(), opt.clone()),
                        ("Backend type".to_string(), preset.backend_type.clone()),
                        ("Model".to_string(), preset.default_model.clone()),
                        ("Base URL".to_string(), preset.base_url.clone()),
                        ("Context window".to_string(), preset.context_window.to_string()),
                    ]
                } else {
                    vec![
                        ("Name".to_string(), String::new()),
                        ("Backend type".to_string(), String::new()),
                        ("Model".to_string(), String::new()),
                        ("Base URL".to_string(), String::new()),
                        ("Context window".to_string(), String::new()),
                    ]
                };
                prefills.push((opt.clone(), fields));
            }

            items.push(ConfigItem {
                path: s.path.clone(),
                label: s.label.clone(),
                kind: ConfigItemKind::Select { options, selected: 0 },
                prefills,
            });
            continue;
        }

        let kind = match s.kind.as_str() {
            "text" => {
                let value = if under_handler {
                    String::new()
                } else {
                    s.toml_key.as_ref()
                        .and_then(|k| resolve_value(&config_value, k))
                        .or_else(|| s.default.clone())
                        .unwrap_or_default()
                };
                ConfigItemKind::TextInput { value }
            }
            "select" => {
                let mut options = if let Some(ref opts) = s.options {
                    opts.clone()
                } else if let Some(ref from) = s.options_from {
                    resolve_options(&config_value, from)
                } else {
                    vec![]
                };

                let current = if under_handler {
                    String::new()
                } else {
                    s.toml_key.as_ref()
                        .and_then(|k| resolve_value(&config_value, k))
                        .unwrap_or_default()
                };

                let selected = options.iter().position(|o| o == &current).unwrap_or_else(|| {
                    if !current.is_empty() {
                        options.push(current);
                        options.len() - 1
                    } else {
                        0
                    }
                });

                ConfigItemKind::Select { options, selected }
            }
            "toggle" => {
                let value = if under_handler {
                    false
                } else {
                    s.toml_key.as_ref()
                        .and_then(|k| resolve_value(&config_value, k))
                        .or_else(|| s.default.clone())
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(false)
                };
                ConfigItemKind::Toggle { value }
            }
            "label" => ConfigItemKind::Label,
            _ => continue,
        };

        items.push(ConfigItem {
            path: s.path.clone(),
            label: s.label.clone(),
            kind,
            prefills: vec![],
        });

    }

    // Dynamic items: existing backends (sorted by name)
    let mut backend_names: Vec<&String> = config.llm.backends.keys().collect();
    backend_names.sort();
    for name in backend_names {
        let backend = &config.llm.backends[name];
        // Quote name if it contains dots so config_edit treats it as a single key segment
        let quoted = if name.contains('.') { format!("\"{}\"", name) } else { name.to_string() };
        let prefix = format!("llm.backends.{}", quoted);

        items.push(ConfigItem {
            path: format!("{}.backend_type", prefix),
            label: "Backend type".to_string(),
            kind: {
                let opts = vec!["anthropic".to_string(), "openai-compat".to_string()];
                let sel = opts.iter().position(|o| o == &backend.backend_type).unwrap_or(0);
                ConfigItemKind::Select { options: opts, selected: sel }
            },
            prefills: vec![],
        });
        items.push(ConfigItem {
            path: format!("{}.model", prefix),
            label: "Model".to_string(),
            kind: ConfigItemKind::TextInput { value: backend.model.clone() },
            prefills: vec![],
        });
        items.push(ConfigItem {
            path: format!("{}.api_key_cmd", prefix),
            label: "API key command".to_string(),
            kind: ConfigItemKind::TextInput {
                value: backend.api_key_cmd.clone().unwrap_or_default(),
            },
            prefills: vec![],
        });
        items.push(ConfigItem {
            path: format!("{}.base_url", prefix),
            label: "Base URL".to_string(),
            kind: ConfigItemKind::TextInput {
                value: backend.base_url.clone().unwrap_or_default(),
            },
            prefills: vec![],
        });
        items.push(ConfigItem {
            path: format!("{}.use_proxy", prefix),
            label: "Use proxy".to_string(),
            kind: ConfigItemKind::Toggle { value: backend.use_proxy },
            prefills: vec![],
        });
        items.push(ConfigItem {
            path: format!("{}.context_window", prefix),
            label: "Context window".to_string(),
            kind: ConfigItemKind::TextInput {
                value: backend.context_window.map(|v| v.to_string()).unwrap_or_default(),
            },
            prefills: vec![],
        });
    }

    // Global sandbox rule forms are now generated client-side via the
    // _client:sandbox_rules placeholder (merged with local rules).
    // The daemon still handles edit_global_rule/add_global_rule RPCs.

    (items, handlers)
}

/// Build plugin config items from plugin metadata.
fn build_plugin_items(
    config: &DaemonConfig,
    plugin_metas: &[omnish_daemon::plugin::PluginConfigMeta],
) -> Vec<ConfigItem> {
    let mut items = Vec::new();
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
    items
}

/// Serialize current global sandbox rules to JSON for the client data item.
/// Format: `[{"plugin":"bash","rules":["command starts_with glab"]}, ...]`
fn build_rules_json(plugins: &std::collections::HashMap<String, omnish_common::config::SandboxPluginConfig>) -> String {
    let entries: Vec<serde_json::Value> = plugins.iter()
        .filter(|(_, cfg)| !cfg.permit_rules.is_empty())
        .map(|(name, cfg)| serde_json::json!({ "plugin": name, "rules": cfg.permit_rules }))
        .collect();
    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}

// Global rule form generation has moved to the client side
// (sandbox_local_rule_items in chat_session.rs). The daemon still handles
// add_global_rule / edit_global_rule RPCs via apply_config_changes.

fn find_schema_item<'a>(schema: &'a [SchemaItem], path: &str) -> Option<&'a SchemaItem> {
    schema.iter().find(|s| s.path == path)
}

fn find_handler_for_path<'a>(schema: &'a [SchemaItem], path: &str) -> Option<&'a str> {
    schema.iter()
        .filter(|s| s.handler.is_some())
        .find(|s| path.starts_with(&s.path) && path != s.path)
        .and_then(|s| s.handler.as_deref())
}

/// Resolve handlers for dynamically-generated config items (not in the TOML schema).
/// The client sends sandbox rule changes with well-known path prefixes.
fn resolve_dynamic_handler(path: &str) -> Option<String> {
    if !path.starts_with("sandbox.rules.") {
        return None;
    }
    // sandbox.rules.__add__.<field> → add_global_rule
    if path.starts_with("sandbox.rules.__add__.") {
        return Some("add_global_rule".to_string());
    }
    // sandbox.rules.__edit__.<plugin>.<idx>.<field> → edit_global_rule:<plugin>:<idx>
    if let Some(rest) = path.strip_prefix("sandbox.rules.__edit__.") {
        // rest = "<plugin>.<idx>.<field>"
        let mut parts = rest.splitn(3, '.');
        let plugin = parts.next()?;
        let idx_str = parts.next()?;
        if idx_str.parse::<usize>().is_ok() {
            return Some(format!("edit_global_rule:{}:{}", plugin, idx_str));
        }
    }
    None
}

/// Apply config changes to daemon.toml.
pub fn apply_config_changes(config_path: &Path, changes: &[ConfigChange]) -> anyhow::Result<()> {
    let schema = parse_schema();

    let mut generic: Vec<&ConfigChange> = Vec::new();
    let mut handler_groups: std::collections::HashMap<String, Vec<&ConfigChange>> = std::collections::HashMap::new();

    for change in changes {
        if let Some(handler) = find_handler_for_path(&schema, &change.path) {
            handler_groups.entry(handler.to_string()).or_default().push(change);
        } else if let Some(handler) = resolve_dynamic_handler(&change.path) {
            handler_groups.entry(handler).or_default().push(change);
        } else {
            generic.push(change);
        }
    }

    for change in &generic {
        let item = find_schema_item(&schema, &change.path);
        let toml_key = item.and_then(|s| s.toml_key.as_deref()).unwrap_or(&change.path);
        let kind = item.map(|s| s.kind.as_str()).unwrap_or_else(|| {
            if change.path.ends_with(".use_proxy")
                || (change.path.starts_with("plugins.") && change.path.ends_with(".enabled"))
            { "toggle" }
            else { "text" }
        });

        match kind {
            "label" => continue,
            "toggle" => {
                let bool_val: bool = change.value.parse().unwrap_or(false);
                omnish_common::config_edit::set_toml_value_nested_bool(config_path, toml_key, bool_val)?;
            }
            _ => {
                omnish_common::config_edit::set_toml_value_nested(config_path, toml_key, &change.value)?;
            }
        }
    }

    for (handler, changes) in &handler_groups {
        if handler == "add_backend" {
            handle_add_backend(config_path, changes)?;
        } else if handler == "add_global_rule" {
            handle_add_global_rule(config_path, changes)?;
        } else if handler.starts_with("edit_global_rule:") {
            // handler format: "edit_global_rule:<plugin>:<index>"
            let rest = &handler["edit_global_rule:".len()..];
            let colon = rest.rfind(':').ok_or_else(|| anyhow::anyhow!("malformed handler: {}", handler))?;
            let plugin = &rest[..colon];
            let idx: usize = rest[colon+1..].parse()
                .map_err(|_| anyhow::anyhow!("invalid index in handler: {}", handler))?;
            handle_edit_global_rule(config_path, plugin, idx, changes)?;
        } else {
            anyhow::bail!("unknown handler: {}", handler);
        }
    }

    Ok(())
}

fn handle_add_global_rule(config_path: &Path, changes: &[&ConfigChange]) -> anyhow::Result<()> {
    let plugin = changes.iter().find(|c| c.path.ends_with(".plugin"))
        .map(|c| c.value.as_str()).unwrap_or("").trim().to_string();
    let field = changes.iter().find(|c| c.path.ends_with(".field"))
        .map(|c| c.value.as_str()).unwrap_or("").trim().to_string();
    let operator = changes.iter().find(|c| c.path.ends_with(".operator"))
        .map(|c| c.value.as_str()).unwrap_or("starts_with").trim().to_string();
    let operator = if operator.is_empty() { "starts_with".to_string() } else { operator };
    let value = changes.iter().find(|c| c.path.ends_with(".value"))
        .map(|c| c.value.as_str()).unwrap_or("").trim().to_string();

    if plugin.is_empty() { anyhow::bail!("add_global_rule: plugin is required"); }
    if field.is_empty()  { anyhow::bail!("add_global_rule: field is required"); }
    if value.is_empty()  { anyhow::bail!("add_global_rule: pattern is required"); }

    let rule = format!("{} {} {}", field, operator, value);
    let array_key = format!("sandbox.plugins.{}.permit_rules", plugin);
    omnish_common::config_edit::append_to_toml_array(config_path, &array_key, &rule)?;
    Ok(())
}

fn handle_edit_global_rule(
    config_path: &Path,
    plugin: &str,
    idx: usize,
    changes: &[&ConfigChange],
) -> anyhow::Result<()> {
    let delete = changes.iter().find(|c| c.path.ends_with(".Delete"))
        .map(|c| c.value == "true").unwrap_or(false);
    let array_key = format!("sandbox.plugins.{}.permit_rules", plugin);

    if delete {
        omnish_common::config_edit::remove_from_toml_array(config_path, &array_key, idx)?;
        return Ok(());
    }

    let field = changes.iter().find(|c| c.path.ends_with(".field"))
        .map(|c| c.value.as_str()).unwrap_or("").trim().to_string();
    let operator = changes.iter().find(|c| c.path.ends_with(".operator"))
        .map(|c| c.value.as_str()).unwrap_or("starts_with").trim().to_string();
    let operator = if operator.is_empty() { "starts_with".to_string() } else { operator };
    let value = changes.iter().find(|c| c.path.ends_with(".value"))
        .map(|c| c.value.as_str()).unwrap_or("").trim().to_string();

    if field.is_empty() { anyhow::bail!("edit_global_rule: field is required"); }
    if value.is_empty() { anyhow::bail!("edit_global_rule: pattern is required"); }

    let rule = format!("{} {} {}", field, operator, value);
    omnish_common::config_edit::replace_in_toml_array(config_path, &array_key, idx, &rule)?;
    Ok(())
}

fn handle_add_backend(config_path: &Path, changes: &[&ConfigChange]) -> anyhow::Result<()> {
    let name = changes.iter()
        .find(|c| c.path.ends_with(".name"))
        .map(|c| c.value.as_str())
        .ok_or_else(|| anyhow::anyhow!("add_backend: name field is required"))?;

    if name.is_empty() {
        anyhow::bail!("add_backend: name cannot be empty");
    }

    // Quote backend name if it contains dots (e.g. "gemini-3.1" → "\"gemini-3.1\"")
    // so config_edit's split_key_path treats it as a single segment.
    let quoted_name = if name.contains('.') {
        format!("\"{}\"", name)
    } else {
        name.to_string()
    };

    for change in changes {
        if change.path.ends_with(".name") || change.path.ends_with(".provider") {
            continue;
        }
        let field = change.path.rsplit('.').next().unwrap_or("");
        if field.is_empty() {
            continue;
        }
        // Convert plain api_key input to api_key_cmd = "echo <key>"
        if field == "api_key" {
            if !change.value.is_empty() {
                let toml_key = format!("llm.backends.{}.api_key_cmd", quoted_name);
                let cmd_value = format!("echo {}", change.value);
                omnish_common::config_edit::set_toml_value_nested(config_path, &toml_key, &cmd_value)?;
            }
            continue;
        }
        let toml_key = format!("llm.backends.{}.{}", quoted_name, field);
        if field == "use_proxy" {
            let bool_val: bool = change.value.parse().unwrap_or(false);
            omnish_common::config_edit::set_toml_value_nested_bool(config_path, &toml_key, bool_val)?;
        } else if field == "context_window" {
            if let Ok(int_val) = change.value.parse::<i64>() {
                omnish_common::config_edit::set_toml_value_nested_int(config_path, &toml_key, int_val)?;
            }
        } else {
            omnish_common::config_edit::set_toml_value_nested(config_path, &toml_key, &change.value)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnish_common::config::DaemonConfig;

    #[test]
    fn test_parse_schema() {
        let schema = parse_schema();
        assert!(!schema.is_empty());
        assert!(schema.iter().any(|s| s.path == "general.proxy.http_proxy"));
        assert!(schema.iter().any(|s| s.path == "general.hotkeys.command_prefix"));
        assert!(schema.iter().any(|s| s.path == "llm.use_cases.completion"));
        assert!(schema.iter().any(|s| s.path == "llm.backends.__new__" && s.handler.is_some()));
        assert!(schema.iter().any(|s| s.path == "tasks.hourly_summary.enabled"));
        assert!(schema.iter().any(|s| s.path == "tasks.daily_summary.enabled"));
    }

    #[test]
    fn test_resolve_value_top_level() {
        let mut config = DaemonConfig::default();
        config.proxy = omnish_common::config::ProxyConfig {
            http_proxy: Some("http://proxy:8080".to_string()),
            ..Default::default()
        };
        let val = toml::Value::try_from(&config).unwrap();
        assert_eq!(resolve_value(&val, "proxy.http_proxy"), Some("http://proxy:8080".to_string()));
    }

    #[test]
    fn test_resolve_value_nested() {
        let config = DaemonConfig::default();
        let val = toml::Value::try_from(&config).unwrap();
        assert_eq!(resolve_value(&val, "llm.default"), Some("claude".to_string()));
    }

    #[test]
    fn test_resolve_options_from_backends() {
        let mut config = DaemonConfig::default();
        config.llm.backends.insert("claude".to_string(), omnish_common::config::LlmBackendConfig {
            backend_type: "anthropic".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            api_key_cmd: None,
            base_url: None,
            use_proxy: false,
            context_window: None,
            max_content_chars: None,
        });
        config.llm.backends.insert("openai".to_string(), omnish_common::config::LlmBackendConfig {
            backend_type: "openai-compat".to_string(),
            model: "gpt-4o".to_string(),
            api_key_cmd: None,
            base_url: Some("https://api.openai.com".to_string()),
            use_proxy: false,
            context_window: None,
            max_content_chars: None,
        });
        let val = toml::Value::try_from(&config).unwrap();
        let options = resolve_options(&val, "llm.backends");
        assert_eq!(options.len(), 2);
        assert!(options.contains(&"claude".to_string()));
        assert!(options.contains(&"openai".to_string()));
    }

    #[test]
    fn test_build_config_items_includes_leaf_items() {
        let config = DaemonConfig::default();
        let (items, _handlers) = build_config_items(&config, &[]);
        assert!(items.iter().any(|i| i.path == "general.proxy.http_proxy"));
        assert!(items.iter().any(|i| i.path == "general.hotkeys.command_prefix"));
        assert!(items.iter().any(|i| i.path == "llm.use_cases.completion"));
        assert!(items.iter().any(|i| i.path == "llm.backends.__new__.name"));
        assert!(items.iter().any(|i| i.path == "tasks.hourly_summary.enabled"));
        assert!(items.iter().any(|i| i.path == "tasks.daily_summary.enabled"));
    }

    #[test]
    fn test_build_config_items_returns_handlers() {
        let config = DaemonConfig::default();
        let (_items, handlers) = build_config_items(&config, &[]);
        // Label-only submenus (llm, shell_completion, sandbox) + dynamic placeholder (plugins)
        // + handler submenu (add_backend)
        // Note: add_global_rule is now generated client-side
        assert_eq!(handlers.len(), 5);
        let llm = handlers.iter().find(|h| h.path == "llm").unwrap();
        assert_eq!(llm.label, "LLM");
        assert_eq!(llm.handler, "");
        let add = handlers.iter().find(|h| h.path == "llm.backends.__new__").unwrap();
        assert_eq!(add.handler, "add_backend");
        assert_eq!(add.label, "Add backend");
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
        assert!(items.iter().any(|i| i.path == "llm.backends.claude.backend_type"));
        assert!(items.iter().any(|i| i.path == "llm.backends.claude.model"));
        assert!(items.iter().any(|i| i.path == "llm.backends.claude.api_key_cmd"));
        let model_item = items.iter().find(|i| i.path == "llm.backends.claude.model").unwrap();
        match &model_item.kind {
            ConfigItemKind::TextInput { value } => assert_eq!(value, "claude-sonnet-4-5-20250929"),
            _ => panic!("expected TextInput"),
        }
    }

    #[test]
    fn test_build_config_items_generates_plugin_items() {
        let config = DaemonConfig::default();
        let metas = vec![omnish_daemon::plugin::PluginConfigMeta {
            name: "web_search".to_string(),
            config_params: vec![omnish_daemon::plugin::ConfigParam {
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
        use std::collections::HashMap;
        let mut config = DaemonConfig::default();
        let mut ws_cfg = HashMap::new();
        ws_cfg.insert("enabled".to_string(), serde_json::Value::Bool(false));
        ws_cfg.insert("api_key".to_string(), serde_json::Value::String("sk-test".to_string()));
        config.plugins.insert("web_search".to_string(), ws_cfg.into());

        let metas = vec![omnish_daemon::plugin::PluginConfigMeta {
            name: "web_search".to_string(),
            config_params: vec![omnish_daemon::plugin::ConfigParam {
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

    /// Guard test: every config schema path must map to a ConfigSection that
    /// has diff + notify implemented in ConfigWatcher::reload().
    ///
    /// When you add a new section to config_schema.toml, this test will fail
    /// until the corresponding ConfigSection is added to WATCHED_SECTIONS
    /// and its diff logic is implemented in config_watcher.rs.
    ///
    /// Run with: cargo test -p omnish-daemon -- --ignored test_all_schema_paths
    #[test]
    #[ignore]
    fn test_all_schema_paths_covered_by_config_watcher() {
        use crate::config_watcher::{ConfigSection, ConfigWatcher};
        use std::collections::HashSet;

        let watched: HashSet<ConfigSection> = ConfigWatcher::WATCHED_SECTIONS.iter().copied().collect();
        let schema = parse_schema();

        let mut missing: Vec<(String, ConfigSection)> = Vec::new();

        for item in &schema {
            // Use toml_key if present, otherwise derive from path
            let toml_root = item.toml_key.as_deref()
                .unwrap_or(&item.path)
                .split('.')
                .next()
                .unwrap_or("");

            if let Some(section) = ConfigSection::from_toml_key(toml_root) {
                if !watched.contains(&section) {
                    missing.push((item.path.clone(), section));
                }
            }
            // Items with no ConfigSection mapping (e.g. listen_addr) are
            // intentionally excluded — they require a daemon restart.
        }

        // Deduplicate by section for a cleaner error message
        let mut missing_sections: Vec<ConfigSection> = missing.iter()
            .map(|(_, s)| *s)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        missing_sections.sort_by_key(|s| format!("{:?}", s));

        assert!(
            missing_sections.is_empty(),
            "Config schema has items in sections not covered by ConfigWatcher::WATCHED_SECTIONS.\n\
             Missing sections: {:?}\n\
             Example paths: {:?}\n\
             Add these sections to WATCHED_SECTIONS and implement their diff in reload().",
            missing_sections,
            missing.iter().take(5).map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
        );
    }
}
