use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::os::fd::AsRawFd;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

use omnish_common::config::ClientSandboxConfig;

use omnish_protocol::message::*;
use omnish_protocol::message::{ChatToolStatus, ConfigHandlerInfo, StatusIcon};
use omnish_pty::proxy::PtyProxy;
use omnish_transport::rpc_client::RpcClient;

use crate::{client_plugin, command, display, ghost_complete, markdown, widgets};
use crate::display::{BOLD, BRIGHT_WHITE, CYAN, DIM, GRAY, GREEN, RED, RESET, YELLOW};
use widgets::scroll_view::ScrollView;

#[derive(Debug, Clone)]
pub enum ScrollEntry {
    UserInput(String),
    ToolStatus(ChatToolStatus),
    LlmText(String),
    Response(String),
    Separator,
    SystemMessage(String),
}

/// Action requested by chat session upon exit.
pub enum ChatExitAction {
    /// Normal exit — no special action needed.
    Normal,
    /// Request to toggle Landlock sandbox on the shell process.
    Lock(bool),
}

enum ResumeMismatchAction {
    Cancel,
    CdToOld(String),
    StayHere(String),
    ContinueDifferentHost,
}

pub struct ChatSession {
    current_thread_id: Option<String>,
    cached_thread_ids: Vec<String>,
    chat_history: VecDeque<String>,
    history_index: Option<usize>,
    completer: ghost_complete::GhostCompleter,
    scroll_history: Vec<ScrollEntry>,
    thinking_visible: bool,
    has_activity: bool,
    pending_input: Option<String>,
    client_plugins: Arc<client_plugin::ClientPluginManager>,
    ghost_hint_shown: bool,
    pending_model: Option<String>,
    /// Non-default model name for resumed thread (shown as ghost hint).
    resumed_model: Option<String>,
    /// Shell's current working directory (from /proc/pid/cwd), set at chat entry.
    shell_cwd: Option<String>,
    /// Directory to cd into after chat mode exits (set by resume mismatch handler).
    pending_cd: Option<String>,
    extended_unicode: bool,
    /// Total terminal lines printed (for tracking tool section position).
    lines_printed: usize,
    /// Line position where the current batch of tool headers starts.
    tool_section_start: Option<usize>,
    /// scroll_history index where the current tool batch starts.
    tool_section_hist_idx: Option<usize>,
    /// Current spinner animation frame index (for running tool icons).
    spinner_frame: usize,
    /// Client-local sandbox config (enabled + preferred backend). Shared with
    /// main event loop so menu edits persist across chat sessions.
    sandbox_state: Arc<RwLock<ClientSandboxConfig>>,
    /// Input text to restore into the editor after an early Ctrl-C cancellation.
    cancelled_input: Option<String>,
    /// Buffered /thread sandbox preference before a thread exists. Applied
    /// right after ChatReady for a new thread, then cleared.
    pending_sandbox_off: Option<bool>,
}

fn write_stdout(s: &str) {
    nix::unistd::write(std::io::stdout(), s.as_bytes()).ok();
}

/// Strip `-YYYYMMDD` date suffix from model name for display.
/// e.g. "claude-sonnet-4-5-20250929" → "claude-sonnet-4-5"
fn strip_date_suffix(model: &str) -> &str {
    if model.len() > 9 {
        let suffix = &model[model.len() - 9..];
        if suffix.starts_with('-') && suffix[1..].bytes().all(|b| b.is_ascii_digit()) {
            return &model[..model.len() - 9];
        }
    }
    model
}

/// Map language code to display name for the language selector.
fn lang_code_to_display(code: &str) -> &str {
    match code {
        "zh" => "简体中文",
        "zh-tw" => "繁體中文",
        _ => "English",
    }
}

/// Map language display name back to code for config storage.
fn lang_display_to_code(display: &str) -> &str {
    match display {
        "简体中文" => "zh",
        "繁體中文" => "zh-tw",
        _ => "en",
    }
}

/// Convert path segment to display label: capitalize first letter, _ -> space.
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

/// Whether a config path refers to a sensitive value (API key, token, etc.)
fn is_sensitive_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("api_key")
        || lower.contains("secret")
        || lower.contains("token")
        || lower.contains("password")
}

/// Mask a sensitive value for display.
fn mask_sensitive_value(old: &str) -> (String, String) {
    if old.is_empty() {
        (crate::i18n::t("empty").to_string(), "***hidden***".into())
    } else {
        (format!("***{} chars***", old.len()), "***hidden***".into())
    }
}

/// Build a human-readable label for a config item from its config path segments
/// (e.g. "LLM > Default" from ["llm", "use_cases", "completion"]).
fn item_display_label(segments: Vec<String>, item_label: &str) -> String {
    let parts: Vec<String> = segments.into_iter()
        .filter(|s| s != "__new__")
        .map(|s| crate::i18n::translate_label(&segment_to_label(&s)))
        .collect();
    match parts.len() {
        0 => item_label.to_string(),
        1 => format!("{} > {}", parts[0], item_label),
        _ => format!("{} > {}", parts[..parts.len() - 1].join(" > "), item_label),
    }
}

/// A single config change detected between pre- and post-edit snapshots.
struct ConfigDiff {
    label: String,
    old_value: String,
    new_value: String,
}

/// Parsed global rule entry from the daemon's JSON data item.
#[derive(serde::Deserialize, Clone)]
struct GlobalRuleEntry {
    plugin: String,
    rules: Vec<String>,
}

/// Tool name → list of input parameter names (from tool input_schema).
type ToolParamsMap = std::collections::HashMap<String, Vec<String>>;

/// Parameters for building a permit rule form.
struct RuleFormParams<'a> {
    prefix: &'a str,
    plugin: &'a str,
    field: &'a str,
    operator: &'a str,
    value: &'a str,
    with_delete: bool,
    scope: Option<&'a str>,
}

/// Pre-scan items for `sandbox.__rules_json` and `sandbox.__tool_params_json`
/// data items; return parsed entries and the items list with data items removed.
fn extract_global_rules(items: Vec<ConfigItem>) -> (Vec<ConfigItem>, Vec<GlobalRuleEntry>, ToolParamsMap) {
    let mut rules = Vec::new();
    let mut tool_params = ToolParamsMap::new();
    let filtered = items.into_iter().filter(|it| {
        if it.path == "sandbox.__rules_json" {
            if let ConfigItemKind::Data { ref value } = it.kind {
                if let Ok(v) = serde_json::from_str::<Vec<GlobalRuleEntry>>(value) {
                    rules = v;
                }
            }
            return false;
        }
        if it.path == "sandbox.__tool_params_json" {
            if let ConfigItemKind::Data { ref value } = it.kind {
                if let Ok(v) = serde_json::from_str::<ToolParamsMap>(value) {
                    tool_params = v;
                }
            }
            return false;
        }
        true
    }).collect();
    (filtered, rules, tool_params)
}

/// Expand client-side placeholder labels in config items.
///
/// Convention: a Label item with `label = "_client:<key>"` is a placeholder.
/// The client replaces it with locally-detected data (potentially multiple items).
/// This keeps daemon in control of menu structure while client provides local info.
///
/// `local_paths` is populated with item paths that represent client-local config
/// (i.e. should be saved to client.toml instead of sent to the daemon via RPC).
///
/// `extra_handlers` receives ConfigHandlerInfo entries for locally-generated
/// handler submenus (local sandbox rules) that `build_menu_tree` needs.
fn expand_client_placeholders(
    items: Vec<ConfigItem>,
    sandbox: &ClientSandboxConfig,
    local_paths: &mut std::collections::HashSet<String>,
    extra_handlers: &mut Vec<ConfigHandlerInfo>,
    global_rules: &[GlobalRuleEntry],
    tool_params: &ToolParamsMap,
) -> Vec<ConfigItem> {
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        if let ConfigItemKind::Label = &item.kind {
            if let Some(key) = item.label.strip_prefix("_client:") {
                let expanded = resolve_client_placeholder(
                    &item.path, key, sandbox, extra_handlers, global_rules, tool_params
                );
                if key == "sandbox_config" {
                    for it in &expanded {
                        local_paths.insert(it.path.clone());
                    }
                }
                if key == "sandbox_rules" {
                    for it in &expanded {
                        // Local rule items have paths starting with sandbox.rules.local.
                        if it.path.starts_with("sandbox.rules.local.") {
                            local_paths.insert(it.path.clone());
                        }
                    }
                }
                result.extend(expanded);
                continue;
            }
        }
        result.push(item);
    }
    result
}

/// Resolve a single client-side placeholder into concrete items.
fn resolve_client_placeholder(
    base_path: &str,
    key: &str,
    sandbox: &ClientSandboxConfig,
    extra_handlers: &mut Vec<ConfigHandlerInfo>,
    global_rules: &[GlobalRuleEntry],
    tool_params: &ToolParamsMap,
) -> Vec<ConfigItem> {
    match key {
        "sandbox_availability" => sandbox_availability_labels(base_path),
        "sandbox_config" => sandbox_config_items(base_path, sandbox),
        "sandbox_rules" => sandbox_local_rule_items(sandbox, extra_handlers, global_rules, tool_params),
        _ => vec![],
    }
}

/// Generate Toggle + Select items for client-local sandbox config.
/// Paths use `sandbox.__enabled` / `sandbox.__backend` so they nest under
/// the existing Sandbox submenu in the config tree.
fn sandbox_config_items(base_path: &str, state: &ClientSandboxConfig) -> Vec<ConfigItem> {
    let parent = base_path.rsplit_once('.').map(|(p, _)| p).unwrap_or(base_path);
    let options: Vec<String> = if cfg!(target_os = "macos") {
        vec!["macos".to_string()]
    } else {
        vec!["bwrap".to_string(), "landlock".to_string()]
    };
    let selected = options.iter().position(|o| o == &state.backend).unwrap_or(0);
    vec![
        ConfigItem {
            path: format!("{}.__enabled", parent),
            label: crate::i18n::t("config.enabled").to_string(),
            kind: ConfigItemKind::Toggle { value: state.enabled },
            prefills: vec![],
        },
        ConfigItem {
            path: format!("{}.__backend", parent),
            label: crate::i18n::t("config.backend").to_string(),
            kind: ConfigItemKind::Select { options, selected },
            prefills: vec![],
        },
    ]
}

use omnish_common::sandbox_rule::{OPERATORS, parse_rule_parts};

/// Build the ConfigItems for a permit rule form (edit or add).
/// `with_delete=true` prepends a Delete toggle (for edit forms).
fn rule_form_fields(
    params: RuleFormParams<'_>,
    tool_params: &ToolParamsMap,
) -> Vec<ConfigItem> {
    let op_idx = OPERATORS.iter().position(|&o| o == params.operator).unwrap_or(0);
    let mut items = Vec::new();
    if let Some(s) = params.scope {
        items.push(ConfigItem {
            path: format!("{}._scope", params.prefix),
            label: crate::i18n::tf("sandbox.scope_label", &[("scope", s)]),
            kind: ConfigItemKind::Label,
            prefills: vec![],
        });
    }

    // Build sorted tool name list for the Plugin selector
    let mut tool_names: Vec<&String> = tool_params.keys().collect();
    tool_names.sort();

    if params.with_delete {
        // Edit form: Plugin is read-only label
        items.push(ConfigItem {
            path: format!("{}.plugin", params.prefix),
            label: format!("Plugin: {}", params.plugin),
            kind: ConfigItemKind::Label,
            prefills: vec![],
        });
    } else if tool_names.is_empty() {
        // No tool metadata available — fall back to TextInput
        items.push(ConfigItem {
            path: format!("{}.plugin", params.prefix),
            label: crate::i18n::t("config.plugin").to_string(),
            kind: ConfigItemKind::TextInput { value: params.plugin.to_string() },
            prefills: vec![],
        });
    } else {
        // Add form with tool metadata: Plugin is a Select with prefills
        let options: Vec<String> = tool_names.iter().map(|s| s.to_string()).collect();
        let selected = options.iter().position(|s| s == params.plugin).unwrap_or(0);

        // Build prefills: when plugin changes, update Param name options
        let prefills: Vec<(String, Vec<(String, String)>)> = options.iter().map(|tool_name| {
            let param_csv = tool_params.get(tool_name.as_str())
                .map(|v| v.join(","))
                .unwrap_or_default();
            (tool_name.clone(), vec![
                (crate::i18n::t("config.param_name").to_string(), param_csv),
            ])
        }).collect();

        items.push(ConfigItem {
            path: format!("{}.plugin", params.prefix),
            label: crate::i18n::t("config.plugin").to_string(),
            kind: ConfigItemKind::Select { options, selected },
            prefills,
        });
    }

    // Param name: Select if we know the params for the current/default plugin, else TextInput
    let effective_plugin = if params.plugin.is_empty() {
        tool_names.first().map(|s| s.as_str()).unwrap_or("")
    } else {
        params.plugin
    };
    let current_params = tool_params.get(effective_plugin);
    if let Some(param_list) = current_params.filter(|p| !p.is_empty() && !params.with_delete) {
        let selected = param_list.iter().position(|p| p == params.field).unwrap_or(0);
        items.push(ConfigItem {
            path: format!("{}.field", params.prefix),
            label: crate::i18n::t("config.param_name").to_string(),
            kind: ConfigItemKind::Select {
                options: param_list.clone(),
                selected,
            },
            prefills: vec![],
        });
    } else {
        items.push(ConfigItem {
            path: format!("{}.field", params.prefix),
            label: crate::i18n::t("config.param_name").to_string(),
            kind: ConfigItemKind::TextInput { value: params.field.to_string() },
            prefills: vec![],
        });
    }

    items.push(ConfigItem {
        path: format!("{}.operator", params.prefix),
        label: crate::i18n::t("config.operator").to_string(),
        kind: ConfigItemKind::Select {
            options: OPERATORS.iter().map(|s| s.to_string()).collect(),
            selected: op_idx,
        },
        prefills: vec![],
    });
    items.push(ConfigItem {
        path: format!("{}.value", params.prefix),
        label: crate::i18n::t("config.pattern").to_string(),
        kind: ConfigItemKind::TextInput { value: params.value.to_string() },
        prefills: vec![],
    });
    if params.with_delete {
        items.push(ConfigItem {
            path: format!("{}._delete", params.prefix),
            label: crate::i18n::t("config.delete").to_string(),
            kind: ConfigItemKind::Toggle { value: false },
            prefills: vec![],
        });
    }
    items
}

/// Generate sandbox rule items for the Rules submenu.
///
/// Produces a flat list directly under `sandbox.rules`:
/// - "Add permit rule" form with Scope selector (handler "add_rule")
/// - One pre-filled edit form per existing rule (global or local)
///
/// Handler infos for all submenus are pushed into `extra_handlers`.
fn sandbox_local_rule_items(
    sandbox: &ClientSandboxConfig,
    extra_handlers: &mut Vec<ConfigHandlerInfo>,
    global_rules: &[GlobalRuleEntry],
    tool_params: &ToolParamsMap,
) -> Vec<ConfigItem> {
    let mut items = Vec::new();
    let mut seq = 0usize; // sequential numbering for flat paths

    // "Add permit rule" form — single entry with scope selector
    let add_prefix = "sandbox.rules._add";
    extra_handlers.push(ConfigHandlerInfo {
        path: add_prefix.to_string(),
        label: crate::i18n::t("config.add_permit_rule").to_string(),
        handler: "add_rule".to_string(),
    });
    // Scope selector as first field
    items.push(ConfigItem {
        path: format!("{}.scope", add_prefix),
        label: crate::i18n::t("config.scope").to_string(),
        kind: ConfigItemKind::Select {
            options: vec!["local".to_string(), "global".to_string()],
            selected: 0,
        },
        prefills: vec![],
    });
    items.extend(rule_form_fields(RuleFormParams { prefix: add_prefix, plugin: "", field: "", operator: "starts_with", value: "", with_delete: false, scope: None, }, tool_params));

    // Global rules — editable forms (changes forwarded to daemon via RPC)
    for entry in global_rules {
        for (idx, rule) in entry.rules.iter().enumerate() {
            let prefix = format!("sandbox.rules._r{}", seq);
            seq += 1;
            let handler = format!("edit_global_rule:{}:{}", entry.plugin, idx);
            extra_handlers.push(ConfigHandlerInfo {
                path: prefix.clone(),
                label: format!("{} {} [global]", entry.plugin, rule),
                handler,
            });
            let (field, operator, value) = parse_rule_parts(rule);
            items.extend(rule_form_fields(RuleFormParams { prefix: &prefix, plugin: &entry.plugin, field: &field, operator: &operator, value: &value, with_delete: true, scope: Some("global"), }, tool_params));
        }
    }

    // Local rules — editable forms (handled client-side)
    let mut plugin_names: Vec<&String> = sandbox.plugins.keys().collect();
    plugin_names.sort();
    for plugin_name in plugin_names {
        let cfg = &sandbox.plugins[plugin_name];
        for (idx, rule) in cfg.permit_rules.iter().enumerate() {
            let prefix = format!("sandbox.rules._r{}", seq);
            seq += 1;
            extra_handlers.push(ConfigHandlerInfo {
                path: prefix.clone(),
                label: format!("{} {} [local]", plugin_name, rule),
                handler: format!("edit_local_rule:{}:{}", plugin_name, idx),
            });
            let (field, operator, value) = parse_rule_parts(rule);
            items.extend(rule_form_fields(RuleFormParams { prefix: &prefix, plugin: plugin_name, field: &field, operator: &operator, value: &value, with_delete: true, scope: Some("local"), }, tool_params));
        }
    }

    items
}

/// Save a client-local sandbox config change to client.toml and update the in-memory state.
///
/// `schema_path` is the item path like `sandbox.__enabled` / `sandbox.__backend`.
/// Uses a `.toml.lock` file (same convention as `save_client_config_cache`) to
/// prevent concurrent clobber with daemon-pushed client config writes.
/// Returns true on success, false on error.
fn save_local_sandbox_config(
    schema_path: &str,
    value: &str,
    sandbox_state: &Arc<RwLock<ClientSandboxConfig>>,
) -> bool {
    use fs2::FileExt;
    use toml_edit::DocumentMut;

    // Resolve the toml key and parse the typed value before acquiring the lock.
    enum Change { Enabled(bool), Backend(String) }
    let change = if schema_path.ends_with(".__enabled") {
        match value.parse::<bool>() {
            Ok(b) => Change::Enabled(b),
            Err(_) => {
                write_stdout(&format!("{RED}Invalid boolean: {}{RESET}\r\n", value));
                return false;
            }
        }
    } else if schema_path.ends_with(".__backend") {
        Change::Backend(value.to_string())
    } else {
        write_stdout(&format!("{RED}Unknown local config path: {}{RESET}\r\n", schema_path));
        return false;
    };

    let config_path = std::env::var("OMNISH_CLIENT_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| omnish_common::config::omnish_dir().join("client.toml"));
    let lock_path = config_path.with_extension("toml.lock");

    let result: anyhow::Result<()> = (|| {
        let lock_file = std::fs::File::create(&lock_path)?;
        lock_file.lock_exclusive()?;

        let content = if config_path.exists() {
            std::fs::read_to_string(&config_path)?
        } else {
            String::new()
        };
        let mut doc = content.parse::<DocumentMut>()?;

        match &change {
            Change::Enabled(v) => {
                omnish_common::config_edit::set_toml_nested_in_doc(&mut doc, "sandbox.enabled", toml_edit::value(*v))?;
            }
            Change::Backend(v) => {
                omnish_common::config_edit::set_toml_nested_in_doc(&mut doc, "sandbox.backend", toml_edit::value(v.as_str()))?;
            }
        }

        let output = doc.to_string();
        let output = if output.ends_with('\n') { output } else { format!("{}\n", output) };
        std::fs::write(&config_path, output)?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            match change {
                Change::Enabled(v) => sandbox_state.write().unwrap().enabled = v,
                Change::Backend(v) => sandbox_state.write().unwrap().backend = v,
            }
            true
        }
        Err(e) => {
            write_stdout(&format!("{RED}Failed to save sandbox config: {}{RESET}\r\n", e));
            false
        }
    }
}

/// Add a local permit rule to client.toml and update sandbox_state in-memory.
fn add_local_rule(
    changes: &[widgets::menu::MenuChange],
    sandbox_state: &Arc<RwLock<ClientSandboxConfig>>,
) -> bool {
    let plugin = find_change_value(changes, ".plugin").trim().to_string();
    let field   = find_change_value(changes, ".field").trim().to_string();
    let operator = find_change_value(changes, ".operator").trim().to_string();
    let value   = find_change_value(changes, ".value").trim().to_string();

    if plugin.is_empty() { write_stdout(&format!("{RED}Plugin is required{RESET}\r\n")); return false; }
    if field.is_empty()  { write_stdout(&format!("{RED}Param name is required{RESET}\r\n")); return false; }
    if value.is_empty()  { write_stdout(&format!("{RED}Pattern is required{RESET}\r\n")); return false; }

    let rule = format!("{} {} {}", field, operator.as_str().if_empty("starts_with"), value);
    let config_path = client_config_path();
    let array_key = format!("sandbox.plugins.{}.permit_rules", plugin);
    if let Err(e) = omnish_common::config_edit::append_to_toml_array(&config_path, &array_key, &rule) {
        write_stdout(&format!("{RED}Failed to save rule: {}{RESET}\r\n", e));
        return false;
    }
    sandbox_state.write().unwrap()
        .plugins.entry(plugin).or_default()
        .permit_rules.push(rule);
    true
}

/// Edit or delete a local permit rule in client.toml and update sandbox_state.
fn edit_local_rule(
    plugin: &str,
    idx: usize,
    changes: &[widgets::menu::MenuChange],
    sandbox_state: &Arc<RwLock<ClientSandboxConfig>>,
) -> bool {
    let delete = changes.iter().any(|c| c.path.ends_with("._delete") && c.value == "true");
    let config_path = client_config_path();
    let array_key = format!("sandbox.plugins.{}.permit_rules", plugin);

    if delete {
        if let Err(e) = omnish_common::config_edit::remove_from_toml_array(&config_path, &array_key, idx) {
            write_stdout(&format!("{RED}Failed to delete rule: {}{RESET}\r\n", e));
            return false;
        }
        let mut guard = sandbox_state.write().unwrap();
        if let Some(cfg) = guard.plugins.get_mut(plugin) {
            if idx < cfg.permit_rules.len() { cfg.permit_rules.remove(idx); }
        }
        return true;
    }

    let field    = find_change_value(changes, ".field").trim().to_string();
    let operator = find_change_value(changes, ".operator").trim().to_string();
    let value    = find_change_value(changes, ".value").trim().to_string();
    if field.is_empty() { write_stdout(&format!("{RED}Param name is required{RESET}\r\n")); return false; }
    if value.is_empty() { write_stdout(&format!("{RED}Pattern is required{RESET}\r\n")); return false; }

    let rule = format!("{} {} {}", field, operator.as_str().if_empty("starts_with"), value);
    if let Err(e) = omnish_common::config_edit::replace_in_toml_array(&config_path, &array_key, idx, &rule) {
        write_stdout(&format!("{RED}Failed to update rule: {}{RESET}\r\n", e));
        return false;
    }
    let mut guard = sandbox_state.write().unwrap();
    if let Some(cfg) = guard.plugins.get_mut(plugin) {
        if idx < cfg.permit_rules.len() { cfg.permit_rules[idx] = rule; }
    }
    true
}

fn find_change_value<'a>(changes: &'a [widgets::menu::MenuChange], suffix: &str) -> &'a str {
    changes.iter().find(|c| c.path.ends_with(suffix)).map(|c| c.value.as_str()).unwrap_or("")
}

fn client_config_path() -> std::path::PathBuf {
    std::env::var("OMNISH_CLIENT_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| omnish_common::config::omnish_dir().join("client.toml"))
}

/// Send a ConfigUpdate RPC to the daemon and return Ok(()) on success, Err(message) on failure.
fn send_config_update(
    rt: &tokio::runtime::Handle,
    rpc: &RpcClient,
    changes: Vec<ConfigChange>,
) -> Result<(), String> {
    let result = rt.block_on(async {
        rpc.call(Message::ConfigUpdate { changes }).await
    });
    match result {
        Ok(Message::ConfigUpdateResult { ok: false, error }) => {
            Err(format!("Handler error: {}", error.unwrap_or_default()))
        }
        Err(e) => Err(format!("RPC error: {}", e)),
        _ => Ok(()),
    }
}

trait StrOrDefault<'a> {
    fn if_empty(self, default: &'a str) -> &'a str;
}
impl<'a> StrOrDefault<'a> for &'a str {
    fn if_empty(self, default: &'a str) -> &'a str {
        if self.is_empty() { default } else { self }
    }
}

fn sandbox_availability_labels(base_path: &str) -> Vec<ConfigItem> {
    use omnish_plugin::SandboxBackendType;
    #[cfg(not(target_os = "macos"))]
    use omnish_plugin::BwrapUnavailableReason;
    use crate::display;

    // Use the parent path so labels sit at the same level as "backend",
    // not nested inside a sub-submenu.
    let parent = base_path.rsplit_once('.').map(|(p, _)| p).unwrap_or(base_path);
    let label = |suffix: &str, text: String| ConfigItem {
        path: format!("{}.__{}", parent, suffix),
        label: text,
        kind: ConfigItemKind::Label,
        prefills: vec![],
    };

    let avail = crate::i18n::t("sandbox.available");
    let not_avail = crate::i18n::t("sandbox.not_available");
    let mut labels = Vec::new();

    #[cfg(target_os = "macos")]
    {
        // On macOS only seatbelt is relevant.
        if omnish_plugin::is_available(SandboxBackendType::MacosSeatbelt) {
            labels.push(label("macos", format!(
                "  {}macos{}: {}{}{}", display::BRIGHT_WHITE, display::RESET, display::GREEN, avail, display::RESET,
            )));
        } else {
            labels.push(label("macos", format!(
                "  {}macos{}: {}{}{}", display::BRIGHT_WHITE, display::RESET, display::RED, not_avail, display::RESET,
            )));
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        // bwrap
        if omnish_plugin::is_available(SandboxBackendType::Bwrap) {
            labels.push(label("bwrap", format!(
                "  {}bwrap{}: {}{}{}", display::BRIGHT_WHITE, display::RESET, display::GREEN, avail, display::RESET,
            )));
        } else {
            labels.push(label("bwrap", format!(
                "  {}bwrap{}: {}{}{}", display::BRIGHT_WHITE, display::RESET, display::RED, not_avail, display::RESET,
            )));
            match omnish_plugin::bwrap_unavailable_reason() {
                Some(BwrapUnavailableReason::NotInstalled) => {
                    labels.push(label("bwrap_hint", format!(
                        "  {}{}{}", display::DIM, crate::i18n::t("sandbox.hint_install_bwrap"), display::RESET,
                    )));
                }
                Some(BwrapUnavailableReason::NamespaceDenied) => {
                    labels.push(label("bwrap_hint", format!(
                        "  {}{}{}", display::DIM, crate::i18n::t("sandbox.hint_ns_denied"), display::RESET,
                    )));
                }
                None => {}
            }
        }

        // landlock
        if omnish_plugin::is_available(SandboxBackendType::Landlock) {
            labels.push(label("landlock", format!(
                "  {}landlock{}: {}{}{}", display::BRIGHT_WHITE, display::RESET, display::GREEN, avail, display::RESET,
            )));
        } else {
            labels.push(label("landlock", format!(
                "  {}landlock{}: {}{}{}", display::BRIGHT_WHITE, display::RESET, display::RED, not_avail, display::RESET,
            )));
            labels.push(label("landlock_hint", format!(
                "  {}{}{}", display::DIM, crate::i18n::t("sandbox.hint_landlock_kernel"), display::RESET,
            )));
        }
    }

    labels
}

/// Extract the current value string from a ConfigItem.
fn item_value(item: &ConfigItem) -> String {
    match &item.kind {
        ConfigItemKind::Toggle { value } => value.to_string(),
        ConfigItemKind::Select { options, selected } => {
            options.get(*selected).cloned().unwrap_or_default()
        }
        ConfigItemKind::TextInput { value } => {
            if value.is_empty() {
                crate::i18n::t("empty").to_string()
            } else {
                value.clone()
            }
        }
        ConfigItemKind::Label | ConfigItemKind::Data { .. } => String::new(),
    }
}

/// Compute diff between two config item snapshots by matching on `path`.
/// Uses BTreeMap for deterministic traversal order and sorts output by label.
fn compute_config_diff(old_items: &[ConfigItem], new_items: &[ConfigItem]) -> Vec<ConfigDiff> {
    // Skip Label items — they're non-interactive and include client-side
    // placeholders that differ between daemon response and expanded form.
    let old_map: std::collections::BTreeMap<&str, &ConfigItem> = old_items.iter()
        .filter(|i| !matches!(i.kind, ConfigItemKind::Label | ConfigItemKind::Data { .. }))
        .map(|i| (i.path.as_str(), i))
        .collect();
    let new_map: std::collections::BTreeMap<&str, &ConfigItem> = new_items.iter()
        .filter(|i| !matches!(i.kind, ConfigItemKind::Label | ConfigItemKind::Data { .. }))
        .map(|i| (i.path.as_str(), i))
        .collect();

    let mut diffs = Vec::new();

    // Changed or removed items (sorted by path via BTreeMap)
    for (path, old_item) in &old_map {
        let segments = omnish_common::config_edit::split_key_path(path);
        if let Some(new_item) = new_map.get(path) {
            let real_old = item_value(old_item);
            let real_new = item_value(new_item);
            if real_old != real_new {
                let (old_display, new_display) = if is_sensitive_path(path) {
                    mask_sensitive_value(&real_old)
                } else if *path == "general.language" {
                    (lang_code_to_display(&real_old).to_string(),
                     lang_code_to_display(&real_new).to_string())
                } else {
                    (real_old, real_new)
                };
                diffs.push(ConfigDiff {
                    label: item_display_label(segments, &new_item.label),
                    old_value: old_display,
                    new_value: new_display,
                });
            }
        } else {
            let display = if is_sensitive_path(path) {
                "***hidden***".to_string()
            } else {
                item_value(old_item)
            };
            diffs.push(ConfigDiff {
                label: item_display_label(segments, &old_item.label),
                old_value: display,
                new_value: "(removed)".to_string(),
            });
        }
    }

    // Added items (present in new but not old)
    for (path, new_item) in &new_map {
        if !old_map.contains_key(path) {
            let display = if is_sensitive_path(path) {
                "***hidden***".to_string()
            } else {
                item_value(new_item)
            };
            let segments = omnish_common::config_edit::split_key_path(path);
            diffs.push(ConfigDiff {
                label: item_display_label(segments, &new_item.label),
                old_value: "(not set)".to_string(),
                new_value: display,
            });
        }
    }

    // Sort by label for deterministic display order
    diffs.sort_by(|a, b| a.label.cmp(&b.label));
    diffs
}

/// Truncate a string to max `n` Unicode characters (not bytes), appending "…" if truncated.
fn char_truncate(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{}…", truncated)
    }
}

/// Display a list of config diffs to the user.
fn display_config_diff(diffs: &[ConfigDiff]) {
    let val_width = 40; // truncation limit for very long values
    let max_label_chars = diffs.iter().map(|d| d.label.chars().count()).max().unwrap_or(0);
    let col2_start = (max_label_chars + 2).clamp(22, 60);

    write_stdout(&format!("\r\n{BOLD}Config changes:{RESET}\r\n"));

    for d in diffs {
        let label = char_truncate(&d.label, col2_start);
        let label_padding = col2_start.saturating_sub(label.chars().count()) + 2;

        write_stdout(&format!(
            "  {}{:<pad$}{GRAY}{}{RESET} {YELLOW}→{RESET} {GREEN}{}{RESET}\r\n",
            label,
            "",
            char_truncate(&d.old_value, val_width),
            char_truncate(&d.new_value, val_width),
            pad = label_padding,
        ));
    }
    write_stdout("\r\n");
}

/// Build MenuItem tree from flat ConfigItems and handler info.
fn build_menu_tree(
    items: &[ConfigItem],
    handlers: &[ConfigHandlerInfo],
) -> (Vec<widgets::menu::MenuItem>, HashMap<String, String>) {
    use widgets::menu::MenuItem;
    let mut root: Vec<MenuItem> = Vec::new();
    let mut path_map: HashMap<String, String> = HashMap::new();

    let submenu_lookup: HashMap<&str, (&str, &str)> = handlers.iter()
        .map(|h| (h.path.as_str(), (h.handler.as_str(), h.label.as_str())))
        .collect();

    for item in items {
        let segments = omnish_common::config_edit::split_key_path(&item.path);
        let mut current = &mut root;
        for (i, seg) in segments.iter().enumerate() {
            if i == segments.len() - 1 {
                // Leaf item
                // ._delete toggles render as destructive Buttons
                let translated = crate::i18n::translate_label(&item.label);
                let menu_item = if item.path.ends_with("._delete") {
                    MenuItem::Button { label: translated.clone() }
                } else {
                    match &item.kind {
                        ConfigItemKind::Toggle { value } => MenuItem::Toggle {
                            label: translated.clone(),
                            value: *value,
                        },
                        ConfigItemKind::Select { options, selected } => {
                            let display_options = if item.path == "general.language" {
                                options.iter().map(|o| lang_code_to_display(o).to_string()).collect()
                            } else {
                                options.clone()
                            };
                            MenuItem::Select {
                                label: translated.clone(),
                                options: display_options,
                                selected: *selected,
                                prefills: item.prefills.clone(),
                            }
                        },
                        ConfigItemKind::TextInput { value } => MenuItem::TextInput {
                            label: translated.clone(),
                            value: value.clone(),
                        },
                        ConfigItemKind::Label => MenuItem::Label {
                            label: translated.clone(),
                        },
                        ConfigItemKind::Data { .. } => continue, // data items are invisible
                    }
                };
                current.push(menu_item);

                // Build display path for path_map reverse lookup
                let mut display_parts: Vec<String> = Vec::new();
                let mut schema_prefix = String::new();
                for (j, s) in segments[..i].iter().enumerate() {
                    if j > 0 { schema_prefix.push('.'); }
                    schema_prefix.push_str(s);
                    let label = submenu_lookup.get(schema_prefix.as_str())
                        .map(|(_, lbl)| crate::i18n::translate_label(lbl))
                        .unwrap_or_else(|| crate::i18n::translate_label(&segment_to_label(s)));
                    display_parts.push(label);
                }
                display_parts.push(crate::i18n::translate_label(&item.label));
                let display_key = display_parts.join(".");
                path_map.insert(display_key, item.path.clone());
            } else {
                // Intermediate segment — find or create submenu
                let schema_path_so_far = segments[..=i].join(".");
                let label = submenu_lookup.get(schema_path_so_far.as_str())
                    .map(|(_, lbl)| crate::i18n::translate_label(lbl))
                    .unwrap_or_else(|| crate::i18n::translate_label(&segment_to_label(seg)));

                let pos = current.iter().position(|m| {
                    matches!(m, MenuItem::Submenu { label: l, .. } if *l == label)
                });
                let idx = match pos {
                    Some(idx) => idx,
                    None => {
                        let handler = submenu_lookup.get(schema_path_so_far.as_str())
                            .and_then(|(name, _)| if name.is_empty() { None } else { Some(name.to_string()) });
                        // Submenus with a handler are forms: fields filled by the user
                        // are collected and dispatched to the handler on Done/ESC.
                        let form_mode = handler.is_some();
                        current.push(MenuItem::Submenu {
                            label: label.clone(),
                            children: Vec::new(),
                            handler,
                            form_mode,
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

impl ChatSession {
    pub fn new(
        chat_history: VecDeque<String>,
        extended_unicode: bool,
        sandbox_state: Arc<RwLock<ClientSandboxConfig>>,
    ) -> Self {
        // Snapshot sandbox config for ClientPluginManager; subsequent menu
        // edits only affect the NEXT chat session.
        let (sandbox_enabled, sandbox_backend) = {
            let s = sandbox_state.read().unwrap();
            (s.enabled, s.backend.clone())
        };
        Self {
            current_thread_id: None,
            cached_thread_ids: Vec::new(),
            chat_history,
            history_index: None,
            completer: ghost_complete::GhostCompleter::new(vec![
                Box::new(ghost_complete::BuiltinProvider::new()),
            ]),
            scroll_history: Vec::new(),
            thinking_visible: false,
            has_activity: false,
            pending_input: None,
            client_plugins: Arc::new(client_plugin::ClientPluginManager::new(
                sandbox_enabled,
                &sandbox_backend,
            )),
            ghost_hint_shown: false,
            pending_model: None,
            resumed_model: None,
            shell_cwd: None,
            pending_cd: None,
            extended_unicode,
            lines_printed: 0,
            tool_section_start: None,
            tool_section_hist_idx: None,
            spinner_frame: 0,
            sandbox_state,
            cancelled_input: None,
            pending_sandbox_off: None,
        }
    }

    pub fn sandbox_status(&self) -> omnish_plugin::SandboxDetectResult {
        self.client_plugins.sandbox_status()
    }

    pub fn into_history(self) -> VecDeque<String> {
        self.chat_history
    }

    /// Return the thread ID used in this chat session (if any).
    pub fn thread_id(&self) -> Option<&str> {
        self.current_thread_id.as_deref()
    }

    /// Return a pending cd path set by resume mismatch handler.
    pub fn pending_cd(&self) -> Option<&str> {
        self.pending_cd.as_deref()
    }

    fn show_thinking(&mut self) {
        write_stdout(&format!("{}(thinking...){}\r\n", crate::display::DIM, crate::display::RESET));
        self.thinking_visible = true;
    }

    fn erase_thinking(&mut self) {
        if self.thinking_visible {
            write_stdout("\x1b[1A\r\x1b[K");
            self.thinking_visible = false;
        }
    }

    fn print_line(&mut self, line: &str) {
        write_stdout(line);
        write_stdout("\r\n");
        let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
        self.lines_printed += Self::visual_rows(line, cols as usize);
    }

    /// How many terminal rows a line occupies (accounting for wrapping).
    fn visual_rows(line: &str, cols: usize) -> usize {
        let w = display::display_width(line);
        if w == 0 || cols == 0 { 1 } else { ((w - 1) / cols) + 1 }
    }

    /// Re-render the tool section from tool_section_start.
    /// Moves cursor up, erases, and re-renders all ToolStatus entries with their output.
    fn redraw_tool_section(&mut self) {
        let start_line = match self.tool_section_start {
            Some(s) => s,
            None => return,
        };
        let hist_start = match self.tool_section_hist_idx {
            Some(s) => s,
            None => return,
        };

        let lines_up = self.lines_printed - start_line;
        if lines_up > 0 {
            write_stdout(&format!("\x1b[{}A", lines_up));
        }
        write_stdout("\r\x1b[J"); // erase from cursor to end of screen

        let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
        let cols = cols as usize;
        let mut count = 0usize;

        // Track leading completed entries so we can advance past them after
        // rendering, avoiding redundant redraws on future spinner ticks.
        let mut advance_lines = 0usize;
        let mut advance_entries = 0usize;
        let mut seen_running = false;

        for (idx, entry) in self.scroll_history[hist_start..].iter().enumerate() {
            match entry {
                ScrollEntry::ToolStatus(cts) => {
                    let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                    let param_desc = cts.param_desc.as_deref().unwrap_or("");
                    let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Running);
                    let sf = if *icon == StatusIcon::Running { Some(self.spinner_frame) } else { None };
                    if *icon == StatusIcon::Running { seen_running = true; }
                    let header = display::render_tool_header_with_spinner(icon, display_name, param_desc, cols, sf);
                    write_stdout(&header);
                    write_stdout("\r\n");
                    count += Self::visual_rows(&header, cols);
                    if let Some(ref lines) = cts.result_compact {
                        let rendered = display::render_tool_output_with_cols(lines, cols, self.extended_unicode);
                        for line in &rendered {
                            write_stdout(line);
                            write_stdout("\r\n");
                            count += Self::visual_rows(line, cols);
                        }
                    }
                    if !seen_running {
                        advance_lines = count;
                        advance_entries = idx + 1;
                    }
                }
                ScrollEntry::LlmText(text) => {
                    write_stdout("\r\n");
                    count += 1;
                    for (i, line) in text.split('\n').enumerate() {
                        let formatted = if i == 0 {
                            format!("{BRIGHT_WHITE}●{RESET} {}", line)
                        } else {
                            format!("  {}", line)
                        };
                        write_stdout(&formatted);
                        write_stdout("\r\n");
                        count += Self::visual_rows(&formatted, cols);
                    }
                    if !seen_running {
                        advance_lines = count;
                        advance_entries = idx + 1;
                    }
                }
                _ => {}
            }
        }

        self.lines_printed = start_line + count;

        // Advance section markers past completed entries so future spinner
        // redraws only cover the still-running portion.
        if advance_entries > 0 {
            self.tool_section_start = Some(start_line + advance_lines);
            self.tool_section_hist_idx = Some(hist_start + advance_entries);
        }
    }

    /// Mark all Running tool statuses as Error and redraw.
    fn mark_running_tools_error(&mut self) {
        let mut changed = false;
        for entry in &mut self.scroll_history {
            if let ScrollEntry::ToolStatus(cts) = entry {
                if cts.status_icon == Some(StatusIcon::Running) {
                    cts.status_icon = Some(StatusIcon::Error);
                    changed = true;
                }
            }
        }
        if changed {
            self.redraw_tool_section();
        }
    }

    fn push_entry(&mut self, entry: ScrollEntry) {
        self.scroll_history.push(entry);
    }

    fn browse_history(&self) {
        if self.scroll_history.is_empty() {
            return;
        }
        let (rows, cols) = super::get_terminal_size().unwrap_or((24, 80));
        let lines: Vec<String> = self.scroll_history.iter().flat_map(|entry| {
            match entry {
                ScrollEntry::UserInput(text) => {
                    text.lines().enumerate().map(|(i, line)| {
                        if i == 0 {
                            format!("{CYAN}> {RESET}{}", line)
                        } else {
                            format!("  {}", line)
                        }
                    }).collect::<Vec<_>>()
                }
                ScrollEntry::ToolStatus(cts) => {
                    let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                    let param_desc = cts.param_desc.as_deref().unwrap_or("");
                    let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Success);
                    let mut lines = vec![display::render_tool_header_full(icon, display_name, param_desc)];
                    if let Some(ref full) = cts.result_full {
                        lines.extend(display::render_tool_output(full, self.extended_unicode));
                    }
                    lines
                }
                ScrollEntry::LlmText(text) => {
                    let mut out = vec![String::new()];
                    for (i, line) in text.split('\n').enumerate() {
                        if i == 0 {
                            out.push(format!("{BRIGHT_WHITE}●{RESET} {}", line));
                        } else {
                            out.push(format!("  {}", line));
                        }
                    }
                    out
                }
                ScrollEntry::Response(content) => {
                    let rendered = super::markdown::render(content);
                    let mut out = vec![String::new()]; // empty line before response
                    for (i, line) in rendered.split("\r\n").enumerate() {
                        if i == 0 {
                            out.push(format!("{BRIGHT_WHITE}●{RESET} {}", line));
                        } else {
                            out.push(format!("  {}", line));
                        }
                    }
                    out
                }
                ScrollEntry::Separator => {
                    vec![display::render_separator_plain(cols)]
                }
                ScrollEntry::SystemMessage(msg) => {
                    vec![format!("{}{}{}", crate::display::DIM, msg, crate::display::RESET)]
                }
            }
        }).collect();

        if lines.is_empty() {
            return;
        }

        let compact_h = (rows as usize / 3).max(3);
        let expanded_h = (rows as usize).saturating_sub(3);
        let mut sv = ScrollView::new(compact_h, expanded_h, cols as usize);
        for line in &lines {
            sv.push_line(line);
        }
        sv.run_browse();
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &mut self,
        rpc: &RpcClient,
        session_id: &str,
        proxy: &PtyProxy,
        initial_msg: Option<String>,
        client_debug_fn: &dyn Fn() -> String,
        onboarded: &AtomicBool,
        cursor_col: u16,
        cursor_row: u16,
    ) -> ChatExitAction {
        // Eagerly update cwd so the daemon has the current value before any chat message.
        // Without this, polling lag (up to 60s) can cause chat to see a stale cwd (#354).
        if let Some(cwd) = crate::get_shell_cwd(proxy.child_pid() as u32) {
            self.shell_cwd = Some(cwd.clone());
            let mut attrs = std::collections::HashMap::new();
            attrs.insert("shell_cwd".to_string(), cwd);
            let msg = Message::SessionUpdate(SessionUpdate {
                session_id: session_id.to_string(),
                timestamp_ms: crate::timestamp_ms(),
                attrs,
            });
            // Use call() (not send()) to wait for Ack — the daemon spawns each
            // message as a separate tokio task, so fire-and-forget send() can race
            // with the subsequent ChatMessage.
            let _ = rpc.call(msg).await;
        }

        let is_resumed = initial_msg.as_ref()
            .map(|m| m.starts_with("/resume"))
            .unwrap_or(false);
        let show_ghost_hint = initial_msg.is_none() || is_resumed;
        self.pending_input = initial_msg;

        // Move past shell prompt to a new line
        write_stdout("\r\n");

        let mut exit_action = ChatExitAction::Normal;
        loop {
            let (input, is_fast_resume) = if let Some(msg) = self.pending_input.take() {
                (msg, true)
            } else {
                write_stdout(&format!("{CYAN}> {RESET}"));
                let initial = self.cancelled_input.take();
                // Show ghost hint on first prompt (only if no cancelled input to restore)
                if initial.is_none() && show_ghost_hint && !self.ghost_hint_shown {
                    self.ghost_hint_shown = true;
                    let hint = if let Some(ref model) = self.resumed_model {
                        format!("model for conversation: {}", model)
                    } else if is_resumed {
                        String::new()
                    } else {
                        "type to start, /resume to continue".to_string()
                    };
                    if !hint.is_empty() {
                        write_stdout(&format!("\x1b7{DIM}{}{RESET}\x1b8", hint));
                    }
                }
                crate::event_log::push(format!("chat_loop: entering read_input_with allow_backspace_exit={}", !self.has_activity));
                let result = self.read_input_with(!self.has_activity, initial.as_deref());
                crate::event_log::push(format!("chat_loop: read_input_with returned {}", if result.is_some() { "Some" } else { "None" }));
                match result {
                    Some(line) => {
                        write_stdout("\r\n");
                        (line, false)
                    }
                    None => break,
                }
            };

            let trimmed = input.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Mark onboarded on first chat entry
            if !onboarded.load(Ordering::Relaxed) {
                onboarded.store(true, Ordering::Relaxed);
                crate::onboarding::mark_onboarded();
            }

            // Add user input to scroll history for browse mode (Ctrl+O)
            self.push_entry(ScrollEntry::UserInput(trimmed.to_string()));

            let is_inspection = trimmed.starts_with("/debug")
                || trimmed.starts_with("/context")
                || trimmed.starts_with("/template");
            let auto_exit = !self.has_activity && is_inspection;

            self.has_activity = true;
            save_to_history(&mut self.chat_history, trimmed, 100);
            self.history_index = None;

            // /thread del [N]
            if trimmed == "/thread del" || trimmed.starts_with("/thread del ") {
                self.handle_thread_del(trimmed, session_id, rpc).await;
                continue;
            }

            // /thread list [N]
            if trimmed == "/thread list" || trimmed.starts_with("/thread list ") {
                let limit = trimmed.strip_prefix("/thread list")
                    .and_then(|s| s.trim().parse::<usize>().ok());
                self.handle_thread_list(session_id, rpc, limit).await;
                continue;
            }

            // /thread sandbox [on|off]
            if trimmed == "/thread sandbox"
                || trimmed == "/thread sandbox on"
                || trimmed == "/thread sandbox off"
            {
                self.handle_thread_sandbox(trimmed, session_id, rpc).await;
                continue;
            }

            // /resume_tid <thread_id> (internal — used by :: resume shortcut)
            if let Some(tid) = trimmed.strip_prefix("/resume_tid ") {
                if !self.handle_resume_tid(tid.trim(), session_id, rpc).await && is_fast_resume {
                    break; // cancelled or failed on auto-resume — exit chat mode
                }
                continue;
            }

            // /resume [N]
            if trimmed == "/resume" || trimmed.starts_with("/resume ") {
                if !self.handle_resume(trimmed, session_id, rpc).await && is_fast_resume {
                    break; // cancelled or failed on auto-resume — exit chat mode
                }
                continue;
            }

            // /model
            if trimmed == "/model" {
                self.handle_model(session_id, rpc).await;
                continue;
            }

            // /test — hidden test commands
            if trimmed == "/test" || trimmed.starts_with("/test ") {
                let arg = trimmed.strip_prefix("/test").unwrap().trim();
                match arg {
                    "" => {
                        write_stdout(&format!("{DIM}Available /test commands:{RESET}\r\n"));
                        write_stdout(&format!("{DIM}  /test picker [N]          — flat picker (N = initial index){RESET}\r\n"));
                        write_stdout(&format!("{DIM}  /test multi_level_picker  — cascading picker (3 levels){RESET}\r\n"));
                        write_stdout(&format!("{DIM}  /test menu                — multi-level menu widget{RESET}\r\n"));
                        write_stdout(&format!("{DIM}  /test lock on|off         — toggle Landlock sandbox for shell{RESET}\r\n"));
                        write_stdout(&format!("{DIM}  /test disconnect N1 [N2]  — daemon disconnects after N1s, reconnect delay N2s{RESET}\r\n"));
                    }
                    "multi_level_picker" => self.handle_test_multi_level_picker(),
                    "menu" => self.handle_test_menu(),
                    other => {
                        if other == "picker" || other.starts_with("picker ") {
                            let idx: usize = other.strip_prefix("picker")
                                .unwrap().trim().parse().unwrap_or(0);
                            self.handle_test_picker(idx);
                        } else if other.starts_with("disconnect") {
                            self.handle_test_disconnect(other, rpc).await;
                        } else {
                            write_stdout(&format!(
                                "{DIM}Unknown test: {}. Run /test for a list.{RESET}\r\n",
                                other
                            ));
                        }
                    }
                }
                continue;
            }

            // /config
            if trimmed == "/config" {
                self.handle_config(session_id, rpc).await;
                continue;
            }

            // /context
            if trimmed == "/context" || trimmed.starts_with("/context ") {
                let (without_redirect, redirect) = command::parse_redirect_pub(trimmed);
                let (base_cmd, limit) = command::parse_limit_pub(without_redirect);
                let query = if base_cmd == "/context chat" {
                    if let Some(ref tid) = self.current_thread_id {
                        format!("__cmd:context chat:{}", tid)
                    } else {
                        "__cmd:context chat".to_string()
                    }
                } else if let Some(ref tid) = self.current_thread_id {
                    format!("__cmd:context chat:{}", tid)
                } else {
                    "__cmd:context".to_string()
                };
                let request_id = Uuid::new_v4().to_string()[..8].to_string();
                let request = Message::Request(Request {
                    request_id: request_id.clone(),
                    session_id: session_id.to_string(),
                    query,
                    scope: RequestScope::AllSessions,
                });
                match rpc.call(request).await {
                    Ok(Message::Response(resp)) if resp.request_id == request_id => {
                        let display_text = if let Some(json) = super::parse_cmd_response(&resp.content) {
                            super::cmd_display_str(&json)
                        } else {
                            resp.content
                        };
                        let display_text = if let Some(ref l) = limit {
                            command::apply_limit(&display_text, l)
                        } else {
                            display_text
                        };
                        if let Some(path) = redirect {
                            super::handle_command_result(&display_text, Some(path), self.shell_cwd.as_deref());
                        } else {
                            let output = display::render_response(&display_text);
                            write_stdout(&output);
                        }
                    }
                    _ => {
                        write_stdout(&display::render_error(crate::i18n::t("error.failed_get_context")));
                    }
                }
                if auto_exit { break; }
                continue;
            }

            // /test lock on|off
            if trimmed == "/test lock on" || trimmed == "/test lock off" {
                let lock = trimmed == "/test lock on";
                exit_action = ChatExitAction::Lock(lock);
                break;
            }

            // Other /commands
            if trimmed.starts_with('/')
                && super::handle_slash_command(
                    trimmed, session_id, rpc, proxy, self.shell_cwd.as_deref(), client_debug_fn, cursor_col, cursor_row,
                )
                .await
            {
                if auto_exit { break; }
                continue;
            }

            // Lazily create thread
            if self.current_thread_id.is_none() {
                let req_id = Uuid::new_v4().to_string()[..8].to_string();
                let start_msg = Message::ChatStart(ChatStart {
                    request_id: req_id.clone(),
                    session_id: session_id.to_string(),
                    new_thread: true,
                    thread_id: None,
                });
                match rpc.call(start_msg).await {
                    Ok(Message::ChatReady(ready)) if ready.request_id == req_id => {
                        self.current_thread_id = Some(ready.thread_id);
                        // Apply buffered /thread sandbox preference
                        if let Some(off) = self.pending_sandbox_off.take() {
                            let arg = if off { "off" } else { "on" };
                            let query = format!("__cmd:thread sandbox {}:{}",
                                arg, self.current_thread_id.as_deref().unwrap());
                            let rid2 = Uuid::new_v4().to_string()[..8].to_string();
                            let req = Message::Request(Request {
                                request_id: rid2,
                                session_id: session_id.to_string(),
                                query,
                                scope: RequestScope::AllSessions,
                            });
                            let _ = rpc.call(req).await;
                        }
                    }
                    _ => {
                        write_stdout(&display::render_error(crate::i18n::t("error.failed_start_chat")));
                        continue;
                    }
                }
            }

            // Show thinking indicator
            self.show_thinking();

            // Send ChatMessage
            let req_id = Uuid::new_v4().to_string()[..8].to_string();
            let chat_msg = Message::ChatMessage(omnish_protocol::message::ChatMessage {
                request_id: req_id.clone(),
                session_id: session_id.to_string(),
                thread_id: self.current_thread_id.clone().unwrap(),
                query: trimmed.to_string(),
                model: self.pending_model.take(),
            });

            // Ctrl-C cancellation
            let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let (stop_tx, stop_rx) = std::sync::mpsc::channel();
            tokio::task::spawn_blocking(move || {
                if wait_for_ctrl_c(stop_rx) {
                    let _ = cancel_tx.send(true);
                }
            });

            let rpc_result = rpc.call_stream(chat_msg);
            let mut interrupted = false;
            let mut got_first_output = false;

            async fn wait_cancel(rx: &mut tokio::sync::watch::Receiver<bool>) {
                loop {
                    if *rx.borrow() { return; }
                    if rx.changed().await.is_err() {
                        std::future::pending::<()>().await;
                    }
                }
            }

            // Race initial RPC call against Ctrl-C
            let stream_result;
            {
                let mut crx = cancel_rx.clone();
                tokio::select! {
                    result = rpc_result => { stream_result = Some(result); }
                    _ = wait_cancel(&mut crx) => {
                        interrupted = true;
                        stream_result = None;
                    }
                }
            }

            if let Some(result) = stream_result {
                match result {
                    Ok(mut rx) => {
                        let mut spinner_interval = tokio::time::interval(std::time::Duration::from_millis(200));
                        spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        // Consume the first immediate tick
                        spinner_interval.tick().await;

                        'stream: loop {
                            // Phase 1: Collect messages
                            let mut tool_calls: Vec<ChatToolCall> = Vec::new();
                            #[allow(unused_assignments)]
                            let mut got_response = false;
                            loop {
                                let mut crx = cancel_rx.clone();
                                tokio::select! {
                                    msg = rx.recv() => {
                                        match msg {
                                            Some(Message::ChatToolStatus(cts)) => {
                                                got_first_output = true;
                                                self.erase_thinking();
                                                if cts.tool_name.is_empty() {
                                                    // LLM intermediate text
                                                    self.print_line("");
                                                    for (i, line) in cts.status.split('\n').enumerate() {
                                                        if i == 0 {
                                                            self.print_line(&format!("{BRIGHT_WHITE}●{RESET} {}", line));
                                                        } else {
                                                            self.print_line(&format!("  {}", line));
                                                        }
                                                    }
                                                    self.push_entry(ScrollEntry::LlmText(cts.status.clone()));
                                                } else if cts.result_compact.is_none() {
                                                    // First status — tool is running (before execution)
                                                    if self.tool_section_start.is_none() {
                                                        self.tool_section_start = Some(self.lines_printed);
                                                        self.tool_section_hist_idx = Some(self.scroll_history.len());
                                                    }
                                                    let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
                                                    let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                                                    let param_desc = cts.param_desc.as_deref().unwrap_or("");
                                                    let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Running);
                                                    let sf = if *icon == StatusIcon::Running { Some(self.spinner_frame) } else { None };
                                                    let header = display::render_tool_header_with_spinner(icon, display_name, param_desc, cols as usize, sf);
                                                    self.print_line(&header);
                                                    self.push_entry(ScrollEntry::ToolStatus(cts));
                                                } else {
                                                    // Second status — tool completed (after execution)
                                                    // Update matching ToolStatus entry in scroll_history
                                                    let tool_call_id = cts.tool_call_id.clone();
                                                    if let Some(entry) = self.scroll_history.iter_mut().rev().find(|e| {
                                                        matches!(e, ScrollEntry::ToolStatus(prev)
                                                            if prev.tool_call_id == tool_call_id)
                                                    }) {
                                                        *entry = ScrollEntry::ToolStatus(cts.clone());
                                                    }
                                                    // Re-render entire tool section with updated statuses
                                                    self.redraw_tool_section();
                                                }
                                            }
                                            Some(Message::ChatToolCall(tc)) => {
                                                tool_calls.push(tc);
                                            }
                                            Some(Message::ChatResponse(resp)) if resp.request_id == req_id => {
                                                got_first_output = true;
                                                self.erase_thinking();
                                                self.tool_section_start = None;
                                                self.tool_section_hist_idx = None;
                                                self.print_line("");
                                                let rendered = markdown::render(&resp.content);
                                                for (i, line) in rendered.split("\r\n").enumerate() {
                                                    if i == 0 {
                                                        self.print_line(&format!("{BRIGHT_WHITE}●{RESET} {}", line));
                                                    } else {
                                                        self.print_line(&format!("  {}", line));
                                                    }
                                                }
                                                self.push_entry(ScrollEntry::Response(resp.content.clone()));
                                                let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
                                                self.print_line(&display::render_separator(cols));
                                                self.push_entry(ScrollEntry::Separator);
                                                got_response = true;
                                                break;
                                            }
                                            None => {
                                                if tool_calls.is_empty() {
                                                    // No tool calls pending — real disconnect
                                                    self.erase_thinking();
                                                    self.mark_running_tools_error();
                                                    self.tool_section_start = None;
                                                    self.tool_section_hist_idx = None;
                                                    write_stdout(&display::render_error(
                                                        "Daemon connection lost",
                                                    ));
                                                    got_response = true;
                                                }
                                                // else: normal stream end after tool calls forwarded
                                                break;
                                            }
                                            _ => { got_response = true; break; }
                                        }
                                    }
                                    _ = spinner_interval.tick(), if self.tool_section_start.is_some() => {
                                        self.spinner_frame = self.spinner_frame.wrapping_add(1);
                                        self.redraw_tool_section();
                                    }
                                    _ = wait_cancel(&mut crx) => {
                                        interrupted = true;
                                        break 'stream;
                                    }
                                }
                            }

                            if got_response || tool_calls.is_empty() {
                                break 'stream;
                            }

                            // Phase 2+3: Execute tools in parallel, send results as they complete
                            let shell_cwd = super::get_shell_cwd(proxy.child_pid() as u32);
                            let total = tool_calls.len();
                            let mut join_set = tokio::task::JoinSet::new();
                            for (idx, tc) in tool_calls.iter().enumerate() {
                                let plugins = Arc::clone(&self.client_plugins);
                                let tool_name = tc.tool_name.clone();
                                let plugin_name = tc.plugin_name.clone();
                                let mut sandboxed = tc.sandboxed;
                                if sandboxed {
                                    // Daemon only checks global rules; also check client-local rules.
                                    let local_sandbox = self.sandbox_state.read().unwrap();
                                    if let Some(plugin_cfg) = local_sandbox.plugins.get(&tc.tool_name) {
                                        let input: serde_json::Value =
                                            serde_json::from_str(&tc.input).unwrap_or_default();
                                        if let Some(rule) = omnish_common::sandbox_rule::check_bypass_raw(
                                            &plugin_cfg.permit_rules, &input,
                                        ) {
                                            sandboxed = false;
                                            crate::event_log::push(format!(
                                                "tool '{}' sandbox bypassed by local rule: {}",
                                                tc.tool_name, rule,
                                            ));
                                        }
                                    }
                                }
                                if !sandboxed {
                                    crate::event_log::push(format!(
                                        "tool '{}' running without sandbox (permit rule match)",
                                        tc.tool_name,
                                    ));
                                }
                                let tool_input: serde_json::Value =
                                    serde_json::from_str(&tc.input).unwrap_or_default();
                                let cwd = shell_cwd.clone();
                                join_set.spawn(async move {
                                    let result = tokio::task::spawn_blocking(move || {
                                        plugins.execute_tool(
                                            &plugin_name,
                                            &tool_name,
                                            &tool_input,
                                            cwd.as_deref(),
                                            sandboxed,
                                        )
                                    }).await;
                                    (idx, result)
                                });
                            }

                            let mut completed = 0;
                            let mut send_failed = false;
                            loop {
                                let mut crx2 = cancel_rx.clone();
                                tokio::select! {
                                    next = join_set.join_next() => {
                                        match next {
                                            Some(Ok((idx, result))) => {
                                                completed += 1;
                                                let tc = &tool_calls[idx];
                                                let output = result
                                                    .unwrap_or_else(|_| crate::client_plugin::PluginOutput {
                                                        content: "Tool execution panicked".to_string(),
                                                        is_error: true,
                                                        needs_summarization: false,
                                                    });

                                                let result_msg =
                                                    Message::ChatToolResult(ChatToolResult {
                                                        request_id: tc.request_id.clone(),
                                                        thread_id: tc.thread_id.clone(),
                                                        tool_call_id: tc.tool_call_id.clone(),
                                                        content: output.content,
                                                        is_error: output.is_error,
                                                        needs_summarization: output.needs_summarization,
                                                    });

                                                if completed < total {
                                                    // Intermediate result — send and render status inline
                                                    match rpc.call(result_msg).await {
                                                        Ok(Message::ChatToolStatus(cts)) => {
                                                            // Update running header in-place: move cursor up to the tool's line
                                                            let lines_up = total - idx;
                                                            let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
                                                            let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                                                            let param_desc = cts.param_desc.as_deref().unwrap_or("");
                                                            let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Success);
                                                            let header = display::render_tool_header(icon, display_name, param_desc, cols as usize);
                                                            write_stdout(&format!("\x1b[{}A\r\x1b[K{}\x1b[{}B\r", lines_up, header, lines_up));
                                                            // Update scroll_history entry
                                                            let tool_call_id = cts.tool_call_id.clone();
                                                            if let Some(entry) = self.scroll_history.iter_mut().rev().find(|e| {
                                                                matches!(e, ScrollEntry::ToolStatus(prev) if prev.tool_call_id == tool_call_id)
                                                            }) {
                                                                *entry = ScrollEntry::ToolStatus(cts);
                                                            }
                                                        }
                                                        Err(_) => {
                                                            send_failed = true;
                                                            break;
                                                        }
                                                        _ => {} // Ack or other
                                                    }
                                                } else {
                                                    // Last result — switch to streaming for agent loop continuation
                                                    match rpc.call_stream(result_msg).await {
                                                        Ok(new_rx) => {
                                                            rx = new_rx;
                                                            continue 'stream;
                                                        }
                                                        Err(_) => {
                                                            send_failed = true;
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                            Some(Err(_)) => {
                                                // JoinSet task panicked
                                                completed += 1;
                                                if completed >= total { break; }
                                            }
                                            None => break, // All tasks done
                                        }
                                    }
                                    _ = spinner_interval.tick(), if self.tool_section_start.is_some() => {
                                        self.spinner_frame = self.spinner_frame.wrapping_add(1);
                                        self.redraw_tool_section();
                                    }
                                    _ = wait_cancel(&mut crx2) => {
                                        interrupted = true;
                                        break 'stream;
                                    }
                                }
                            }
                            if send_failed {
                                self.mark_running_tools_error();
                                self.tool_section_start = None;
                                self.tool_section_hist_idx = None;
                                write_stdout(&display::render_error(
                                    "Daemon connection lost",
                                ));
                                break 'stream;
                            }

                            // Sync shell_cwd after tools execute — tools like glob/read may
                            // change cwd via picker interaction, and we need the updated cwd
                            // for the next round of tool calls.
                            if let Some(cwd) = super::get_shell_cwd(proxy.child_pid() as u32) {
                                self.shell_cwd = Some(cwd.clone());
                                let mut attrs = std::collections::HashMap::new();
                                attrs.insert("shell_cwd".to_string(), cwd);
                                let msg = Message::SessionUpdate(SessionUpdate {
                                    session_id: session_id.to_string(),
                                    timestamp_ms: crate::timestamp_ms(),
                                    attrs,
                                });
                                let _ = rpc.call(msg).await;
                            }
                        }
                    }
                    Err(_) => {
                        write_stdout(&display::render_error(crate::i18n::t("error.failed_receive_response")));
                    }
                }
            }

            // Stop Ctrl-C listener
            let _ = stop_tx.send(());

            if interrupted {
                if !got_first_output {
                    // No LLM output yet — treat as input cancellation:
                    // erase thinking indicator and user echo line, restore input for editing.
                    self.erase_thinking();
                    // Erase the user echo (may span multiple visual rows)
                    let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
                    let cols = cols as usize;
                    let echo_rows: usize = input.split('\n').map(|line| {
                        // Each editor line has a 2-char prefix: "> " or "  "
                        let dw = 2 + display::display_width(line);
                        if dw == 0 || cols == 0 { 1 } else { dw.div_ceil(cols) }
                    }).sum();
                    write_stdout(&display::erase_lines(echo_rows));
                    // Remove the UserInput entry we just pushed
                    if matches!(self.scroll_history.last(), Some(ScrollEntry::UserInput(_))) {
                        self.scroll_history.pop();
                    }
                    // Restore input for re-editing
                    self.cancelled_input = Some(input.clone());

                    Self::send_interrupt(&req_id, session_id, self.current_thread_id.as_deref().unwrap(), "", rpc);
                } else {
                    self.erase_thinking();
                    self.mark_running_tools_error();
                    self.tool_section_start = None;
                    self.tool_section_hist_idx = None;
                    self.print_line("");
                    let msg = crate::i18n::t("chat.user_interrupted");
                    self.print_line(&format!("{BRIGHT_WHITE}●{RESET} {msg}"));
                    self.push_entry(ScrollEntry::Response(msg.to_string()));

                    Self::send_interrupt(&req_id, session_id, self.current_thread_id.as_deref().unwrap(), trimmed, rpc);
                }
            }
        }

        // Release the thread binding on the daemon so other sessions can use it
        if let Some(ref tid) = self.current_thread_id {
            let msg = Message::ChatEnd(ChatEnd {
                session_id: session_id.to_string(),
                thread_id: tid.clone(),
            });
            let _ = rpc.call(msg).await;
        }
        exit_action
    }

    fn send_interrupt(req_id: &str, session_id: &str, thread_id: &str, query: &str, rpc: &RpcClient) {
        let msg = Message::ChatInterrupt(ChatInterrupt {
            request_id: req_id.to_string(),
            session_id: session_id.to_string(),
            thread_id: thread_id.to_string(),
            query: query.to_string(),
        });
        let rpc = rpc.clone();
        tokio::spawn(async move {
            let _ = rpc.call(msg).await;
        });
    }

    // ── Command handlers ─────────────────────────────────────────────────

    async fn handle_thread_del(&mut self, trimmed: &str, session_id: &str, rpc: &RpcClient) {
        let idx_str = trimmed
            .strip_prefix("/thread del")
            .map(|s| s.trim())
            .unwrap_or("");

        let del_index = if idx_str.is_empty() {
            let rid = Uuid::new_v4().to_string()[..8].to_string();
            let req = Message::Request(Request {
                request_id: rid.clone(),
                session_id: session_id.to_string(),
                query: "__cmd:conversations".to_string(),
                scope: RequestScope::AllSessions,
            });
            match rpc.call(req).await {
                Ok(Message::Response(resp)) if resp.request_id == rid => {
                    if let Some(json) = super::parse_cmd_response(&resp.content) {
                        if let Some(ids) = json.get("thread_ids").and_then(|v| v.as_array()) {
                            self.cached_thread_ids = ids
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect();
                        }
                        let display_text = super::cmd_display_str(&json);
                        if self.cached_thread_ids.is_empty() {
                            return;
                        }
                        let item_strings: Vec<String> = display_text
                            .lines()
                            .filter(|l| l.trim_start().starts_with('['))
                            .map(|l| l.trim_start().to_string())
                            .collect();
                        let items: Vec<&str> = item_strings.iter().map(|s| s.as_str()).collect();
                        if items.is_empty() {
                            return;
                        }
                        match widgets::picker::pick_many("Select conversations to delete:", &items)
                        {
                            Some(mut indices) if !indices.is_empty() => {
                                indices.sort();
                                indices
                                    .iter()
                                    .map(|&i| (i + 1).to_string())
                                    .collect::<Vec<_>>()
                                    .join(",")
                            }
                            _ => return,
                        }
                    } else {
                        return;
                    }
                }
                _ => {
                    write_stdout(&display::render_error(crate::i18n::t("error.failed_list_conversations")));
                    return;
                }
            }
        } else {
            idx_str.to_string()
        };

        // Auto-fetch if cache empty
        if self.cached_thread_ids.is_empty() {
            let rid = Uuid::new_v4().to_string()[..8].to_string();
            let req = Message::Request(Request {
                request_id: rid.clone(),
                session_id: session_id.to_string(),
                query: "__cmd:conversations".to_string(),
                scope: RequestScope::AllSessions,
            });
            if let Ok(Message::Response(resp)) = rpc.call(req).await {
                if resp.request_id == rid {
                    if let Some(json) = super::parse_cmd_response(&resp.content) {
                        if let Some(ids) = json.get("thread_ids").and_then(|v| v.as_array()) {
                            self.cached_thread_ids = ids
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect();
                        }
                    }
                }
            }
        }

        match super::parse_index_expr(&del_index) {
            Some(indices) => {
                let mut valid = true;
                for &i in &indices {
                    if i > self.cached_thread_ids.len() {
                        write_stdout(&display::render_error(&format!(
                            "Index {} out of range ({} conversations)",
                            i,
                            self.cached_thread_ids.len()
                        )));
                        valid = false;
                        break;
                    }
                }
                if valid {
                    let mut deleted = Vec::new();
                    for &i in &indices {
                        let tid = &self.cached_thread_ids[i - 1];
                        if tid.is_empty() {
                            write_stdout(&display::render_error(&format!(
                                "Conversation [{}] already deleted",
                                i
                            )));
                            continue;
                        }
                        let rid = Uuid::new_v4().to_string()[..8].to_string();
                        let req = Message::Request(Request {
                            request_id: rid.clone(),
                            session_id: session_id.to_string(),
                            query: format!("__cmd:conversations del {}", tid),
                            scope: RequestScope::AllSessions,
                        });
                        match rpc.call(req).await {
                            Ok(Message::Response(resp)) if resp.request_id == rid => {
                                if let Some(json) = super::parse_cmd_response(&resp.content) {
                                    if let Some(deleted_id) =
                                        json.get("deleted_thread_id").and_then(|v| v.as_str())
                                    {
                                        if self.current_thread_id.as_deref() == Some(deleted_id) {
                                            self.current_thread_id = None;
                                        }
                                    }
                                    self.cached_thread_ids[i - 1] = String::new();
                                    deleted.push(i);
                                }
                            }
                            _ => {
                                write_stdout(&display::render_error(
                                    &crate::i18n::tf("error.failed_delete_conversation", &[("n", &i.to_string())])
                                ));
                            }
                        }
                    }
                    if !deleted.is_empty() {
                        let nums: Vec<String> = deleted.iter().map(|i| format!("[{}]", i)).collect();
                        let msg = crate::i18n::tf("chat.deleted_conversation", &[("nums", &nums.join(", "))]);
                        write_stdout(&display::render_response(&msg));
                    }
                }
            }
            None => {
                write_stdout(&display::render_error(
                    crate::i18n::t("error.invalid_index_expression"),
                ));
            }
        }
    }

    async fn handle_thread_list(&mut self, session_id: &str, rpc: &RpcClient, limit: Option<usize>) {
        let query = match limit {
            Some(n) => format!("__cmd:conversations {}", n),
            None => "__cmd:conversations".to_string(),
        };
        let request_id = Uuid::new_v4().to_string()[..8].to_string();
        let request = Message::Request(Request {
            request_id: request_id.clone(),
            session_id: session_id.to_string(),
            query,
            scope: RequestScope::AllSessions,
        });
        match rpc.call(request).await {
            Ok(Message::Response(resp)) if resp.request_id == request_id => {
                if let Some(json) = super::parse_cmd_response(&resp.content) {
                    if let Some(ids) = json.get("thread_ids").and_then(|v| v.as_array()) {
                        self.cached_thread_ids = ids
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                    }
                    let display_text = super::cmd_display_str(&json);
                    write_stdout(&display::render_response(&display_text));
                } else {
                    write_stdout(&display::render_response(&resp.content));
                }
            }
            _ => {
                write_stdout(&display::render_error(crate::i18n::t("error.failed_list_conversations")));
            }
        }
    }

    async fn handle_thread_sandbox(&mut self, trimmed: &str, session_id: &str, rpc: &RpcClient) {
        let sub = trimmed
            .strip_prefix("/thread sandbox")
            .map(|s| s.trim())
            .unwrap_or("");

        let desired_off: Option<bool> = match sub {
            "" => None,       // query only
            "on" => Some(false),
            "off" => Some(true),
            _ => {
                write_stdout(&display::render_error(crate::i18n::t("error.usage_thread_sandbox")));
                return;
            }
        };

        // No active thread: buffer the preference for apply-after-create.
        if self.current_thread_id.is_none() {
            match desired_off {
                Some(off) => {
                    self.pending_sandbox_off = Some(off);
                    let state = if off { "off" } else { "on" };
                    write_stdout(&display::render_response(&format!("sandbox: {}", state)));
                }
                None => {
                    let msg = match self.pending_sandbox_off {
                        Some(true) => "no active thread; pending: off".to_string(),
                        Some(false) => "no active thread; pending: on".to_string(),
                        None => "no active thread".to_string(),
                    };
                    write_stdout(&display::render_response(&msg));
                }
            }
            return;
        }

        let tid = self.current_thread_id.as_deref().unwrap();
        let query = match desired_off {
            Some(true) => format!("__cmd:thread sandbox off:{}", tid),
            Some(false) => format!("__cmd:thread sandbox on:{}", tid),
            None => format!("__cmd:thread sandbox:{}", tid),
        };

        let rid = Uuid::new_v4().to_string()[..8].to_string();
        let request = Message::Request(Request {
            request_id: rid.clone(),
            session_id: session_id.to_string(),
            query,
            scope: RequestScope::AllSessions,
        });
        match rpc.call(request).await {
            Ok(Message::Response(resp)) if resp.request_id == rid => {
                let display_text = if let Some(json) = super::parse_cmd_response(&resp.content) {
                    super::cmd_display_str(&json)
                } else {
                    resp.content
                };
                write_stdout(&display::render_response(&display_text));
            }
            _ => {
                write_stdout(&display::render_error(crate::i18n::t("error.failed_update_sandbox")));
            }
        }
    }

    async fn handle_resume(&mut self, trimmed: &str, session_id: &str, rpc: &RpcClient) -> bool {
        // Resolve which thread_id to resume, then delegate to handle_resume_tid
        let tid: Option<String> = if let Some(idx_str) = trimmed.strip_prefix("/resume ") {
            // Auto-fetch if cache empty
            if self.cached_thread_ids.is_empty() {
                self.fetch_thread_ids(session_id, rpc).await;
            }
            match idx_str.trim().parse::<usize>() {
                Ok(i) if i >= 1 && i <= self.cached_thread_ids.len() => {
                    let t = self.cached_thread_ids[i - 1].clone();
                    if t.is_empty() {
                        write_stdout(&display::render_error(&format!(
                            "Conversation [{}] was deleted", i
                        )));
                        None
                    } else {
                        Some(t)
                    }
                }
                Ok(i) if i >= 1 => {
                    if self.cached_thread_ids.is_empty() {
                        write_stdout(&display::render_error(crate::i18n::t("error.no_conversation_to_resume")));
                    } else {
                        write_stdout(&display::render_error(&format!(
                            "Index {} out of range ({} conversations)",
                            i, self.cached_thread_ids.len()
                        )));
                    }
                    None
                }
                _ => {
                    write_stdout(&display::render_error(crate::i18n::t("error.invalid_index")));
                    None
                }
            }
        } else {
            // /resume without index — picker (with lock-aware disabled items)
            self.show_resume_picker(session_id, rpc).await
        };

        if let Some(tid) = tid {
            self.handle_resume_tid(&tid, session_id, rpc).await
        } else {
            false
        }
    }

    /// Fetch and cache thread IDs from the daemon.
    async fn fetch_thread_ids(&mut self, session_id: &str, rpc: &RpcClient) {
        let rid = Uuid::new_v4().to_string()[..8].to_string();
        let req = Message::Request(Request {
            request_id: rid.clone(),
            session_id: session_id.to_string(),
            query: "__cmd:conversations".to_string(),
            scope: RequestScope::AllSessions,
        });
        if let Ok(Message::Response(resp)) = rpc.call(req).await {
            if resp.request_id == rid {
                if let Some(json) = super::parse_cmd_response(&resp.content) {
                    if let Some(ids) = json.get("thread_ids").and_then(|v| v.as_array()) {
                        self.cached_thread_ids = ids
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                    }
                }
            }
        }
    }

    /// Show the resume picker with lock-aware disabled items.
    /// Returns the selected thread_id, or None on ESC/cancel.
    async fn show_resume_picker(&mut self, session_id: &str, rpc: &RpcClient) -> Option<String> {
        self.fetch_thread_ids(session_id, rpc).await;
        if self.cached_thread_ids.is_empty() {
            write_stdout(&display::render_error(crate::i18n::t("error.no_conversations_to_resume")));
            return None;
        }
        let rid = Uuid::new_v4().to_string()[..8].to_string();
        let req = Message::Request(Request {
            request_id: rid.clone(),
            session_id: session_id.to_string(),
            query: "__cmd:conversations".to_string(),
            scope: RequestScope::AllSessions,
        });
        match rpc.call(req).await {
            Ok(Message::Response(resp)) if resp.request_id == rid => {
                if let Some(json) = super::parse_cmd_response(&resp.content) {
                    let display_str = super::cmd_display_str(&json);
                    let item_strings: Vec<String> = display_str
                        .lines()
                        .filter(|l| l.trim_start().starts_with('['))
                        .map(|l| l.trim_start().to_string())
                        .collect();
                    let items: Vec<&str> =
                        item_strings.iter().map(|s| s.as_str()).collect();
                    if items.is_empty() {
                        write_stdout(&display::render_error(crate::i18n::t("error.no_conversations_to_resume")));
                        return None;
                    }
                    // Build disabled flags from locked_threads
                    use widgets::picker::DisabledIcon;
                    let disabled: Vec<Option<DisabledIcon>> = json.get("locked_threads")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().map(|v| {
                            if v.as_bool().unwrap_or(false) { Some(DisabledIcon::Key) } else { None }
                        }).collect())
                        .unwrap_or_else(|| vec![None; items.len()]);
                    match widgets::picker::pick_one_with_disabled("Resume conversation:", &items, &disabled) {
                        Some(idx) if idx < self.cached_thread_ids.len() => {
                            Some(self.cached_thread_ids[idx].clone())
                        }
                        _ => None,
                    }
                } else {
                    write_stdout(&display::render_error(crate::i18n::t("error.no_conversations_to_resume")));
                    None
                }
            }
            _ => {
                write_stdout(&display::render_error(crate::i18n::t("error.failed_list_conversations")));
                None
            }
        }
    }

    /// Apply a ChatReady response: handle errors, set thread, render history.
    fn apply_chat_ready(&mut self, ready: ChatReady) {
        // Error from daemon (thread_locked, not_found, etc.)
        if let Some(ref err_display) = ready.error_display {
            write_stdout(&display::render_error(err_display));
            return;
        }
        if ready.error.is_some() {
            write_stdout(&display::render_error(crate::i18n::t("error.failed_resume")));
            return;
        }
        if ready.thread_id.is_empty() {
            write_stdout(&display::render_error(crate::i18n::t("error.failed_resume")));
            return;
        }

        self.current_thread_id = Some(ready.thread_id);

        if let Some(history) = ready.history {
            // Parse structured history entries (each is a JSON-encoded string)
            let mut all_entries: Vec<ScrollEntry> = Vec::new();
            for entry_str in &history {
                let entry: serde_json::Value = match serde_json::from_str(entry_str) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match entry.get("type").and_then(|t| t.as_str()) {
                    Some("user_input") => {
                        let text = entry["text"].as_str().unwrap_or("");
                        all_entries.push(ScrollEntry::UserInput(text.to_string()));
                    }
                    Some("llm_text") => {
                        let text = entry["text"].as_str().unwrap_or("");
                        all_entries.push(ScrollEntry::LlmText(text.to_string()));
                    }
                    Some("tool_status") => {
                        let cts = ChatToolStatus {
                            request_id: String::new(),
                            thread_id: String::new(),
                            tool_name: entry["tool_name"].as_str().unwrap_or("").to_string(),
                            tool_call_id: entry["tool_call_id"].as_str().map(String::from),
                            status: String::new(),
                            status_icon: Some(match entry["status_icon"].as_str() {
                                Some("error") => StatusIcon::Error,
                                Some("running") => StatusIcon::Running,
                                _ => StatusIcon::Success,
                            }),
                            display_name: entry["display_name"].as_str().map(String::from),
                            param_desc: entry["param_desc"].as_str().map(String::from),
                            result_compact: entry["result_compact"].as_array().map(|a|
                                a.iter().filter_map(|v| v.as_str().map(String::from)).collect()),
                            result_full: entry["result_full"].as_array().map(|a|
                                a.iter().filter_map(|v| v.as_str().map(String::from)).collect()),
                        };
                        all_entries.push(ScrollEntry::ToolStatus(cts));
                    }
                    Some("response") => {
                        let text = entry["text"].as_str().unwrap_or("");
                        all_entries.push(ScrollEntry::Response(text.to_string()));
                    }
                    Some("separator") => {
                        all_entries.push(ScrollEntry::Separator);
                    }
                    _ => {}
                }
            }

            // Find the start of the last exchange (last UserInput)
            let last_exchange_start = all_entries.iter().rposition(|e|
                matches!(e, ScrollEntry::UserInput(_))
            ).unwrap_or(0);

            // Push ALL entries to scroll_history (for Ctrl+O browse)
            for entry in &all_entries {
                self.push_entry(entry.clone());
            }

            // Render only the last exchange on terminal
            let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
            if last_exchange_start > 0 {
                let earlier_count = all_entries[..last_exchange_start].iter()
                    .filter(|e| matches!(e, ScrollEntry::UserInput(_)))
                    .count();
                if earlier_count > 0 {
                    self.print_line(&format!(
                        "{DIM}({} earlier message{}){RESET}",
                        earlier_count,
                        if earlier_count == 1 { "" } else { "s" }
                    ));
                }
            }
            for entry in &all_entries[last_exchange_start..] {
                match entry {
                    ScrollEntry::UserInput(text) => {
                        for (i, line) in text.lines().enumerate() {
                            if i == 0 {
                                self.print_line(&format!("{CYAN}> {RESET}{}", line));
                            } else {
                                self.print_line(&format!("  {}", line));
                            }
                        }
                    }
                    ScrollEntry::ToolStatus(cts) => {
                        let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                        let param_desc = cts.param_desc.as_deref().unwrap_or("");
                        let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Success);
                        let header = display::render_tool_header(icon, display_name, param_desc, cols as usize);
                        self.print_line(&header);
                        if let Some(ref lines) = cts.result_compact {
                            let rendered = display::render_tool_output_with_cols(lines, cols as usize, self.extended_unicode);
                            for line in &rendered {
                                self.print_line(line);
                            }
                        }
                    }
                    ScrollEntry::LlmText(text) => {
                        self.print_line("");
                        for (i, line) in text.split('\n').enumerate() {
                            if i == 0 {
                                self.print_line(&format!("{BRIGHT_WHITE}●{RESET} {}", line));
                            } else {
                                self.print_line(&format!("  {}", line));
                            }
                        }
                    }
                    ScrollEntry::Response(content) => {
                        self.print_line("");
                        let rendered = markdown::render(content);
                        for (i, line) in rendered.split("\r\n").enumerate() {
                            if i == 0 {
                                self.print_line(&format!("{BRIGHT_WHITE}●{RESET} {}", line));
                            } else {
                                self.print_line(&format!("  {}", line));
                            }
                        }
                    }
                    ScrollEntry::Separator => {
                        self.print_line(&display::render_separator(cols));
                    }
                    ScrollEntry::SystemMessage(msg) => {
                        self.print_line(&format!("{DIM}{}{RESET}", msg));
                    }
                }
            }
        } else {
            write_stdout(&format!("{DIM}(resumed conversation){RESET}\r\n"));
            self.push_entry(ScrollEntry::SystemMessage("(resumed conversation)".to_string()));
        }

        // Store non-default model name for ghost hint
        if let Some(model) = ready.model_name {
            self.resumed_model = Some(model);
        }

        // Warn if sandbox is disabled for this thread
        if ready.sandbox_disabled == Some(true) {
            write_stdout(&format!(
                "{DIM}sandbox: disabled for this thread, tool execution is not sandboxed.{RESET}\r\n"
            ));
        }
    }

    /// Resume a specific thread by ID via ChatStart protocol message.
    /// Returns `true` if the thread was successfully resumed, `false` if cancelled or failed.
    async fn handle_resume_tid(&mut self, tid: &str, session_id: &str, rpc: &RpcClient) -> bool {
        crate::event_log::push(format!("resume_tid: sending ChatStart thread={}", tid));
        let rid = Uuid::new_v4().to_string()[..8].to_string();
        let start_msg = Message::ChatStart(ChatStart {
            request_id: rid.clone(),
            session_id: session_id.to_string(),
            new_thread: false,
            thread_id: Some(tid.to_string()),
        });
        crate::event_log::push("resume_tid: awaiting ChatReady (timeout 15s)");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            rpc.call(start_msg),
        ).await;
        match result {
            Ok(Ok(Message::ChatReady(ready))) if ready.request_id == rid => {
                crate::event_log::push(format!("resume_tid: got ChatReady error={:?}", ready.error));

                // If thread is locked, show picker to let user choose another thread
                if ready.error.as_deref() == Some("thread_locked") {
                    crate::event_log::push("resume_tid: thread locked, showing picker");
                    if let Some(alt_tid) = self.show_resume_picker(session_id, rpc).await {
                        // Resume the selected thread (locked items are disabled in picker,
                        // so this should not hit thread_locked again)
                        let rid2 = Uuid::new_v4().to_string()[..8].to_string();
                        let start2 = Message::ChatStart(ChatStart {
                            request_id: rid2.clone(),
                            session_id: session_id.to_string(),
                            new_thread: false,
                            thread_id: Some(alt_tid),
                        });
                        if let Ok(Ok(Message::ChatReady(r2))) = tokio::time::timeout(
                            std::time::Duration::from_secs(15),
                            rpc.call(start2),
                        ).await {
                            if r2.request_id == rid2 {
                                self.apply_chat_ready(r2);
                                return true;
                            }
                        }
                        write_stdout(&display::render_error(crate::i18n::t("error.failed_resume")));
                    }
                    return false;
                }

                // Render history first, then check mismatch
                self.apply_chat_ready(ready.clone());
                crate::event_log::push("resume_tid: apply_chat_ready done");

                if ready.error.is_none() && !ready.thread_id.is_empty() {
                    if let Some(action) = self.check_resume_mismatch(&ready) {
                        match action {
                            ResumeMismatchAction::Cancel => {
                                crate::event_log::push("resume_tid: user cancelled due to cwd/host mismatch");
                                write_stdout(&format!("{DIM}(User canceled){RESET}\r\n"));
                                // Release the thread claim
                                let end_msg = Message::ChatEnd(ChatEnd {
                                    session_id: session_id.to_string(),
                                    thread_id: ready.thread_id.clone(),
                                });
                                let _ = rpc.send(end_msg).await;
                                return false;
                            }
                            ResumeMismatchAction::CdToOld(old_cwd) => {
                                // Update shell_cwd so daemon uses correct cwd for bash tools
                                self.shell_cwd = Some(old_cwd.clone());
                                let mut attrs = std::collections::HashMap::new();
                                attrs.insert("shell_cwd".to_string(), old_cwd.clone());
                                let msg = Message::SessionUpdate(SessionUpdate {
                                    session_id: session_id.to_string(),
                                    timestamp_ms: crate::timestamp_ms(),
                                    attrs,
                                });
                                let _ = rpc.send(msg).await;
                                self.pending_cd = Some(old_cwd.clone());
                                write_stdout(&format!("{DIM}cwd changed: {}{RESET}\r\n", old_cwd));
                            }
                            ResumeMismatchAction::StayHere(_old_cwd) => {}
                            ResumeMismatchAction::ContinueDifferentHost => {}
                        }
                    }
                }
                crate::event_log::push("resume_tid: done");
                return true;
            }
            Ok(Ok(msg)) => {
                crate::event_log::push(format!("resume_tid: unexpected response {:?}", std::mem::discriminant(&msg)));
                write_stdout(&display::render_error(crate::i18n::t("error.failed_resume")));
            }
            Ok(Err(e)) => {
                crate::event_log::push(format!("resume_tid: RPC error: {}", e));
                write_stdout(&display::render_error(crate::i18n::t("error.failed_resume")));
            }
            Err(_) => {
                let connected = rpc.is_connected().await;
                crate::event_log::push(format!("resume_tid: timed out waiting for daemon response (connected={})", connected));
                write_stdout(&display::render_error(crate::i18n::t("error.resume_timed_out")));
            }
        }
        crate::event_log::push("resume_tid: done");
        false
    }

    // ── Resume mismatch check ─────────────────────────────────────────────

    /// Compare thread's previous host/cwd against current environment.
    /// Returns None if no mismatch, or Some(action) after prompting the user.
    fn check_resume_mismatch(&self, ready: &ChatReady) -> Option<ResumeMismatchAction> {
        let cur_host = nix::unistd::gethostname()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_default();
        let cur_cwd = self.shell_cwd.clone().unwrap_or_default();

        let thread_host = ready.thread_host.as_deref().unwrap_or("");
        let thread_cwd = ready.thread_cwd.as_deref().unwrap_or("");

        // No previous data — nothing to compare
        if thread_host.is_empty() && thread_cwd.is_empty() {
            return None;
        }

        let same_host = thread_host.is_empty() || thread_host == cur_host;
        let same_cwd = thread_cwd.is_empty() || thread_cwd == cur_cwd;

        if same_host && same_cwd {
            return None;
        }

        if !same_host {
            // Different machine
            let title = format!(
                "This conversation was on {CYAN}{}{RESET} (current: {CYAN}{}{RESET}). Proceed?",
                thread_host, cur_host,
            );
            let items = &["[Y]es", "[C]ancel"];
            match widgets::picker::pick_one(&title, items) {
                Some(0) => Some(ResumeMismatchAction::ContinueDifferentHost),
                _ => Some(ResumeMismatchAction::Cancel),
            }
        } else {
            // Same machine, different cwd
            let title = format!(
                "Switch to {CYAN}{}{RESET} (last conversation path)?",
                thread_cwd,
            );
            let items = &["[Y]es", "[N]o, stay here", "[C]ancel"];
            match widgets::picker::pick_one(&title, items) {
                Some(0) => Some(ResumeMismatchAction::CdToOld(thread_cwd.to_string())),
                Some(1) => Some(ResumeMismatchAction::StayHere(thread_cwd.to_string())),
                _ => Some(ResumeMismatchAction::Cancel),
            }
        }
    }

    // ── Model picker ─────────────────────────────────────────────────────

    async fn handle_model(&mut self, session_id: &str, rpc: &RpcClient) {
        // Build query with thread_id if available
        let query = match &self.current_thread_id {
            Some(tid) => format!("__cmd:models {}", tid),
            None => "__cmd:models".to_string(),
        };

        let rid = Uuid::new_v4().to_string()[..8].to_string();
        let req = Message::Request(Request {
            request_id: rid.clone(),
            session_id: session_id.to_string(),
            query,
            scope: RequestScope::AllSessions,
        });

        let models = match rpc.call(req).await {
            Ok(Message::Response(resp)) if resp.request_id == rid => {
                match super::parse_cmd_response(&resp.content) {
                    Some(json) => json.get("models").and_then(|v| v.as_array()).cloned(),
                    None => None,
                }
            }
            _ => None,
        };

        let models = match models {
            Some(m) if !m.is_empty() => m,
            _ => {
                write_stdout(&display::render_error(crate::i18n::t("error.no_llm_backends")));
                return;
            }
        };

        // Build picker items and find selected index
        let mut selected_idx = 0;
        let item_strings: Vec<String> = models.iter().enumerate().map(|(i, m)| {
            let name = m["name"].as_str().unwrap_or("?");
            let model = m["model"].as_str().unwrap_or("?");
            let short_model = strip_date_suffix(model);
            if m["selected"].as_bool().unwrap_or(false) {
                selected_idx = i;
            }
            format!("{} ({})", name, short_model)
        }).collect();
        let items: Vec<&str> = item_strings.iter().map(|s| s.as_str()).collect();

        match widgets::picker::pick_one_at("Select model:", &items, selected_idx) {
            Some(idx) if idx < models.len() => {
                let name = models[idx]["name"].as_str().unwrap_or("").to_string();
                let display_name = &item_strings[idx];

                if let Some(ref tid) = self.current_thread_id {
                    // Existing thread — send model-only ChatMessage
                    let rid = Uuid::new_v4().to_string()[..8].to_string();
                    let msg = Message::ChatMessage(omnish_protocol::message::ChatMessage {
                        request_id: rid.clone(),
                        session_id: session_id.to_string(),
                        thread_id: tid.clone(),
                        query: String::new(),
                        model: Some(name),
                    });
                    match rpc.call(msg).await {
                        Ok(Message::Ack) => {
                            write_stdout(&format!("{DIM}Switched to {}{RESET}\r\n", display_name));
                        }
                        _ => {
                            write_stdout(&display::render_error(crate::i18n::t("error.failed_switch_model")));
                        }
                    }
                } else {
                    // New thread — defer model selection to first message
                    self.pending_model = Some(name);
                    write_stdout(&format!("{DIM}Switched to {}{RESET}\r\n", display_name));
                }
            }
            _ => {} // ESC or no selection — do nothing
        }
    }

    // ── Test helpers (hidden from /help) ────────────────────────────────

    fn handle_test_picker(&self, selected_idx: usize) {
        let items: Vec<String> = (1..=20)
            .map(|i| format!("test-backend-{} (test-model-{})", i, i))
            .collect();
        let refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
        let idx = selected_idx.min(items.len().saturating_sub(1));
        let result = widgets::picker::pick_one_at("Select model:", &refs, idx);
        let msg = match result {
            Some(idx) => crate::i18n::tf("chat.selected", &[("item", &items[idx])]),
            None => crate::i18n::t("chat.cancelled").to_string(),
        };
        write_stdout(&format!("{DIM}{}{RESET}\r\n", msg));
    }

    async fn handle_test_disconnect(&self, arg: &str, rpc: &RpcClient) {
        let parts: Vec<&str> = arg.split_whitespace().collect();
        // parts[0] = "disconnect", parts[1] = N1, parts[2] = N2 (optional)
        let delay_secs: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let reconnect_delay: Option<u64> = parts.get(2).and_then(|s| s.parse().ok());

        let msg = Message::TestDisconnect { delay_secs };
        match rpc.call(msg).await {
            Ok(Message::Ack) => {
                write_stdout(&format!(
                    "{DIM}Daemon will disconnect in {}s{RESET}\r\n",
                    delay_secs
                ));
                if let Some(n2) = reconnect_delay {
                    write_stdout(&format!(
                        "{DIM}Client will delay reconnect by {}s{RESET}\r\n",
                        n2
                    ));
                    // Schedule reconnect suppression
                    let delay = std::time::Duration::from_secs(delay_secs + n2);
                    rpc.suppress_reconnect(delay);
                }
            }
            Ok(_) => {
                write_stdout(&format!("{DIM}Unexpected response from daemon{RESET}\r\n"));
            }
            Err(e) => {
                write_stdout(&format!(
                    "{DIM}Failed to send disconnect request: {}{RESET}\r\n", e
                ));
            }
        }
    }

    fn handle_test_menu(&self) {
        use std::cell::RefCell;
        use widgets::menu::{MenuChange, MenuItem, MenuResult};

        let make_add_item = || {
            use crate::i18n::t;
            let name = t("config.name");
            let btype = t("config.backend_type");
            let model = t("config.model");
            let base_url = t("config.base_url");
            let ctx_win = t("config.context_window");
            MenuItem::Submenu {
            label: t("config.add_backend").to_string(),
            children: vec![
                MenuItem::Select {
                    label: t("config.provider").to_string(),
                    options: vec![
                        "custom".to_string(),
                        "anthropic".to_string(),
                        "openai".to_string(),
                        "openrouter".to_string(),
                        "deepseek".to_string(),
                    ],
                    selected: 0,
                    prefills: vec![
                        ("anthropic".to_string(), vec![
                            (name.to_string(), "anthropic".to_string()),
                            (btype.to_string(), "anthropic".to_string()),
                            (model.to_string(), "claude-sonnet-4-5-20250929".to_string()),
                            (base_url.to_string(), "".to_string()),
                            (ctx_win.to_string(), "200000".to_string()),
                        ]),
                        ("openai".to_string(), vec![
                            (name.to_string(), "openai".to_string()),
                            (btype.to_string(), "openai-compat".to_string()),
                            (model.to_string(), "gpt-4o".to_string()),
                            (base_url.to_string(), "https://api.openai.com/v1".to_string()),
                            (ctx_win.to_string(), "128000".to_string()),
                        ]),
                        ("openrouter".to_string(), vec![
                            (name.to_string(), "openrouter".to_string()),
                            (btype.to_string(), "openai-compat".to_string()),
                            (model.to_string(), "".to_string()),
                            (base_url.to_string(), "https://openrouter.ai/api/v1".to_string()),
                            (ctx_win.to_string(), "200000".to_string()),
                        ]),
                        ("deepseek".to_string(), vec![
                            (name.to_string(), "deepseek".to_string()),
                            (btype.to_string(), "anthropic".to_string()),
                            (model.to_string(), "deepseek-chat".to_string()),
                            (base_url.to_string(), "https://api.deepseek.com/anthropic".to_string()),
                            (ctx_win.to_string(), "131072".to_string()),
                        ]),
                    ],
                },
                MenuItem::Label {
                    label: "──────────────────────────────".to_string(),
                },
                MenuItem::TextInput {
                    label: name.to_string(),
                    value: String::new(),
                },
                MenuItem::Select {
                    label: btype.to_string(),
                    options: vec!["anthropic".to_string(), "openai-compat".to_string()],
                    selected: 0,
                    prefills: vec![],
                },
                MenuItem::TextInput {
                    label: model.to_string(),
                    value: String::new(),
                },
                MenuItem::TextInput {
                    label: t("config.api_key").to_string(),
                    value: String::new(),
                },
                MenuItem::TextInput {
                    label: base_url.to_string(),
                    value: String::new(),
                },
                MenuItem::Toggle {
                    label: t("config.use_proxy").to_string(),
                    value: false,
                },
                MenuItem::TextInput {
                    label: ctx_win.to_string(),
                    value: String::new(),
                },
            ],
            handler: Some("add_backend".to_string()),
            form_mode: true,
        }};

        let mut items = vec![
            MenuItem::Label {
                label: "Test menu — labels are non-interactive".to_string(),
            },
            MenuItem::Submenu {
                label: "LLM".to_string(),
                children: vec![
                    MenuItem::Label {
                        label: "Configure LLM backend settings".to_string(),
                    },
                    MenuItem::Select {
                        label: "Default backend".to_string(),
                        options: vec![
                            "claude".to_string(),
                            "openai".to_string(),
                            "local".to_string(),
                        ],
                        selected: 0,
                        prefills: vec![],
                    },
                    MenuItem::Toggle {
                        label: "Streaming".to_string(),
                        value: true,
                    },
                    MenuItem::TextInput {
                        label: "API key".to_string(),
                        value: "sk-***".to_string(),
                    },
                    MenuItem::TextInput {
                        label: "Proxy URL".to_string(),
                        value: String::new(),
                    },
                ],
                handler: None,
                form_mode: false,
            },
            MenuItem::Submenu {
                label: "Shell".to_string(),
                children: vec![
                    MenuItem::Toggle {
                        label: "Developer mode".to_string(),
                        value: false,
                    },
                    MenuItem::Toggle {
                        label: "Completion enabled".to_string(),
                        value: true,
                    },
                    MenuItem::Select {
                        label: "Theme".to_string(),
                        options: vec![
                            "default".to_string(),
                            "minimal".to_string(),
                            "compact".to_string(),
                        ],
                        selected: 0,
                        prefills: vec![],
                    },
                ],
                handler: None,
                form_mode: false,
            },
            MenuItem::Toggle {
                label: "Telemetry".to_string(),
                value: false,
            },
            MenuItem::Label {
                label: "── Info label 1 ──".to_string(),
            },
            MenuItem::Label {
                label: "── Info label 2 ──".to_string(),
            },
            MenuItem::TextInput {
                label: "Username".to_string(),
                value: "user".to_string(),
            },
            make_add_item(),
            MenuItem::Submenu {
                label: "Save failure test".to_string(),
                children: vec![
                    MenuItem::Toggle {
                        label: "Toggle option".to_string(),
                        value: false,
                    },
                    MenuItem::Select {
                        label: "Select option".to_string(),
                        options: vec!["A".to_string(), "B".to_string(), "C".to_string()],
                        selected: 0,
                        prefills: vec![],
                    },
                    MenuItem::TextInput {
                        label: "Text option".to_string(),
                        value: "original".to_string(),
                    },
                ],
                handler: None,
                form_mode: false,
            },
        ];

        // Shadow copy tracks current items for the handler callback
        let items_shadow = RefCell::new(items.clone());

        let mut handler_callback = |_handler_name: &str, changes: Vec<MenuChange>| -> Option<Vec<MenuItem>> {
            let provider = changes.iter()
                .find(|c| c.path.ends_with(".Provider"))
                .map(|c| c.value.clone())
                .unwrap_or_default();
            if provider.is_empty() {
                return None;
            }
            let model = changes.iter()
                .find(|c| c.path.ends_with(".Model"))
                .map(|c| c.value.clone())
                .unwrap_or_default();
            let api_key = changes.iter()
                .find(|c| c.path.ends_with(".API key"))
                .map(|c| c.value.clone())
                .unwrap_or_default();

            let label = if model.is_empty() {
                provider.clone()
            } else {
                format!("{} ({})", provider, model)
            };
            let use_proxy = changes.iter()
                .find(|c| c.path.ends_with(".Use proxy"))
                .and_then(|c| c.value.parse::<bool>().ok())
                .unwrap_or(false);

            let new_item = MenuItem::Submenu {
                label,
                children: vec![
                    MenuItem::TextInput {
                        label: "Model".to_string(),
                        value: model,
                    },
                    MenuItem::TextInput {
                        label: "API key".to_string(),
                        value: if api_key.is_empty() { String::new() } else {
                            if api_key.len() > 8 {
                                format!("{}...{}", &api_key[..4], &api_key[api_key.len()-4..])
                            } else {
                                "****".to_string()
                            }
                        },
                    },
                    MenuItem::Toggle {
                        label: "Use proxy".to_string(),
                        value: use_proxy,
                    },
                ],
                handler: None,
                form_mode: false,
            };

            let mut current = items_shadow.borrow().clone();
            // Insert new backend before "Add backend" (last element)
            current.insert(current.len() - 1, new_item);
            // Reset the "Add backend" form
            if let Some(MenuItem::Submenu { children, .. }) = current.last_mut() {
                for child in children.iter_mut() {
                    match child {
                        MenuItem::TextInput { value, .. } => *value = String::new(),
                        MenuItem::Select { selected, .. } => *selected = 0,
                        _ => {}
                    }
                }
            }
            *items_shadow.borrow_mut() = current.clone();
            Some(current)
        };

        // Items under "Save failure test" submenu always fail to test revert behavior
        let mut change_callback = |change: &MenuChange| -> bool {
            if change.path.starts_with("Save failure test.") {
                write_stdout(&format!(
                    "{RED}Simulated save failure: {} = {}{RESET}\r\n",
                    change.path, change.value
                ));
                false
            } else {
                true
            }
        };

        let result = widgets::menu::run_menu("Config", &mut items, Some(&mut handler_callback), Some(&mut change_callback));
        match result {
            MenuResult::Done(changes) => {
                // With on_change, only form-mode (handler submenu) changes remain here
                if changes.is_empty() {
                    write_stdout(&format!("{DIM}No batch changes.{RESET}\r\n"));
                } else {
                    write_stdout(&format!(
                        "{DIM}Batch changes ({}):{RESET}\r\n",
                        changes.len()
                    ));
                    for c in &changes {
                        write_stdout(&format!(
                            "{DIM}  {} = {}{RESET}\r\n",
                            c.path, c.value
                        ));
                    }
                }
            }
            MenuResult::Cancelled => {
                write_stdout(&format!("{DIM}Cancelled.{RESET}\r\n"));
            }
        }
    }

    async fn handle_config(&mut self, _session_id: &str, rpc: &RpcClient) {
        let (items, handlers) = match rpc.call(Message::ConfigQuery).await {
            Ok(Message::ConfigResponse { items, handlers }) => (items, handlers),
            Ok(_) => {
                write_stdout(&format!("{RED}Unexpected response from daemon{RESET}\r\n"));
                return;
            }
            Err(e) => {
                write_stdout(&format!("{RED}Failed to query config: {}{RESET}\r\n", e));
                return;
            }
        };

        // Expand client-side placeholders (label = "_client:<key>")
        let sandbox_snapshot = self.sandbox_state.read().unwrap().clone();
        let mut local_paths = std::collections::HashSet::<String>::new();
        let mut extra_handlers: Vec<ConfigHandlerInfo> = Vec::new();
        let (items, global_rules, tool_params) = extract_global_rules(items);
        let items = expand_client_placeholders(
            items, &sandbox_snapshot, &mut local_paths, &mut extra_handlers, &global_rules, &tool_params
        );
        let mut all_handlers = handlers;
        all_handlers.extend(extra_handlers);

        if items.is_empty() {
            write_stdout(&format!("{DIM}No configurable items.{RESET}\r\n"));
            return;
        }

        // Snapshot initial state for diff computation on exit
        let initial_items = items.clone();

        let (mut menu_items, path_map_initial) = build_menu_tree(&items, &all_handlers);
        let path_map = RefCell::new(path_map_initial);

        let rpc_ref = rpc;
        let path_map_ref = &path_map;
        let local_paths_ref = &local_paths;
        let sandbox_state_ref = &self.sandbox_state;

        let result = tokio::task::block_in_place(|| {
            let rt = tokio::runtime::Handle::current();

            let mut handler_callback = |handler_name: &str, handler_changes: Vec<widgets::menu::MenuChange>| -> Option<Vec<widgets::menu::MenuItem>> {
                // MenuChange paths come from build_path (display labels joined by '.'),
                // e.g. "Sandbox.Rules.Add permit rule.Plugin". Translate to schema paths
                // (e.g. "sandbox.rules._add.plugin") via path_map so downstream logic
                // can match on the stable schema field suffix.
                let translated_changes: Vec<widgets::menu::MenuChange> = {
                    let pm = path_map_ref.borrow();
                    handler_changes.iter()
                        .map(|mc| widgets::menu::MenuChange {
                            path: pm.get(&mc.path).cloned().unwrap_or_else(|| mc.path.clone()),
                            value: mc.value.clone(),
                        })
                        .collect()
                };

                // Local rule operations: handle client-side, then rebuild menu
                if handler_name == "add_rule" {
                    // Unified add: dispatch based on scope selector
                    let scope = find_change_value(&translated_changes, ".scope");
                    if scope == "global" {
                        // Forward to daemon: rewrite paths to __add__ prefix
                        let config_changes: Vec<ConfigChange> = translated_changes.iter()
                            .filter_map(|mc| {
                                if mc.path.ends_with(".scope") { return None; }
                                // Extract field name (last segment) and remap to daemon prefix
                                let field = mc.path.rsplit('.').next().unwrap_or(&mc.path);
                                Some(ConfigChange {
                                    path: format!("sandbox.rules.__add__.{}", field),
                                    value: mc.value.clone(),
                                })
                            })
                            .collect();
                        if let Err(msg) = send_config_update(&rt, rpc_ref, config_changes) {
                            write_stdout(&format!("{RED}{}{RESET}\r\n", msg));
                            return None;
                        }
                    } else {
                        // Local scope (default)
                        if !add_local_rule(&translated_changes, sandbox_state_ref) {
                            return None;
                        }
                    }
                } else if handler_name == "add_local_rule" {
                    let ok = add_local_rule(&translated_changes, sandbox_state_ref);
                    if !ok { return None; }
                } else if let Some(rest) = handler_name.strip_prefix("edit_global_rule:") {
                    // rest = "plugin:idx" — convert to "plugin.idx" for daemon path
                    let rest_dotted = rest.replace(':', ".");
                    let config_changes: Vec<ConfigChange> = translated_changes.iter()
                        .filter_map(|mc| {
                            let field = mc.path.rsplit('.').next().unwrap_or(&mc.path);
                            if field == "_scope" { return None; } // scope label, not a real field
                            Some(ConfigChange {
                                path: format!("sandbox.rules.__edit__.{}.{}", rest_dotted, field),
                                value: mc.value.clone(),
                            })
                        })
                        .collect();
                    if let Err(msg) = send_config_update(&rt, rpc_ref, config_changes) {
                        write_stdout(&format!("{RED}{}{RESET}\r\n", msg));
                        return None;
                    }
                } else if let Some(rest) = handler_name.strip_prefix("edit_local_rule:") {
                    let parsed = rest.rfind(':').and_then(|colon| {
                        let plugin = &rest[..colon];
                        rest[colon+1..].parse::<usize>().ok().map(|idx| (plugin, idx))
                    });
                    match parsed {
                        Some((plugin, idx)) => {
                            if !edit_local_rule(plugin, idx, &translated_changes, sandbox_state_ref) {
                                return None;
                            }
                        }
                        None => {
                            write_stdout(&format!("{RED}Invalid handler: {}{RESET}\r\n", handler_name));
                            return None;
                        }
                    }
                } else {
                    // Global/daemon operation
                    let pm = path_map_ref.borrow();
                    let config_changes: Vec<ConfigChange> = handler_changes.iter()
                        .filter_map(|mc| {
                            let schema_path = match pm.get(&mc.path) {
                                Some(p) => p.clone(),
                                // Skip changes with no schema mapping (e.g. auto-appended Done button)
                                None => return None,
                            };
                            if local_paths_ref.contains(&schema_path) { return None; }
                            Some(ConfigChange { path: schema_path, value: mc.value.clone() })
                        })
                        .collect();
                    drop(pm);

                    let update_result = rt.block_on(async {
                        rpc_ref.call(Message::ConfigUpdate { changes: config_changes }).await
                    });
                    match update_result {
                        Ok(Message::ConfigUpdateResult { ok: false, error }) => {
                            write_stdout(&format!("{RED}Handler error: {}{RESET}\r\n",
                                error.unwrap_or_default()));
                            return None;
                        }
                        Err(e) => {
                            write_stdout(&format!("{RED}RPC error: {}{RESET}\r\n", e));
                            return None;
                        }
                        _ => {}
                    }
                }

                // Re-query and rebuild menu after any handler operation
                let query_result = rt.block_on(async {
                    rpc_ref.call(Message::ConfigQuery).await
                });
                match query_result {
                    Ok(Message::ConfigResponse { items, handlers: new_handlers }) => {
                        let sandbox = sandbox_state_ref.read().unwrap().clone();
                        let mut lp = std::collections::HashSet::new();
                        let mut eh: Vec<ConfigHandlerInfo> = Vec::new();
                        let (items, gr, tp) = extract_global_rules(items);
                        let expanded = expand_client_placeholders(items, &sandbox, &mut lp, &mut eh, &gr, &tp);
                        let mut merged = new_handlers;
                        merged.extend(eh);
                        let (new_tree, new_map) = build_menu_tree(&expanded, &merged);
                        *path_map_ref.borrow_mut() = new_map;
                        Some(new_tree)
                    }
                    _ => None,
                }
            };

            let mut change_callback = |change: &widgets::menu::MenuChange| -> bool {
                let pm = path_map_ref.borrow();
                let schema_path = pm.get(&change.path)
                    .cloned()
                    .unwrap_or_else(|| change.path.clone());
                drop(pm);

                // Client-local paths: save to client.toml, skip RPC
                if local_paths_ref.contains(&schema_path) {
                    return save_local_sandbox_config(&schema_path, &change.value, sandbox_state_ref);
                }

                // Map display names back to stored values
                let value = if schema_path == "general.language" {
                    lang_display_to_code(&change.value).to_string()
                } else {
                    change.value.clone()
                };
                let config_changes = vec![ConfigChange { path: schema_path, value }];
                let update_result = rt.block_on(async {
                    rpc_ref.call(Message::ConfigUpdate { changes: config_changes }).await
                });
                match update_result {
                    Ok(Message::ConfigUpdateResult { ok: false, error }) => {
                        write_stdout(&format!("{RED}Failed to save: {}{RESET}\r\n",
                            error.unwrap_or_default()));
                        false
                    }
                    Err(e) => {
                        write_stdout(&format!("{RED}RPC error: {}{RESET}\r\n", e));
                        false
                    }
                    _ => true,
                }
            };

            widgets::menu::run_menu("Config", &mut menu_items, Some(&mut handler_callback), Some(&mut change_callback))
        });

        // Show diff of changes on exit
        match result {
            widgets::menu::MenuResult::Done(_) | widgets::menu::MenuResult::Cancelled => {
                if let Ok(Message::ConfigResponse { items: final_raw, .. }) = rpc.call(Message::ConfigQuery).await {
                    let sandbox = self.sandbox_state.read().unwrap().clone();
                    let mut lp = std::collections::HashSet::new();
                    let mut eh = Vec::new();
                    let (final_raw, gr, tp) = extract_global_rules(final_raw);
                    let final_items = expand_client_placeholders(final_raw, &sandbox, &mut lp, &mut eh, &gr, &tp);
                    let diff = compute_config_diff(&initial_items, &final_items);
                    if !diff.is_empty() {
                        display_config_diff(&diff);
                    }
                }
            }
        }
    }

    fn handle_test_multi_level_picker(&self) {
        // Level 1: category selection
        let categories = &["Fruits", "Vegetables", "Drinks"];
        let cat_idx = match widgets::picker::pick_one("Select category:", categories) {
            Some(idx) => idx,
            None => {
                write_stdout(&format!("{DIM}Cancelled at level 1{RESET}\r\n"));
                return;
            }
        };
        write_stdout(&format!(
            "{DIM}Category: {}{RESET}\r\n",
            categories[cat_idx]
        ));

        // Level 2: item selection within category
        let items: &[&[&str]] = &[
            &["Apple", "Banana", "Cherry", "Durian"],
            &["Carrot", "Broccoli", "Spinach"],
            &["Water", "Coffee", "Tea", "Juice", "Milk"],
        ];
        let title = format!("Select {} item:", categories[cat_idx].to_lowercase());
        let item_idx = match widgets::picker::pick_one(&title, items[cat_idx]) {
            Some(idx) => idx,
            None => {
                write_stdout(&format!("{DIM}Cancelled at level 2{RESET}\r\n"));
                return;
            }
        };
        let selected = items[cat_idx][item_idx];
        write_stdout(&format!(
            "{DIM}Item: {}{RESET}\r\n",
            selected
        ));

        // Level 3: action selection
        let actions = &["[A]dd to cart", "[V]iew details", "[C]ancel"];
        let action_idx = match widgets::picker::pick_one("Action:", actions) {
            Some(idx) => idx,
            None => {
                write_stdout(&format!("{DIM}Cancelled at level 3{RESET}\r\n"));
                return;
            }
        };

        let result = format!(
            "Result: {} > {} > {}",
            categories[cat_idx], selected, actions[action_idx]
        );
        write_stdout(&format!("{DIM}{}{RESET}\r\n", result));
    }

    // ── Input handling ───────────────────────────────────────────────────

    fn read_input_with(&mut self, allow_backspace_exit: bool, initial: Option<&str>) -> Option<String> {
        use unicode_width::UnicodeWidthChar;
        use widgets::line_editor::LineEditor;

        let stdin_fd = std::io::stdin().as_raw_fd();
        let mut editor = LineEditor::new();
        if let Some(text) = initial {
            editor.set_content(text);
        }
        let mut byte = [0u8; 1];
        let mut has_ghost = false;
        let mut ghost_text = String::new();
        let mut bracketed_paste = false;
        let mut last_input = std::time::Instant::now();

        struct PasteBlock {
            content: String,
            index: usize,
            line_count: usize,
        }
        let paste_blocks: std::cell::RefCell<Vec<PasteBlock>> = std::cell::RefCell::new(vec![]);
        let mut paste_count = 0usize;
        let mut paste_buf = String::new();
        let mut paste_buffering = false;
        let mut paste_last_cr = false;

        // Enable bracketed paste
        write_stdout("\x1b[?2004h");

        let term_cursor_row = std::cell::Cell::new(0usize);

        // Redraw closure — relative cursor movement, no layout dependency
        let redraw = |editor: &LineEditor, ghost: &str, has_ghost: bool| {
            let blocks = paste_blocks.borrow();
            let line_count = editor.line_count();
            let (cursor_row, cursor_col) = editor.cursor();
            let mut fffc_idx = 0usize;
            let mut out = String::new();

            let prev_row = term_cursor_row.get();
            if prev_row > 0 {
                out.push_str(&format!("\x1b[{}A", prev_row));
            }
            out.push('\r');

            let cols = super::get_terminal_size().unwrap_or((24, 80)).1 as usize;
            let cols = cols.max(1);

            let mut display_widths = Vec::with_capacity(line_count);
            for i in 0..line_count {
                let line = editor.line(i);
                let pfx = if i == 0 { format!("{CYAN}> {RESET}") } else { "  ".to_string() };
                let mut s = String::new();
                s.push_str(&pfx);
                let mut dw = 2usize;

                let has_fffc = line.contains(&'\u{FFFC}');
                if has_fffc {
                    for &ch in line {
                        if ch == '\u{FFFC}' {
                            if let Some(block) = blocks.get(fffc_idx) {
                                let marker = format!(
                                    "[pasted text #{} +{} lines]",
                                    block.index, block.line_count
                                );
                                dw += marker.len();
                                s.push_str(&format!("{DIM}{}{RESET}", marker));
                            }
                            fffc_idx += 1;
                        } else {
                            dw += UnicodeWidthChar::width(ch).unwrap_or(1);
                            s.push(ch);
                        }
                    }
                } else {
                    for &ch in line {
                        dw += UnicodeWidthChar::width(ch).unwrap_or(1);
                    }
                    let line_str: String = line.iter().collect();
                    s.push_str(&line_str);
                }

                let cursor_on_fffc = line.contains(&'\u{FFFC}');
                if i == line_count - 1 && has_ghost && !ghost.is_empty() && !cursor_on_fffc {
                    for ch in ghost.chars() {
                        dw += UnicodeWidthChar::width(ch).unwrap_or(1);
                    }
                    s.push_str(&format!("{DIM}{}{RESET}", ghost));
                }

                if i == line_count - 1 {
                    out.push_str(&s);
                    out.push_str("\x1b[J");
                } else {
                    out.push_str(&s);
                    out.push_str("\x1b[K\r\n");
                }
                display_widths.push(dw);
            }

            // Cursor positioning
            let mut cursor_display = 2usize;
            let cursor_line = editor.line(cursor_row);
            let mut local_fffc = 0usize;
            let fffc_before_cursor_row: usize = (0..cursor_row)
                .map(|r| editor.line(r).iter().filter(|&&c| c == '\u{FFFC}').count())
                .sum();
            for &ch in &cursor_line[..cursor_col] {
                if ch == '\u{FFFC}' {
                    let block_idx = fffc_before_cursor_row + local_fffc;
                    if let Some(block) = blocks.get(block_idx) {
                        cursor_display += format!(
                            "[pasted text #{} +{} lines]",
                            block.index, block.line_count
                        )
                        .len();
                    }
                    local_fffc += 1;
                } else {
                    cursor_display += UnicodeWidthChar::width(ch).unwrap_or(1);
                }
            }

            let cursor_after_visual_row: usize = {
                let mut r = 0;
                for w in display_widths.iter().take(line_count.saturating_sub(1)) {
                    r += w / cols + 1;
                }
                r += display_widths[line_count - 1] / cols;
                r
            };
            let target_visual_row: usize = {
                let mut r = 0;
                for w in display_widths.iter().take(cursor_row) {
                    r += w / cols + 1;
                }
                r += cursor_display / cols;
                r
            };
            let target_visual_col = cursor_display % cols;

            let rows_up = cursor_after_visual_row.saturating_sub(target_visual_row);
            if rows_up > 0 {
                out.push_str(&format!("\x1b[{}A", rows_up));
            }
            out.push('\r');
            if target_visual_col > 0 {
                out.push_str(&format!("\x1b[{}C", target_visual_col));
            }

            term_cursor_row.set(target_visual_row);
            write_stdout(&out);
        };

        let disable_paste = || {
            write_stdout("\x1b[?2004l");
        };

        // If initial content was provided, render it immediately.
        if initial.is_some() {
            redraw(&editor, "", false);
        }

        loop {
            // Paste buffer finalization on timeout
            if paste_buffering && !bracketed_paste {
                let mut pfd =
                    libc::pollfd { fd: stdin_fd, events: libc::POLLIN, revents: 0 };
                if unsafe { libc::poll(&mut pfd, 1, 2) } <= 0 {
                    paste_buffering = false;
                    let line_count = paste_buf.lines().count()
                        + if paste_buf.ends_with('\n') { 1 } else { 0 };
                    let line_count = line_count.max(if paste_buf.is_empty() { 0 } else { 1 });
                    if line_count >= 10 {
                        paste_count += 1;
                        paste_blocks.borrow_mut().push(PasteBlock {
                            content: paste_buf.clone(),
                            index: paste_count,
                            line_count,
                        });
                        editor.insert_paste_block();
                    } else if !paste_buf.is_empty() {
                        for ch in paste_buf.chars() {
                            if ch == '\n' {
                                editor.newline();
                            } else {
                                editor.insert(ch);
                            }
                        }
                    }
                    paste_buf.clear();
                    has_ghost = false;
                    ghost_text.clear();
                    self.completer.clear();
                    redraw(&editor, "", false);
                }
            }

            // Idle timeout: exit chat after 30 minutes of no input
            {
                let mut pfd = libc::pollfd { fd: stdin_fd, events: libc::POLLIN, revents: 0 };
                let timeout_ms = 30 * 60 * 1000; // 30 minutes
                let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
                if ready == 0 {
                    // Timeout — auto-exit chat mode
                    write_stdout(&format!("\r\n{DIM}(chat closed due to inactivity){RESET}\r\n"));
                    // Disable bracketed paste before exiting
                    write_stdout("\x1b[?2004l");
                    return None;
                }
            }
            match nix::unistd::read(stdin_fd, &mut byte) {
                Ok(1) => {
                    let now = std::time::Instant::now();
                    let backward = now.duration_since(last_input).as_millis() < 1;
                    last_input = now;
                    let mut pfd =
                        libc::pollfd { fd: stdin_fd, events: libc::POLLIN, revents: 0 };
                    let forward = unsafe { libc::poll(&mut pfd, 1, 0) } > 0;
                    let pasting = bracketed_paste || backward || forward;

                    if pasting && !paste_buffering && byte[0] != 0x1b {
                        paste_buffering = true;
                        paste_buf.clear();
                        paste_last_cr = false;
                    }

                    if !pasting && paste_buffering {
                        paste_buffering = false;
                        let line_count = paste_buf.lines().count()
                            + if paste_buf.ends_with('\n') { 1 } else { 0 };
                        let line_count =
                            line_count.max(if paste_buf.is_empty() { 0 } else { 1 });
                        if line_count >= 10 {
                            paste_count += 1;
                            paste_blocks.borrow_mut().push(PasteBlock {
                                content: paste_buf.clone(),
                                index: paste_count,
                                line_count,
                            });
                            editor.insert_paste_block();
                        } else if !paste_buf.is_empty() {
                            for ch in paste_buf.chars() {
                                if ch == '\n' {
                                    editor.newline();
                                } else {
                                    editor.insert(ch);
                                }
                            }
                        }
                        paste_buf.clear();
                        has_ghost = false;
                        ghost_text.clear();
                        self.completer.clear();
                        redraw(&editor, "", false);
                    }

                    match byte[0] {
                        0x1b => match parse_key_after_esc(stdin_fd) {
                            Some(KeyEvent::Esc) => {
                                disable_paste();
                                return None;
                            }
                            Some(KeyEvent::PasteStart) => {
                                bracketed_paste = true;
                            }
                            Some(KeyEvent::PasteEnd) => {
                                bracketed_paste = false;
                                paste_buffering = false;
                                let line_count = paste_buf.lines().count()
                                    + if paste_buf.ends_with('\n') { 1 } else { 0 };
                                let line_count =
                                    line_count.max(if paste_buf.is_empty() { 0 } else { 1 });
                                if line_count >= 10 {
                                    paste_count += 1;
                                    paste_blocks.borrow_mut().push(PasteBlock {
                                        content: paste_buf.clone(),
                                        index: paste_count,
                                        line_count,
                                    });
                                    editor.insert_paste_block();
                                } else if !paste_buf.is_empty() {
                                    for ch in paste_buf.chars() {
                                        if ch == '\n' {
                                            editor.newline();
                                        } else {
                                            editor.insert(ch);
                                        }
                                    }
                                }
                                paste_buf.clear();
                                has_ghost = false;
                                ghost_text.clear();
                                self.completer.clear();
                                redraw(&editor, "", false);
                            }
                            Some(KeyEvent::ShiftEnter) => {
                                editor.newline();
                                has_ghost = false;
                                ghost_text.clear();
                                self.completer.clear();
                                redraw(&editor, "", false);
                            }
                            Some(KeyEvent::ArrowUp) => {
                                if editor.is_empty() || self.history_index.is_some() {
                                    if self.chat_history.is_empty() {
                                        continue;
                                    }
                                    let idx = match self.history_index {
                                        Some(i) if i > 0 => i - 1,
                                        Some(_) => continue,
                                        None => self.chat_history.len() - 1,
                                    };
                                    self.history_index = Some(idx);
                                    if let Some(cmd) = self.chat_history.get(idx) {
                                        let cmd = cmd.clone();
                                        editor.set_content(&cmd);
                                        has_ghost = false;
                                        ghost_text.clear();
                                        if let Some(g) = self.completer.update(&cmd) {
                                            ghost_text = g.to_string();
                                            has_ghost = true;
                                            redraw(&editor, &ghost_text, true);
                                        } else {
                                            redraw(&editor, "", false);
                                        }
                                    }
                                } else {
                                    editor.move_up();
                                    redraw(&editor, &ghost_text, has_ghost);
                                }
                            }
                            Some(KeyEvent::ArrowDown) => {
                                if editor.is_empty() || self.history_index.is_some() {
                                    if self.chat_history.is_empty() {
                                        continue;
                                    }
                                    let idx = match self.history_index {
                                        Some(i) if i < self.chat_history.len() - 1 => i + 1,
                                        Some(_) => {
                                            self.history_index = None;
                                            editor.set_content("");
                                            has_ghost = false;
                                            ghost_text.clear();
                                            self.completer.clear();
                                            redraw(&editor, "", false);
                                            continue;
                                        }
                                        None => continue,
                                    };
                                    self.history_index = Some(idx);
                                    if let Some(cmd) = self.chat_history.get(idx) {
                                        let cmd = cmd.clone();
                                        editor.set_content(&cmd);
                                        has_ghost = false;
                                        ghost_text.clear();
                                        if let Some(g) = self.completer.update(&cmd) {
                                            ghost_text = g.to_string();
                                            has_ghost = true;
                                            redraw(&editor, &ghost_text, true);
                                        } else {
                                            redraw(&editor, "", false);
                                        }
                                    }
                                } else {
                                    editor.move_down();
                                    redraw(&editor, &ghost_text, has_ghost);
                                }
                            }
                            Some(KeyEvent::ArrowLeft) => {
                                editor.move_left();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            Some(KeyEvent::ArrowRight) => {
                                editor.move_right();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            Some(KeyEvent::Home) => {
                                editor.move_home();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            Some(KeyEvent::End) => {
                                editor.move_end();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            Some(KeyEvent::Delete) => {
                                editor.delete_forward();
                                has_ghost = false;
                                ghost_text.clear();
                                let content = editor.content();
                                if let Some(g) = self.completer.update(&content) {
                                    ghost_text = g.to_string();
                                    has_ghost = true;
                                    redraw(&editor, &ghost_text, true);
                                } else {
                                    self.completer.clear();
                                    redraw(&editor, "", false);
                                }
                            }
                            Some(KeyEvent::CtrlLeft) => {
                                editor.move_word_left();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            Some(KeyEvent::CtrlRight) => {
                                editor.move_word_right();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            None => {}
                        },
                        _ if paste_buffering => {
                            match byte[0] {
                                0x0d => {
                                    paste_buf.push('\n');
                                    paste_last_cr = true;
                                }
                                0x0a => {
                                    if !paste_last_cr {
                                        paste_buf.push('\n');
                                    }
                                    paste_last_cr = false;
                                }
                                b if (0x20..0x80).contains(&b) => {
                                    paste_last_cr = false;
                                    paste_buf.push(b as char);
                                }
                                b if b >= 0x80 => {
                                    paste_last_cr = false;
                                    let mut utf8_buf = vec![b];
                                    let expected =
                                        if b < 0xE0 { 1 } else if b < 0xF0 { 2 } else { 3 };
                                    for _ in 0..expected {
                                        if nix::unistd::read(stdin_fd, &mut byte).unwrap_or(0) == 1
                                        {
                                            utf8_buf.push(byte[0]);
                                        }
                                    }
                                    let ch = String::from_utf8_lossy(&utf8_buf)
                                        .chars()
                                        .next()
                                        .unwrap_or('?');
                                    paste_buf.push(ch);
                                }
                                _ => {
                                    paste_last_cr = false;
                                }
                            }
                            continue;
                        }
                        0x0f => {
                            // Ctrl-O — browse history (alternate screen)
                            self.browse_history();
                        }
                        0x01 => {
                            editor.move_home();
                            redraw(&editor, &ghost_text, has_ghost);
                        }
                        0x05 => {
                            editor.move_end();
                            redraw(&editor, &ghost_text, has_ghost);
                        }
                        0x15 => {
                            editor.kill_to_start();
                            has_ghost = false;
                            ghost_text.clear();
                            let content = editor.content();
                            if let Some(g) = self.completer.update(&content) {
                                ghost_text = g.to_string();
                                has_ghost = true;
                                redraw(&editor, &ghost_text, true);
                            } else {
                                self.completer.clear();
                                redraw(&editor, "", false);
                            }
                        }
                        0x04 if editor.is_empty() && paste_blocks.borrow().is_empty() => {
                            disable_paste();
                            return None;
                        }
                        0x0a => {
                            editor.newline();
                            has_ghost = false;
                            ghost_text.clear();
                            self.completer.clear();
                            redraw(&editor, "", false);
                        }
                        0x0d => {
                            // Enter — submit
                            if has_ghost {
                                redraw(&editor, "", false);
                            }
                            let last_row = editor.line_count() - 1;
                            let (cur_row, _) = editor.cursor();
                            if cur_row < last_row {
                                let down = last_row - cur_row;
                                write_stdout(&format!("\x1b[{}B", down));
                            }
                            let blocks = paste_blocks.borrow();
                            let fffc_before_last: usize = (0..last_row)
                                .map(|r| {
                                    editor
                                        .line(r)
                                        .iter()
                                        .filter(|&&c| c == '\u{FFFC}')
                                        .count()
                                })
                                .sum();
                            let mut end_col = 2usize;
                            let mut local_fi = 0usize;
                            for &ch in editor.line(last_row) {
                                if ch == '\u{FFFC}' {
                                    let bi = fffc_before_last + local_fi;
                                    if let Some(b) = blocks.get(bi) {
                                        end_col += format!(
                                            "[pasted text #{} +{} lines]",
                                            b.index, b.line_count
                                        )
                                        .len();
                                    }
                                    local_fi += 1;
                                } else {
                                    end_col += UnicodeWidthChar::width(ch).unwrap_or(1);
                                }
                            }
                            drop(blocks);
                            write_stdout(&format!("\r\x1b[{}C", end_col));
                            self.completer.clear();
                            disable_paste();
                            // Assemble full content
                            let blocks = paste_blocks.borrow();
                            if blocks.is_empty() {
                                return Some(editor.content());
                            }
                            let mut full = String::new();
                            let mut block_idx = 0usize;
                            let lc = editor.line_count();
                            for i in 0..lc {
                                let line = editor.line(i);
                                let is_fffc_only = line.len() == 1 && line[0] == '\u{FFFC}';
                                if is_fffc_only {
                                    if let Some(block) = blocks.get(block_idx) {
                                        full.push_str(&block.content);
                                        if !block.content.ends_with('\n') {
                                            full.push('\n');
                                        }
                                        block_idx += 1;
                                    }
                                } else {
                                    let line_str: String =
                                        line.iter().filter(|&&c| c != '\u{FFFC}').collect();
                                    full.push_str(&line_str);
                                    if i < lc - 1 {
                                        full.push('\n');
                                    }
                                }
                            }
                            return Some(full);
                        }
                        0x09 => {
                            if let Some(suffix) = self.completer.accept() {
                                for ch in suffix.chars() {
                                    editor.insert(ch);
                                }
                                has_ghost = false;
                                ghost_text.clear();
                                let content = editor.content();
                                if let Some(g) = self.completer.update(&content) {
                                    ghost_text = g.to_string();
                                    has_ghost = true;
                                    redraw(&editor, &ghost_text, true);
                                } else {
                                    redraw(&editor, "", false);
                                }
                            }
                        }
                        0x7f | 0x08 => {
                            let (row, col) = editor.cursor();
                            if col > 0 && editor.line(row)[col - 1] == '\u{FFFC}' {
                                let fffc_idx: usize = (0..row)
                                    .map(|r| {
                                        editor
                                            .line(r)
                                            .iter()
                                            .filter(|&&c| c == '\u{FFFC}')
                                            .count()
                                    })
                                    .sum();
                                editor.delete_back();
                                let (nr, _) = editor.cursor();
                                if editor.line(nr).is_empty() && nr > 0 {
                                    editor.delete_back();
                                }
                                paste_blocks.borrow_mut().remove(fffc_idx);
                                has_ghost = false;
                                ghost_text.clear();
                                self.completer.clear();
                                redraw(&editor, "", false);
                                continue;
                            }
                            if editor.is_empty() && paste_blocks.borrow().is_empty() {
                                if allow_backspace_exit {
                                    disable_paste();
                                    return None;
                                }
                                continue;
                            }
                            if !editor.delete_back() {
                                continue;
                            }
                            has_ghost = false;
                            ghost_text.clear();
                            let content = editor.content();
                            if let Some(g) = self.completer.update(&content) {
                                ghost_text = g.to_string();
                                has_ghost = true;
                                redraw(&editor, &ghost_text, true);
                            } else {
                                self.completer.clear();
                                redraw(&editor, "", false);
                            }
                        }
                        b if b >= 0x20 => {
                            let ch = if b < 0x80 {
                                b as char
                            } else {
                                let mut utf8_buf = vec![b];
                                let expected =
                                    if b < 0xE0 { 1 } else if b < 0xF0 { 2 } else { 3 };
                                for _ in 0..expected {
                                    if nix::unistd::read(stdin_fd, &mut byte).unwrap_or(0) == 1 {
                                        utf8_buf.push(byte[0]);
                                    }
                                }
                                String::from_utf8_lossy(&utf8_buf)
                                    .chars()
                                    .next()
                                    .unwrap_or('?')
                            };
                            editor.insert(ch);
                            has_ghost = false;
                            ghost_text.clear();
                            let content = editor.content();
                            if let Some(g) = self.completer.update(&content) {
                                ghost_text = g.to_string();
                                has_ghost = true;
                                redraw(&editor, &ghost_text, true);
                            } else {
                                self.completer.clear();
                                redraw(&editor, "", false);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {
                    disable_paste();
                    return None;
                }
            }
        }
    }
}

// ── Standalone helpers ───────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum KeyEvent {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    Delete,
    CtrlLeft,
    CtrlRight,
    ShiftEnter,
    PasteStart,
    PasteEnd,
    Esc,
}

fn parse_key_after_esc(stdin_fd: i32) -> Option<KeyEvent> {
    let mut pfd = libc::pollfd { fd: stdin_fd, events: libc::POLLIN, revents: 0 };
    let ready = unsafe { libc::poll(&mut pfd, 1, 15) };
    if ready <= 0 {
        return Some(KeyEvent::Esc);
    }

    let mut b = [0u8; 1];
    if nix::unistd::read(stdin_fd, &mut b) != Ok(1) {
        return Some(KeyEvent::Esc);
    }

    // Accept both CSI ('[') and SS3 ('O') introducer bytes.
    // SS3 is sent when DECCKM (application cursor key mode) is active,
    // which zsh enables by default.
    let is_ss3 = b[0] == b'O';
    match b[0] {
        b'[' | b'O' => {}
        _ => return None,
    }

    // SS3 sequences are always 1 final byte (no params), e.g. \x1bOA
    if is_ss3 {
        if nix::unistd::read(stdin_fd, &mut b) != Ok(1) {
            return None;
        }
        let final_byte = b[0];
        return match final_byte {
            b'A' => Some(KeyEvent::ArrowUp),
            b'B' => Some(KeyEvent::ArrowDown),
            b'C' => Some(KeyEvent::ArrowRight),
            b'D' => Some(KeyEvent::ArrowLeft),
            b'H' => Some(KeyEvent::Home),
            b'F' => Some(KeyEvent::End),
            _ => None,
        };
    }

    let mut params = Vec::new();
    loop {
        if nix::unistd::read(stdin_fd, &mut b) != Ok(1) {
            return None;
        }
        if b[0] >= 0x40 && b[0] <= 0x7E {
            break;
        }
        params.push(b[0]);
    }
    let final_byte = b[0];

    match (params.as_slice(), final_byte) {
        ([], b'A') => Some(KeyEvent::ArrowUp),
        ([], b'B') => Some(KeyEvent::ArrowDown),
        ([], b'C') => Some(KeyEvent::ArrowRight),
        ([], b'D') => Some(KeyEvent::ArrowLeft),
        ([], b'H') => Some(KeyEvent::Home),
        ([], b'F') => Some(KeyEvent::End),
        ([b'3'], b'~') => Some(KeyEvent::Delete),
        ([b'1', b';', b'5'], b'C') => Some(KeyEvent::CtrlRight),
        ([b'1', b';', b'5'], b'D') => Some(KeyEvent::CtrlLeft),
        ([b'1'], b'~') => Some(KeyEvent::Home),
        ([b'4'], b'~') => Some(KeyEvent::End),
        ([b'1', b'3', b';', b'2'], b'u') => Some(KeyEvent::ShiftEnter),
        ([b'2', b'0', b'0'], b'~') => Some(KeyEvent::PasteStart),
        ([b'2', b'0', b'1'], b'~') => Some(KeyEvent::PasteEnd),
        _ => None,
    }
}

fn wait_for_ctrl_c(stop: std::sync::mpsc::Receiver<()>) -> bool {
    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut byte = [0u8; 1];
    loop {
        if stop.try_recv().is_ok() {
            return false;
        }
        let mut pfd = libc::pollfd {
            fd: stdin_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, 100) };
        if ret <= 0 {
            continue;
        }
        match nix::unistd::read(stdin_fd, &mut byte) {
            Ok(1) if byte[0] == 0x03 => return true,
            Ok(1) => {}
            _ => return false,
        }
    }
}

fn save_to_history(history: &mut VecDeque<String>, command: &str, capacity: usize) {
    if command.trim().is_empty() || history.back().is_some_and(|s| s == command) {
        return;
    }
    if history.len() >= capacity {
        history.pop_front();
    }
    history.push_back(command.to_string());
}
