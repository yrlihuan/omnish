# /config Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an interactive `/config` command in chat mode that lets users view and modify daemon configuration through the multi-level menu widget, with changes persisted to daemon.toml via protocol messages.

**Architecture:** Client sends ConfigQuery to daemon, receives flat ConfigItem list + handler info, built from an embedded TOML schema + serialized DaemonConfig. Client builds a menu tree, user navigates/edits, changes are sent back as ConfigUpdate. Handler submenus (e.g., add backend) are applied immediately on ESC; generic items are applied on menu exit. Daemon writes daemon.toml using toml_edit for format-preserving edits.

**Tech Stack:** Rust, serde Serialize/Deserialize, toml/toml_edit, bincode (protocol), existing menu widget

**Spec:** `docs/superpowers/specs/2026-03-23-config-command.md`

---

## File Structure

| File | Responsibility |
|------|---------------|
| `crates/omnish-common/src/config.rs` | Add `Serialize` derive to all daemon config structs |
| `crates/omnish-common/src/config_edit.rs` | Add `set_toml_value_nested()` for dotted key paths |
| `crates/omnish-protocol/src/message.rs` | Add 4 message variants + ConfigItem/ConfigItemKind/ConfigChange/ConfigHandlerInfo types |
| `crates/omnish-daemon/src/config_schema.toml` | Schema mapping: menu items ↔ TOML keys |
| `crates/omnish-daemon/src/config_schema.rs` | Parse schema, build items from config, apply changes + handlers |
| `crates/omnish-daemon/src/server.rs` | Handle ConfigQuery/ConfigUpdate messages |
| `crates/omnish-daemon/src/main.rs` | Add `mod config_schema;`, pass config_path to server |
| `crates/omnish-client/src/widgets/menu.rs` | Add `handler: Option<String>` to `MenuItem::Submenu` + `on_handler_exit` callback |
| `crates/omnish-client/src/chat_session.rs` | Add `/config` command, `build_menu_tree()`, handler callback |

---

### Task 1: Add Serialize derive to daemon config structs

**Files:**
- Modify: `crates/omnish-common/src/config.rs`

- [ ] **Step 1: Add Serialize to all daemon config structs**

Add `Serialize` to the derive list of these structs (line numbers for reference):

- Line 238: `DaemonConfig` — `#[derive(Debug, Serialize, Deserialize, Clone)]`
- Line 354: `LlmConfig` — same pattern
- Line 407: `LlmBackendConfig` — same pattern
- Line 390: `LangfuseConfig` — same pattern
- Line 551: `ContextConfig` — `#[derive(Debug, Serialize, Deserialize, Clone, Default)]`
- Line 425: `CompletionContextConfig` — `#[derive(Debug, Serialize, Deserialize, Clone)]`
- Line 476: `HourlySummaryConfig` — same
- Line 516: `DailySummaryConfig` — same
- Line 188: `TasksConfig` — `#[derive(Debug, Serialize, Deserialize, Clone, Default)]`
- Line 103: `EvictionConfig` — `#[derive(Debug, Serialize, Deserialize, Clone)]`
- Line 122: `DiskCleanupConfig` — same
- Line 136: `AutoUpdateConfig` — same
- Line 74: `DailyNotesConfig` — same
- Line 169: `PeriodicSummaryConfig` — same
- Line 206: `PluginsConfig` — `#[derive(Debug, Serialize, Deserialize, Clone, Default)]`
- Line 218: `SandboxConfig` — `#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]`
- Line 226: `SandboxPluginConfig` — same

Also add `use serde::Serialize;` if not already imported (check existing `use serde::Deserialize;` line).

**Important:** Add `#[serde(skip_serializing)]` to the `tools` field in `DaemonConfig` (line 261):
```rust
    #[serde(default)]
    #[serde(skip_serializing)]
    pub tools: HashMap<String, HashMap<String, serde_json::Value>>,
```
This prevents `toml::Value::try_from(&config)` from panicking when `tools` contains `serde_json::Value` types (e.g., null) that have no TOML equivalent. The `tools` field is not exposed in the `/config` menu.

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --release -p omnish-common`
Expected: Success (no errors). Serialize is already available via `serde = { features = ["derive"] }`.

- [ ] **Step 3: Write test for config serialization round-trip**

Add to `crates/omnish-common/src/config.rs` (in existing test module or create one):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_config_serializes_to_toml() {
        let config = DaemonConfig::default();
        let value = toml::Value::try_from(&config).unwrap();
        // Verify key paths exist
        assert!(value.get("llm").is_some());
        assert!(value.get("llm").unwrap().get("backends").is_some());
        assert!(value.get("proxy").is_some() || value.get("proxy").is_none()); // Option
    }
}
```

- [ ] **Step 4: Run test**

Run: `cargo test --release -p omnish-common -- tests::test_daemon_config_serializes_to_toml`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-common/src/config.rs
git commit -m "feat(config): add Serialize derive to daemon config structs"
```

---

### Task 2: Add set_toml_value_nested for dotted key paths

**Files:**
- Modify: `crates/omnish-common/src/config_edit.rs`

- [ ] **Step 1: Write failing tests for nested key support**

Add to `crates/omnish-common/src/config_edit.rs` test module:

```rust
#[test]
fn test_set_nested_value() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.toml");
    fs::write(&path, "[llm]\ndefault = \"claude\"\n").unwrap();

    set_toml_value_nested(&path, "llm.default", "openai").unwrap();

    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("default = \"openai\""));
}

#[test]
fn test_set_deeply_nested_value() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.toml");
    fs::write(&path, "").unwrap();

    set_toml_value_nested(&path, "llm.use_cases.completion", "claude-fast").unwrap();

    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("[llm.use_cases]") || result.contains("[llm]"));
    assert!(result.contains("completion = \"claude-fast\""));
}

#[test]
fn test_set_nested_creates_file_if_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent.toml");

    set_toml_value_nested(&path, "proxy", "http://proxy:8080").unwrap();

    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("proxy = \"http://proxy:8080\""));
}

#[test]
fn test_set_nested_bool_value() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.toml");
    fs::write(&path, "[tasks.daily_notes]\nenabled = false\n").unwrap();

    set_toml_value_nested_bool(&path, "tasks.daily_notes.enabled", true).unwrap();

    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("enabled = true"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --release -p omnish-common -- test_set_nested`
Expected: FAIL (functions not defined)

- [ ] **Step 3: Implement set_toml_value_nested and set_toml_value_nested_bool**

Add to `crates/omnish-common/src/config_edit.rs`:

```rust
/// Set a potentially nested key in a TOML file, preserving formatting.
///
/// `key` is a dot-separated path like `"llm.use_cases.completion"`.
/// Intermediate tables are created if they don't exist.
/// Creates the file if it doesn't exist.
pub fn set_toml_value_nested(path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    set_toml_value_nested_inner(path, key, toml_edit::value(value))
}

/// Set a nested boolean key in a TOML file, preserving formatting.
pub fn set_toml_value_nested_bool(path: &Path, key: &str, value: bool) -> anyhow::Result<()> {
    set_toml_value_nested_inner(path, key, toml_edit::value(value))
}

fn set_toml_value_nested_inner(
    path: &Path,
    key: &str,
    value: toml_edit::Item,
) -> anyhow::Result<()> {
    let content = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };
    let mut doc = content.parse::<toml_edit::DocumentMut>()?;

    let segments: Vec<&str> = key.split('.').collect();
    if segments.len() == 1 {
        doc[segments[0]] = value;
    } else {
        // Navigate/create intermediate tables
        let (parents, leaf) = segments.split_at(segments.len() - 1);
        let mut table = doc.as_table_mut();
        for &seg in parents {
            if !table.contains_key(seg) {
                table.insert(seg, toml_edit::Item::Table(toml_edit::Table::new()));
            }
            table = table[seg]
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("{} is not a table", seg))?;
        }
        table[leaf[0]] = value;
    }

    let output = doc.to_string();
    let output = if output.ends_with('\n') { output } else { format!("{}\n", output) };
    std::fs::write(path, output)?;
    Ok(())
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --release -p omnish-common -- test_set_nested`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-common/src/config_edit.rs
git commit -m "feat(config_edit): add set_toml_value_nested for dotted key paths"
```

---

### Task 3: Add protocol messages and types

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs`

- [ ] **Step 1: Add ConfigItem, ConfigItemKind, ConfigChange, ConfigHandlerInfo types**

Add after the existing type definitions (before the Message enum):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigItem {
    pub path: String,
    pub label: String,
    pub kind: ConfigItemKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConfigItemKind {
    Toggle { value: bool },
    Select { options: Vec<String>, selected: usize },
    TextInput { value: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChange {
    pub path: String,
    pub value: String,
}

/// Metadata about handler submenus — sent alongside items so the client
/// knows which submenus trigger handler callbacks and what label to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigHandlerInfo {
    /// Schema path of the handler submenu (e.g., "llm.backends.__new__").
    pub path: String,
    /// Display label (e.g., "Add backend"). Used for `__new__` segments
    /// that can't be auto-labeled from the path.
    pub label: String,
    /// Handler function name (e.g., "add_backend").
    pub handler: String,
}
```

- [ ] **Step 2: Add 4 message variants to Message enum**

Add before the closing `}` of the Message enum (before line 36), after `AuthFailed`:

```rust
    ConfigQuery,
    ConfigResponse {
        items: Vec<ConfigItem>,
        handlers: Vec<ConfigHandlerInfo>,
    },
    ConfigUpdate { changes: Vec<ConfigChange> },
    ConfigUpdateResult { ok: bool, error: Option<String> },
```

- [ ] **Step 3: Bump protocol version and variant count**

Change line 8: `pub const PROTOCOL_VERSION: u32 = 9;`
Change line 542: `const EXPECTED_VARIANT_COUNT: usize = 28;`

- [ ] **Step 4: Update the guard test's exhaustive match**

In the `message_variant_guard` test (starting line 541), add the 4 new variants to the `variants` vec:

```rust
Message::ConfigQuery,
Message::ConfigResponse { items: vec![], handlers: vec![] },
Message::ConfigUpdate { changes: vec![] },
Message::ConfigUpdateResult { ok: true, error: None },
```

- [ ] **Step 5: Build and run tests**

Run: `cargo build --release -p omnish-protocol && cargo test --release -p omnish-protocol`
Expected: All tests pass, including `message_variant_guard`

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-protocol/src/message.rs
git commit -m "feat(protocol): add ConfigQuery/Response/Update/UpdateResult messages"
```

---

### Task 4: Create config schema file

**Files:**
- Create: `crates/omnish-daemon/src/config_schema.toml`

- [ ] **Step 1: Create the schema file**

Create `crates/omnish-daemon/src/config_schema.toml` with the contents from the spec:

```toml
# Config schema: maps menu items to daemon.toml keys.
# Embedded via include_str!() — changes require recompilation.
#
# Fields:
#   path         — dot-separated menu hierarchy (e.g., "proxy.http_proxy")
#   label        — display name in menu
#   kind         — text | select | toggle | submenu
#   toml_key     — actual daemon.toml key path (leaf items without handler)
#   options_from — (select) TOML table whose keys become options at runtime
#   options      — (select) static option list
#   handler      — (submenu) Rust function name for grouped change handling

# ── Proxy ──────────────────────────────────────────────
[[items]]
path = "proxy.http_proxy"
label = "HTTP proxy"
kind = "text"
toml_key = "proxy"

[[items]]
path = "proxy.no_proxy"
label = "No proxy"
kind = "text"
toml_key = "no_proxy"

# ── LLM use cases ─────────────────────────────────────
[[items]]
path = "llm.use_cases.completion"
label = "Completion backend"
kind = "select"
toml_key = "llm.use_cases.completion"
options_from = "llm.backends"

[[items]]
path = "llm.use_cases.analysis"
label = "Analysis backend"
kind = "select"
toml_key = "llm.use_cases.analysis"
options_from = "llm.backends"

[[items]]
path = "llm.use_cases.chat"
label = "Chat backend"
kind = "select"
toml_key = "llm.use_cases.chat"
options_from = "llm.backends"

# ── Add new LLM backend ───────────────────────────────
# handler on submenu node: all child changes are grouped
# and passed to the handler function on ESC back
[[items]]
path = "llm.backends.__new__"
label = "Add backend"
kind = "submenu"
handler = "add_backend"

[[items]]
path = "llm.backends.__new__.name"
label = "Name"
kind = "text"

[[items]]
path = "llm.backends.__new__.backend_type"
label = "Backend type"
kind = "select"
options = ["anthropic", "openai_compat"]

[[items]]
path = "llm.backends.__new__.model"
label = "Model"
kind = "text"

[[items]]
path = "llm.backends.__new__.api_key_cmd"
label = "API key command"
kind = "text"

[[items]]
path = "llm.backends.__new__.base_url"
label = "Base URL"
kind = "text"
```

- [ ] **Step 2: Commit**

```bash
git add crates/omnish-daemon/src/config_schema.toml
git commit -m "feat(daemon): add config schema for /config menu"
```

---

### Task 5: Implement config_schema.rs — schema parsing, build_config_items, apply_config_changes

**Files:**
- Create: `crates/omnish-daemon/src/config_schema.rs`
- Modify: `crates/omnish-daemon/src/main.rs` (add `mod config_schema;`)

- [ ] **Step 1: Write tests for schema parsing, value resolution, and dynamic backend items**

```rust
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
        let mut config = DaemonConfig::default();
        config.proxy = Some("http://proxy:8080".to_string());
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
        // __new__ children should be present (handler children with empty values)
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
        // Should have dynamically generated items for existing backend
        assert!(items.iter().any(|i| i.path == "llm.backends.claude.backend_type"));
        assert!(items.iter().any(|i| i.path == "llm.backends.claude.model"));
        assert!(items.iter().any(|i| i.path == "llm.backends.claude.api_key_cmd"));
        // Check values populated
        let model_item = items.iter().find(|i| i.path == "llm.backends.claude.model").unwrap();
        match &model_item.kind {
            ConfigItemKind::TextInput { value } => assert_eq!(value, "claude-sonnet-4-5-20250929"),
            _ => panic!("expected TextInput"),
        }
    }
}
```

- [ ] **Step 2: Implement config_schema.rs**

```rust
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
///
/// Returns `(items, handlers)`:
/// - `items`: flat list of leaf ConfigItems (no submenu nodes).
///   Includes dynamically generated items for existing backends.
/// - `handlers`: metadata about handler submenus so the client can mark
///   the corresponding MenuItem::Submenu with handler info.
pub fn build_config_items(config: &DaemonConfig) -> (Vec<ConfigItem>, Vec<ConfigHandlerInfo>) {
    let schema = parse_schema();
    let config_value = toml::Value::try_from(config)
        .expect("DaemonConfig must be Serializable");

    // Collect handler info for the client
    let handlers: Vec<ConfigHandlerInfo> = schema.iter()
        .filter(|s| s.handler.is_some())
        .map(|s| ConfigHandlerInfo {
            path: s.path.clone(),
            label: s.label.clone(),
            handler: s.handler.clone().unwrap(),
        })
        .collect();

    // Collect handler paths to identify children
    let handler_paths: Vec<&str> = schema.iter()
        .filter(|s| s.handler.is_some())
        .map(|s| s.path.as_str())
        .collect();

    let mut items = Vec::new();
    for s in &schema {
        if s.kind == "submenu" {
            // Submenu nodes are structural — client builds them from paths.
            // Handler info is communicated via the handlers vec above.
            continue;
        }

        // Check if this item is under a handler submenu
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

    // ── Dynamic items: existing backends ──────────────────────────────
    // For each existing backend in config.llm.backends, generate editable
    // items so the user can view and modify them. The path is the actual
    // toml key (e.g., "llm.backends.claude.model"), so apply_config_changes
    // can write it directly (falls through to generic path-as-toml_key).
    for (name, backend) in &config.llm.backends {
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

/// Look up the schema item for a given path.
fn find_schema_item<'a>(schema: &'a [SchemaItem], path: &str) -> Option<&'a SchemaItem> {
    schema.iter().find(|s| s.path == path)
}

/// Find the handler name for a path, if it's under a handler submenu.
fn find_handler_for_path<'a>(schema: &'a [SchemaItem], path: &str) -> Option<&'a str> {
    schema.iter()
        .filter(|s| s.handler.is_some())
        .find(|s| path.starts_with(&s.path) && path != s.path)
        .and_then(|s| s.handler.as_deref())
}

/// Apply config changes to daemon.toml.
///
/// Generic items (no handler) are written via set_toml_value_nested.
/// Handler items are grouped and dispatched to handler functions.
pub fn apply_config_changes(config_path: &Path, changes: &[ConfigChange]) -> anyhow::Result<()> {
    let schema = parse_schema();

    // Split into generic and handler groups
    let mut generic: Vec<&ConfigChange> = Vec::new();
    let mut handler_groups: std::collections::HashMap<String, Vec<&ConfigChange>> = std::collections::HashMap::new();

    for change in changes {
        if let Some(handler) = find_handler_for_path(&schema, &change.path) {
            handler_groups.entry(handler.to_string()).or_default().push(change);
        } else {
            generic.push(change);
        }
    }

    // Apply generic changes.
    // For items with a schema entry, use the schema's toml_key.
    // For dynamic items (e.g., existing backend fields), the path IS the toml_key.
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

    // Dispatch handler groups
    for (handler, changes) in &handler_groups {
        match handler.as_str() {
            "add_backend" => handle_add_backend(config_path, changes)?,
            other => anyhow::bail!("unknown handler: {}", other),
        }
    }

    Ok(())
}

/// Handler: add a new LLM backend to daemon.toml.
///
/// Expects changes with paths like "llm.backends.__new__.name", etc.
/// Extracts name field, writes [llm.backends.<name>] section.
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
            continue; // name is the table key, not a value
        }
        let field = change.path.rsplit('.').next().unwrap_or("");
        if field.is_empty() {
            continue;
        }
        let toml_key = format!("llm.backends.{}.{}", name, field);
        omnish_common::config_edit::set_toml_value_nested(config_path, &toml_key, &change.value)?;
    }

    Ok(())
}
```

- [ ] **Step 3: Add `mod config_schema;` to main.rs**

Add at line 2 of `crates/omnish-daemon/src/main.rs`:
```rust
mod config_schema;
```

- [ ] **Step 4: Build and run tests**

Run: `cargo build --release -p omnish-daemon && cargo test --release -p omnish-daemon -- config_schema`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/config_schema.rs crates/omnish-daemon/src/main.rs
git commit -m "feat(daemon): implement config schema parser and item builder"
```

---

### Task 6: Handle ConfigQuery/ConfigUpdate in daemon server

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`
- Modify: `crates/omnish-daemon/src/main.rs`

The daemon needs access to `config_path` and current `DaemonConfig` at request time. Currently `handle_message` doesn't have these. We need to thread them through.

- [ ] **Step 1: Add config_path and daemon_config to ServerOpts**

In `crates/omnish-daemon/src/server.rs`, add to `ServerOpts` struct (line 116):

```rust
pub struct ServerOpts {
    pub proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub sandbox_rules: SandboxRules,
    pub config_path: std::path::PathBuf,
    pub daemon_config: std::sync::Arc<std::sync::RwLock<omnish_common::config::DaemonConfig>>,
}
```

- [ ] **Step 2: Update ServerOpts construction in main.rs**

In `crates/omnish-daemon/src/main.rs`, where `ServerOpts` is constructed, add the new fields. Find the existing `ServerOpts { proxy, no_proxy, sandbox_rules }` construction and add:

```rust
config_path: config_path.clone(),
daemon_config: Arc::new(std::sync::RwLock::new(config.clone())),
```

- [ ] **Step 3: Add ConfigQuery/ConfigUpdate match arms in handle_message**

In `crates/omnish-daemon/src/server.rs`, add before the `_ =>` catch-all (line 827):

```rust
Message::ConfigQuery => {
    let config = opts.daemon_config.read().unwrap().clone();
    let (items, handlers) = crate::config_schema::build_config_items(&config);
    let _ = tx.send(Message::ConfigResponse { items, handlers }).await;
}
Message::ConfigUpdate { changes } => {
    let result = crate::config_schema::apply_config_changes(&opts.config_path, &changes);
    match result {
        Ok(()) => {
            // Reload config after successful write
            if let Ok(new_config) = omnish_common::config::load_daemon_config() {
                *opts.daemon_config.write().unwrap() = new_config;
            }
            let _ = tx.send(Message::ConfigUpdateResult { ok: true, error: None }).await;
        }
        Err(e) => {
            let _ = tx.send(Message::ConfigUpdateResult {
                ok: false,
                error: Some(e.to_string()),
            }).await;
        }
    }
}
Message::ConfigResponse { .. } | Message::ConfigUpdateResult { .. } => {
    // These are daemon→client messages, ignore if received
    let _ = tx.send(Message::Ack).await;
}
```

- [ ] **Step 4: Build**

Run: `cargo build --release -p omnish-daemon`
Expected: Success

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/main.rs
git commit -m "feat(daemon): handle ConfigQuery/ConfigUpdate messages"
```

---

### Task 7: Add handler submenu support to menu widget

**Files:**
- Modify: `crates/omnish-client/src/widgets/menu.rs`

The menu widget needs to:
1. Know which submenus are "handler" submenus (via the `handler` field)
2. Call a callback when the user ESC-exits a handler submenu
3. Accept a new menu tree from the callback and refresh

- [ ] **Step 1: Add `handler` field to `MenuItem::Submenu` variant**

Change the `Submenu` variant in `MenuItem` (line 31):

```rust
pub enum MenuItem {
    /// Navigate into a child menu.
    Submenu {
        label: String,
        children: Vec<MenuItem>,
        handler: Option<String>,
    },
    // ... rest unchanged
}
```

- [ ] **Step 2: Update ALL existing pattern matches and construction sites for Submenu**

There are 7 match sites in menu.rs. Sites using `..` need no change; construction sites need `handler: None`:

**Line 56** — `MenuItem::label()`: uses `..`, no change needed.

**Line 93** — `render_menu_item`: uses `..`, no change needed.

**Line 387** — navigation in `run_menu`: uses `..`, no change needed.

**Line 497** — Enter key handler: add `..`:
```rust
MenuItem::Submenu { label, children, .. } => {
```

**Line 672** — test `test_menu_item_label`: add `handler: None`:
```rust
let item = MenuItem::Submenu {
    label: "LLM".to_string(),
    children: vec![],
    handler: None,
};
```

**Line 681** — test `test_render_menu_item_submenu`: add `handler: None`:
```rust
let item = MenuItem::Submenu {
    label: "LLM".to_string(),
    children: vec![],
    handler: None,
};
```

**Line 780** — test `test_render_full_menu`: add `handler: None`:
```rust
MenuItem::Submenu {
    label: "LLM".to_string(),
    children: vec![],
    handler: None,
},
```

**Line 799** — test `test_empty_menu_returns_done`: update for new signature:
```rust
let result = run_menu("Empty", &mut vec![], None);
```

Also search for any other `MenuItem::Submenu` construction sites in the file and add `handler: None`.

- [ ] **Step 3: Update `run_menu` signature**

Change the signature to accept a callback:

```rust
pub fn run_menu(
    title: &str,
    items: &mut Vec<MenuItem>,
    on_handler_exit: Option<&mut dyn FnMut(&str, Vec<MenuChange>) -> Option<Vec<MenuItem>>>,
) -> MenuResult
```

Note: changed `&mut [MenuItem]` to `&mut Vec<MenuItem>` so the callback can replace the entire tree.

- [ ] **Step 4: Refactor main loop to re-derive current_items each iteration**

The handler callback needs to replace the entire `items` tree and reset navigation. This creates a mutable borrow conflict: `current_items` is a mutable sub-slice of `items`, but the callback needs `&mut items`. The fix is to **not hold `current_items` across iterations** — re-derive it at the top of each loop iteration.

Replace the existing `current_items` initialization (around line 385) and the main loop:

```rust
// REMOVE the initial current_items assignment before the loop.
// Instead, derive it at the start of each iteration.

loop {
    // 1. Read handler name from nav_stack FIRST (immutable borrow of items).
    //    This must happen before current_items is created to avoid borrow conflict.
    let current_handler: Option<String> = if !nav_stack.is_empty() {
        let last = nav_stack.last().unwrap();
        let mut node: &[MenuItem] = items.as_slice();
        for entry in &nav_stack[..nav_stack.len() - 1] {
            match &node[entry.parent_index] {
                MenuItem::Submenu { children, .. } => node = children,
                _ => break,
            }
        }
        match &node[last.parent_index] {
            MenuItem::Submenu { handler: Some(h), .. } => Some(h.clone()),
            _ => None,
        }
    } else {
        None
    };

    // 2. Re-derive current_items (mutable borrow of items).
    let current_items: &mut [MenuItem] = {
        let mut slice = items.as_mut_slice();
        for entry in &nav_stack {
            match &mut slice[entry.parent_index] {
                MenuItem::Submenu { children, .. } => slice = children.as_mut_slice(),
                _ => unreachable!(),
            }
        }
        slice
    };

    // 3. Rest of loop body (input handling, rendering) uses current_items.
    //    REMOVE any `current_items = ...` reassignment at nav push/pop sites —
    //    just update nav_stack/cursor/scroll_offset/breadcrumb_parts and `continue`.
    //    current_items is re-derived automatically on next iteration.

    // 4. In the ESC handler, use `current_handler` (computed above) instead
    //    of re-reading from items. See Step 5 below.
}
```

The nav push code (Enter on Submenu, around line 497) currently sets `current_items = children.as_mut_slice()`. Remove that assignment — just push to `nav_stack` and `continue` to the next iteration where it will be re-derived.

The nav pop code (ESC, around line 466) currently sets `current_items` from the popped entry. Remove that assignment — just pop `nav_stack` and `continue`.

- [ ] **Step 5: Add handler detection in ESC handling**

In the ESC arm, when `!nav_stack.is_empty()`, BEFORE the normal pop:

```rust
// ESC and nav_stack is not empty
if !nav_stack.is_empty() {
    // current_handler was computed at the top of the loop iteration
    // (before current_items was derived), so no borrow conflict here.

    if let Some(ref handler_name) = current_handler {
        if let Some(ref mut callback) = on_handler_exit {
            // Collect changes whose path contains this handler's breadcrumb prefix
            let handler_prefix = breadcrumb_parts[1..].join(".");
            let handler_changes: Vec<MenuChange> = changes.iter()
                .filter(|c| c.path.starts_with(&handler_prefix))
                .cloned()
                .collect();

            // IMPORTANT: Remove handler changes from main changes vec
            // to prevent double-application on final menu exit
            changes.retain(|c| !c.path.starts_with(&handler_prefix));

            if !handler_changes.is_empty() {
                if let Some(new_items) = callback(handler_name, handler_changes) {
                    *items = new_items;
                    nav_stack.clear();
                    breadcrumb_parts.truncate(1); // keep only title
                    cursor = 0;
                    scroll_offset = 0;
                    // Erase and redraw at top level
                    let cleanup = render_cleanup(last_item_count);
                    common::write_stdout(cleanup.as_bytes());
                    continue; // re-derive current_items at top of loop
                }
            }
        }
    }

    // Normal pop (no handler, or handler returned None)
    let entry = nav_stack.pop().unwrap();
    cursor = entry.cursor;
    scroll_offset = entry.scroll_offset;
    breadcrumb_parts.pop();
    // Erase and redraw
    let cleanup = render_cleanup(last_item_count);
    common::write_stdout(cleanup.as_bytes());
    continue;
}
```

- [ ] **Step 6: Build and run tests**

Run: `cargo build --release -p omnish-client && cargo test --release -p omnish-client -- widgets::menu`
Expected: All existing tests still pass

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-client/src/widgets/menu.rs
git commit -m "feat(menu): add handler submenu support with on_handler_exit callback"
```

---

### Task 8: Implement /config command in client + update handle_test_menu

**Files:**
- Modify: `crates/omnish-client/src/chat_session.rs`

- [ ] **Step 1: Update handle_test_menu for new MenuItem::Submenu signature**

All `MenuItem::Submenu { label, children }` constructions in `handle_test_menu` must add `handler: None`:

```rust
MenuItem::Submenu {
    label: "LLM".to_string(),
    children: vec![...],
    handler: None,
},
```

Also update the `run_menu` call to pass `None`:
```rust
let result = widgets::menu::run_menu("Config", &mut items, None);
```

- [ ] **Step 2: Add build_menu_tree and segment_to_label functions**

```rust
use omnish_protocol::message::{ConfigItem, ConfigItemKind, ConfigChange, ConfigHandlerInfo};
use std::collections::HashMap;
use std::cell::RefCell;

/// Convert path segment to display label: capitalize first letter, _ → space.
fn segment_to_label(seg: &str) -> String {
    seg.replace('_', " ")
        .split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => format!("{}{}", c.to_uppercase(), chars.collect::<String>()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build MenuItem tree from flat ConfigItems and handler info.
///
/// Returns (tree, path_map) where path_map maps display paths → schema paths.
///
/// `handlers` provides metadata about handler submenus:
/// - Their path is used to set `handler: Some(name)` on the corresponding submenu node
/// - Their label is used for `__new__` path segments (instead of auto-labeling)
fn build_menu_tree(
    items: &[ConfigItem],
    handlers: &[ConfigHandlerInfo],
) -> (Vec<widgets::menu::MenuItem>, HashMap<String, String>) {
    use widgets::menu::MenuItem;
    let mut root: Vec<MenuItem> = Vec::new();
    let mut path_map: HashMap<String, String> = HashMap::new();

    // Build a lookup for handler submenus: path → (handler_name, label)
    let handler_lookup: HashMap<&str, (&str, &str)> = handlers.iter()
        .map(|h| (h.path.as_str(), (h.handler.as_str(), h.label.as_str())))
        .collect();

    for item in items {
        let segments: Vec<&str> = item.path.split('.').collect();

        // Navigate/create submenu chain for all segments except the last (leaf)
        let mut current = &mut root;
        for (i, &seg) in segments.iter().enumerate() {
            if i == segments.len() - 1 {
                // Leaf item — create Toggle/Select/TextInput
                let menu_item = match &item.kind {
                    ConfigItemKind::Toggle { value } => MenuItem::Toggle {
                        label: item.label.clone(),
                        value: *value,
                    },
                    ConfigItemKind::Select { options, selected } => MenuItem::Select {
                        label: item.label.clone(),
                        options: options.clone(),
                        selected: *selected,
                    },
                    ConfigItemKind::TextInput { value } => MenuItem::TextInput {
                        label: item.label.clone(),
                        value: value.clone(),
                    },
                };
                current.push(menu_item);

                // Build display path for reverse lookup.
                // Display path = submenu labels joined by ".", ending with item label.
                // We build this by collecting the labels of submenus we traversed.
                let mut display_parts: Vec<String> = Vec::new();
                let mut schema_prefix = String::new();
                for (j, &s) in segments[..i].iter().enumerate() {
                    if j > 0 { schema_prefix.push('.'); }
                    schema_prefix.push_str(s);
                    // Use handler label if this is a __new__ segment
                    let label = if s == "__new__" {
                        handler_lookup.get(schema_prefix.as_str())
                            .map(|(_, lbl)| lbl.to_string())
                            .unwrap_or_else(|| segment_to_label(s))
                    } else {
                        segment_to_label(s)
                    };
                    display_parts.push(label);
                }
                display_parts.push(item.label.clone());
                let display_key = display_parts.join(".");
                path_map.insert(display_key, item.path.clone());
            } else {
                // Intermediate segment — find or create submenu
                // For __new__ segments, use the handler's label
                let schema_path_so_far = segments[..=i].join(".");
                let label = if seg == "__new__" {
                    handler_lookup.get(schema_path_so_far.as_str())
                        .map(|(_, lbl)| lbl.to_string())
                        .unwrap_or_else(|| segment_to_label(seg))
                } else {
                    segment_to_label(seg)
                };

                let pos = current.iter().position(|m| {
                    matches!(m, MenuItem::Submenu { label: l, .. } if *l == label)
                });
                let idx = match pos {
                    Some(idx) => idx,
                    None => {
                        // Check if this path is a handler submenu
                        let handler = handler_lookup.get(schema_path_so_far.as_str())
                            .map(|(name, _)| name.to_string());
                        current.push(MenuItem::Submenu {
                            label: label.clone(),
                            children: Vec::new(),
                            handler,
                        });
                        current.len() - 1
                    }
                };
                current = match &mut current[idx] {
                    MenuItem::Submenu { children, .. } => children,
                    _ => unreachable!(),
                };
            }
        }
    }

    (root, path_map)
}
```

- [ ] **Step 3: Add /config command dispatch**

In `chat_session.rs`, after the `/test` command block (around line 420), add:

```rust
// /config — daemon configuration menu
if trimmed == "/config" {
    self.handle_config(session_id, rpc).await;
    continue;
}
```

- [ ] **Step 4: Implement handle_config**

**Critical: async/sync boundary.** `run_menu` is synchronous (it reads raw terminal input in a loop). The handler callback inside `run_menu` needs to call `rpc.call()` which is async. We are inside an async `fn run()` which runs on a tokio runtime.

**Solution:** Use `tokio::task::block_in_place()` which moves the current task off the runtime thread, allowing `block_on` inside it. This is safe because `run_menu` already blocks the terminal (no other async work can happen while the menu is displayed).

```rust
async fn handle_config(&mut self, session_id: &str, rpc: &RpcClient) {
    // 1. Query daemon for config items
    let (items, handlers) = match rpc.call(Message::ConfigQuery).await {
        Ok(Message::ConfigResponse { items, handlers }) => (items, handlers),
        Ok(_) => {
            write_stdout("\x1b[31mUnexpected response from daemon\x1b[0m\r\n");
            return;
        }
        Err(e) => {
            write_stdout(&format!("\x1b[31mFailed to query config: {}\x1b[0m\r\n", e));
            return;
        }
    };

    if items.is_empty() {
        write_stdout("\x1b[2;90mNo configurable items.\x1b[0m\r\n");
        return;
    }

    // 2. Build menu tree
    let (mut menu_items, path_map_initial) = build_menu_tree(&items, &handlers);

    // Use RefCell so the handler callback can update path_map
    let path_map = RefCell::new(path_map_initial);

    // 3. Run menu with handler callback.
    // block_in_place moves this task off the runtime thread, so
    // Handle::block_on inside the callback won't panic.
    let rpc_ref = rpc;
    let path_map_ref = &path_map;
    let handlers_ref = &handlers;

    let result = tokio::task::block_in_place(|| {
        let rt = tokio::runtime::Handle::current();

        let mut handler_callback = |handler_name: &str, handler_changes: Vec<widgets::menu::MenuChange>| -> Option<Vec<widgets::menu::MenuItem>> {
            // Convert MenuChange to ConfigChange using path_map
            let pm = path_map_ref.borrow();
            let config_changes: Vec<ConfigChange> = handler_changes.iter()
                .map(|mc| {
                    let schema_path = pm.get(&mc.path)
                        .cloned()
                        .unwrap_or_else(|| mc.path.clone());
                    ConfigChange { path: schema_path, value: mc.value.clone() }
                })
                .collect();
            drop(pm);

            // Send changes to daemon via blocking bridge
            let update_result = rt.block_on(async {
                rpc_ref.call(Message::ConfigUpdate { changes: config_changes }).await
            });

            match update_result {
                Ok(Message::ConfigUpdateResult { ok: true, .. }) => {
                    // Re-fetch config to rebuild menu with updated data
                    let query_result = rt.block_on(async {
                        rpc_ref.call(Message::ConfigQuery).await
                    });
                    match query_result {
                        Ok(Message::ConfigResponse { items, handlers: new_handlers }) => {
                            let (new_tree, new_map) = build_menu_tree(&items, &new_handlers);
                            // Update path_map so subsequent lookups use fresh mappings
                            *path_map_ref.borrow_mut() = new_map;
                            Some(new_tree)
                        }
                        _ => None,
                    }
                }
                Ok(Message::ConfigUpdateResult { ok: false, error }) => {
                    write_stdout(&format!("\x1b[31mHandler error: {}\x1b[0m\r\n",
                        error.unwrap_or_default()));
                    None
                }
                _ => None,
            }
        };

        widgets::menu::run_menu("Config", &mut menu_items, Some(&mut handler_callback))
    });

    // 4. Handle generic changes on menu exit
    match result {
        widgets::menu::MenuResult::Done(changes) => {
            if changes.is_empty() {
                write_stdout("\x1b[2;90mNo changes made.\x1b[0m\r\n");
                return;
            }
            // Convert to ConfigChange
            let pm = path_map.borrow();
            let config_changes: Vec<ConfigChange> = changes.iter()
                .map(|mc| {
                    let schema_path = pm.get(&mc.path)
                        .cloned()
                        .unwrap_or_else(|| mc.path.clone());
                    ConfigChange { path: schema_path, value: mc.value.clone() }
                })
                .collect();
            drop(pm);

            match rpc.call(Message::ConfigUpdate { changes: config_changes }).await {
                Ok(Message::ConfigUpdateResult { ok: true, .. }) => {
                    write_stdout(&format!(
                        "\x1b[2;90mConfig saved ({}). Restart daemon to apply.\x1b[0m\r\n",
                        changes.len()
                    ));
                }
                Ok(Message::ConfigUpdateResult { ok: false, error }) => {
                    write_stdout(&format!("\x1b[31mFailed to save: {}\x1b[0m\r\n",
                        error.unwrap_or_default()));
                }
                Err(e) => {
                    write_stdout(&format!("\x1b[31mRPC error: {}\x1b[0m\r\n", e));
                }
                _ => {}
            }
        }
        widgets::menu::MenuResult::Cancelled => {
            write_stdout("\x1b[2;90mCancelled.\x1b[0m\r\n");
        }
    }
}
```

- [ ] **Step 5: Build**

Run: `cargo build --release -p omnish-client`
Expected: Success

- [ ] **Step 6: Manual test**

Start daemon and client. In chat mode, type `/config`. Verify:
1. Menu appears with Proxy and LLM submenus
2. Can navigate into submenus
3. Can edit text fields and select options
4. Existing backends appear under LLM > Backends as viewable/editable submenus
5. LLM > Backends > Add Backend shows empty form; on ESC, handler fires and new backend appears
6. ESC at top level saves generic changes and shows confirmation
7. Ctrl-C cancels without saving

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-client/src/chat_session.rs
git commit -m "feat(client): add /config command with menu-based daemon configuration"
```

---

### Task 9: Full build and integration verification

**Files:** None (verification only)

- [ ] **Step 1: Full workspace build**

Run: `cargo build --release`
Expected: Clean build, no errors

- [ ] **Step 2: Run all tests**

Run: `cargo test --release`
Expected: All tests pass

- [ ] **Step 3: Verify /config end-to-end**

Manual test checklist:
1. Start daemon with a `daemon.toml` that has backends configured
2. Start client, enter chat mode
3. `/config` → verify Proxy and LLM submenus appear
4. Edit HTTP proxy → ESC to top → ESC to exit → verify "Config saved" message
5. Check `daemon.toml` — verify proxy value was written
6. Navigate to LLM > Backends → verify existing backends appear as submenus
7. Navigate to LLM > Backends > Add Backend → fill in fields → ESC back
8. Verify the new backend appears in LLM > Use Cases select options
9. Ctrl-C from any level → verify "Cancelled" and no changes written

- [ ] **Step 4: Final commit if any fixes needed**

```bash
git add -A
git commit -m "fix: address /config integration issues"
```
