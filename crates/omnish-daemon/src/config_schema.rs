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
pub fn build_config_items(config: &DaemonConfig) -> (Vec<ConfigItem>, Vec<ConfigHandlerInfo>) {
    let schema = parse_schema();
    let config_value = toml::Value::try_from(config)
        .expect("DaemonConfig must be Serializable");

    let handlers: Vec<ConfigHandlerInfo> = schema.iter()
        .filter(|s| s.handler.is_some())
        .map(|s| ConfigHandlerInfo {
            path: s.path.clone(),
            label: s.label.clone(),
            handler: s.handler.clone().unwrap(),
        })
        .collect();

    let handler_paths: Vec<&str> = schema.iter()
        .filter(|s| s.handler.is_some())
        .map(|s| s.path.as_str())
        .collect();

    let mut items = Vec::new();
    for s in &schema {
        if s.kind == "submenu" {
            continue;
        }

        let under_handler = handler_paths.iter().any(|hp| s.path.starts_with(hp) && s.path != *hp);

        let kind = match s.kind.as_str() {
            "text" => {
                let value = if under_handler {
                    String::new()
                } else {
                    s.toml_key.as_ref()
                        .and_then(|k| resolve_value(&config_value, k))
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
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(false)
                };
                ConfigItemKind::Toggle { value }
            }
            _ => continue,
        };

        items.push(ConfigItem {
            path: s.path.clone(),
            label: s.label.clone(),
            kind,
        });
    }

    // Dynamic items: existing backends (sorted by name)
    let mut backend_names: Vec<&String> = config.llm.backends.keys().collect();
    backend_names.sort();
    for name in backend_names {
        let backend = &config.llm.backends[name];
        let prefix = format!("llm.backends.{}", name);

        items.push(ConfigItem {
            path: format!("{}.backend_type", prefix),
            label: "Backend type".to_string(),
            kind: {
                let opts = vec!["anthropic".to_string(), "openai_compat".to_string()];
                let sel = opts.iter().position(|o| o == &backend.backend_type).unwrap_or(0);
                ConfigItemKind::Select { options: opts, selected: sel }
            },
        });
        items.push(ConfigItem {
            path: format!("{}.model", prefix),
            label: "Model".to_string(),
            kind: ConfigItemKind::TextInput { value: backend.model.clone() },
        });
        items.push(ConfigItem {
            path: format!("{}.api_key_cmd", prefix),
            label: "API key command".to_string(),
            kind: ConfigItemKind::TextInput {
                value: backend.api_key_cmd.clone().unwrap_or_default(),
            },
        });
        items.push(ConfigItem {
            path: format!("{}.base_url", prefix),
            label: "Base URL".to_string(),
            kind: ConfigItemKind::TextInput {
                value: backend.base_url.clone().unwrap_or_default(),
            },
        });
    }

    (items, handlers)
}

fn find_schema_item<'a>(schema: &'a [SchemaItem], path: &str) -> Option<&'a SchemaItem> {
    schema.iter().find(|s| s.path == path)
}

fn find_handler_for_path<'a>(schema: &'a [SchemaItem], path: &str) -> Option<&'a str> {
    schema.iter()
        .filter(|s| s.handler.is_some())
        .find(|s| path.starts_with(&s.path) && path != s.path)
        .and_then(|s| s.handler.as_deref())
}

/// Apply config changes to daemon.toml.
pub fn apply_config_changes(config_path: &Path, changes: &[ConfigChange]) -> anyhow::Result<()> {
    let schema = parse_schema();

    let mut generic: Vec<&ConfigChange> = Vec::new();
    let mut handler_groups: std::collections::HashMap<String, Vec<&ConfigChange>> = std::collections::HashMap::new();

    for change in changes {
        if let Some(handler) = find_handler_for_path(&schema, &change.path) {
            handler_groups.entry(handler.to_string()).or_default().push(change);
        } else {
            generic.push(change);
        }
    }

    for change in &generic {
        let item = find_schema_item(&schema, &change.path);
        let toml_key = item.and_then(|s| s.toml_key.as_deref()).unwrap_or(&change.path);
        let kind = item.map(|s| s.kind.as_str()).unwrap_or("text");

        match kind {
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
        match handler.as_str() {
            "add_backend" => handle_add_backend(config_path, changes)?,
            other => anyhow::bail!("unknown handler: {}", other),
        }
    }

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

    for change in changes {
        if change.path.ends_with(".name") {
            continue;
        }
        let field = change.path.rsplit('.').next().unwrap_or("");
        if field.is_empty() {
            continue;
        }
        // Convert plain api_key input to api_key_cmd = "echo <key>"
        if field == "api_key" {
            if !change.value.is_empty() {
                let toml_key = format!("llm.backends.{}.api_key_cmd", name);
                let cmd_value = format!("echo {}", change.value);
                omnish_common::config_edit::set_toml_value_nested(config_path, &toml_key, &cmd_value)?;
            }
            continue;
        }
        let toml_key = format!("llm.backends.{}.{}", name, field);
        omnish_common::config_edit::set_toml_value_nested(config_path, &toml_key, &change.value)?;
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
        assert!(schema.iter().any(|s| s.path == "proxy.http_proxy"));
        assert!(schema.iter().any(|s| s.path == "llm.use_cases.completion"));
        assert!(schema.iter().any(|s| s.path == "llm.backends.__new__" && s.handler.is_some()));
    }

    #[test]
    fn test_resolve_value_top_level() {
        let config = DaemonConfig {
            proxy: Some("http://proxy:8080".to_string()),
            ..Default::default()
        };
        let val = toml::Value::try_from(&config).unwrap();
        assert_eq!(resolve_value(&val, "proxy"), Some("http://proxy:8080".to_string()));
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
            max_content_chars: None,
        });
        config.llm.backends.insert("openai".to_string(), omnish_common::config::LlmBackendConfig {
            backend_type: "openai_compat".to_string(),
            model: "gpt-4o".to_string(),
            api_key_cmd: None,
            base_url: Some("https://api.openai.com".to_string()),
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
        let (items, _handlers) = build_config_items(&config);
        assert!(items.iter().any(|i| i.path == "proxy.http_proxy"));
        assert!(items.iter().any(|i| i.path == "llm.use_cases.completion"));
        assert!(items.iter().any(|i| i.path == "llm.backends.__new__.name"));
    }

    #[test]
    fn test_build_config_items_returns_handlers() {
        let config = DaemonConfig::default();
        let (_items, handlers) = build_config_items(&config);
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
            max_content_chars: None,
        });
        let (items, _handlers) = build_config_items(&config);
        assert!(items.iter().any(|i| i.path == "llm.backends.claude.backend_type"));
        assert!(items.iter().any(|i| i.path == "llm.backends.claude.model"));
        assert!(items.iter().any(|i| i.path == "llm.backends.claude.api_key_cmd"));
        let model_item = items.iter().find(|i| i.path == "llm.backends.claude.model").unwrap();
        match &model_item.kind {
            ConfigItemKind::TextInput { value } => assert_eq!(value, "claude-sonnet-4-5-20250929"),
            _ => panic!("expected TextInput"),
        }
    }
}
