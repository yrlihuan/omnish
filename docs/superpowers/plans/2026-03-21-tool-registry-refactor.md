# ToolRegistry Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract a unified ToolRegistry from PluginManager that manages metadata for ALL tools (plugin tools + built-in tools like CommandQueryTool) so that server.rs doesn't need `is_command_query` branching.

**Architecture:** ToolRegistry owns all tool metadata (definitions, display_name, formatter, status_template, custom status_text). PluginManager retains plugin loading/disk scanning/executable paths. Server uses ToolRegistry uniformly for both plugin and built-in tools - tool execution dispatch remains in server.rs.

**Tech Stack:** Rust, serde_json, existing PluginManager/formatter/CommandQueryTool infrastructure.

**Lifecycle:** ToolRegistry is built once at startup via `register()` / `register_def()` (requires `&mut self`). After initialization, only `update_overrides()` is called at runtime (via `RwLock`-protected fields). `register()` is never called after startup.

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/omnish-daemon/src/tool_registry.rs` | Create | ToolRegistry struct, ToolMeta, registration, metadata queries, `all_defs()` |
| `crates/omnish-daemon/src/plugin.rs` | Modify | Remove metadata query methods, add `register_all(&self, registry)`, update `watch_with` to accept registry |
| `crates/omnish-daemon/src/tools/command_query.rs` | Modify | Add `register(registry)` to register built-in tool metadata; remove redundant `definitions()`, `display_name()`, `status_text()` |
| `crates/omnish-daemon/src/server.rs` | Modify | Replace PluginManager metadata calls + `is_command_query` branches with ToolRegistry |
| `crates/omnish-daemon/src/main.rs` | Modify | Create ToolRegistry, pass to DaemonServer and `watch_with` |
| `crates/omnish-daemon/src/lib.rs` | Modify | Add `pub mod tool_registry;` |

---

### Task 1: Create ToolRegistry with ToolMeta and Registration

**Files:**
- Create: `crates/omnish-daemon/src/tool_registry.rs`
- Modify: `crates/omnish-daemon/src/lib.rs`

- [ ] **Step 1: Write the failing test**

In `tool_registry.rs`, write tests that verify basic registration and lookup:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_lookup_display_name() {
        let mut reg = ToolRegistry::new();
        reg.register(ToolMeta {
            name: "my_tool".to_string(),
            display_name: "MyTool".to_string(),
            formatter: "default".to_string(),
            status_template: "{arg}".to_string(),
            custom_status: None,
            plugin_type: Some(PluginType::DaemonTool),
            plugin_name: Some("builtin".to_string()),
        });
        assert_eq!(reg.display_name("my_tool"), "MyTool");
        assert_eq!(reg.display_name("unknown"), "unknown");
    }

    #[test]
    fn test_formatter_lookup() {
        let mut reg = ToolRegistry::new();
        reg.register(ToolMeta {
            name: "read".to_string(),
            display_name: "Read".to_string(),
            formatter: "read".to_string(),
            status_template: String::new(),
            custom_status: None,
            plugin_type: None,
            plugin_name: None,
        });
        assert_eq!(reg.formatter_name("read"), "read");
        assert_eq!(reg.formatter_name("unknown"), "default");
    }

    #[test]
    fn test_status_text_with_template() {
        let mut reg = ToolRegistry::new();
        reg.register(ToolMeta {
            name: "bash".to_string(),
            display_name: "Bash".to_string(),
            formatter: "default".to_string(),
            status_template: "{command}".to_string(),
            custom_status: None,
            plugin_type: None,
            plugin_name: None,
        });
        let input = serde_json::json!({"command": "ls -la"});
        assert_eq!(reg.status_text("bash", &input), "ls -la");
    }

    #[test]
    fn test_status_text_with_custom_fn() {
        let mut reg = ToolRegistry::new();
        reg.register(ToolMeta {
            name: "omnish_get_output".to_string(),
            display_name: "GetOutput".to_string(),
            formatter: "default".to_string(),
            status_template: String::new(),
            custom_status: Some(Arc::new(|_name, input| {
                let seq = input["seq"].as_u64().unwrap_or(0);
                let cmd = input["command"].as_str().unwrap_or("");
                format!("[{}] {}", seq, cmd)
            })),
            plugin_type: None,
            plugin_name: None,
        });
        let input = serde_json::json!({"seq": 5, "command": "ls"});
        assert_eq!(reg.status_text("omnish_get_output", &input), "[5] ls");
    }

    #[test]
    fn test_plugin_type_lookup() {
        let mut reg = ToolRegistry::new();
        reg.register(ToolMeta {
            name: "bash".to_string(),
            display_name: "Bash".to_string(),
            formatter: "default".to_string(),
            status_template: String::new(),
            custom_status: None,
            plugin_type: Some(PluginType::ClientTool),
            plugin_name: Some("builtin".to_string()),
        });
        assert_eq!(reg.plugin_type("bash"), Some(PluginType::ClientTool));
        assert_eq!(reg.plugin_type("unknown"), None);
    }

    #[test]
    fn test_override_updates() {
        let mut reg = ToolRegistry::new();
        reg.register_def(omnish_llm::tool::ToolDef {
            name: "bash".to_string(),
            description: "Original".to_string(),
            input_schema: serde_json::json!({}),
        });
        let defs = reg.all_defs();
        assert_eq!(defs[0].description, "Original");

        let mut descs = HashMap::new();
        descs.insert("bash".to_string(), "Overridden".to_string());
        reg.update_overrides(descs, HashMap::new());
        let defs = reg.all_defs();
        assert_eq!(defs[0].description, "Overridden");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon tool_registry`
Expected: FAIL - module doesn't exist yet

- [ ] **Step 3: Write ToolRegistry implementation**

Design note: `descriptions` and `override_params` are behind `RwLock` from the start because `update_overrides()` is called at runtime by the file watcher (which only has `&self`). All other fields (`tools`, `defs`) are written once at startup and never modified.

```rust
use crate::plugin::PluginType;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Custom status text function: (tool_name, input) -> status string.
pub type CustomStatusFn = Arc<dyn Fn(&str, &serde_json::Value) -> String + Send + Sync>;

/// Metadata for a single tool (plugin or built-in).
pub struct ToolMeta {
    pub name: String,
    pub display_name: String,
    pub formatter: String,
    pub status_template: String,
    /// Optional custom status_text function (overrides template interpolation).
    pub custom_status: Option<CustomStatusFn>,
    /// Plugin type (ClientTool/DaemonTool). None for built-in tools that don't go through plugins.
    pub plugin_type: Option<PluginType>,
    /// Plugin directory name (for executable lookup). None for built-in tools.
    pub plugin_name: Option<String>,
}

/// Unified registry for all tool metadata (plugin + built-in).
///
/// Built once at startup via `register()` / `register_def()`. After initialization,
/// only `update_overrides()` is called at runtime (RwLock-protected fields).
///
/// Provides:
/// - `display_name()`, `formatter_name()`, `status_text()` - UI metadata
/// - `plugin_type()`, `plugin_name()` - execution routing
/// - `all_defs()` - tool definitions for LLM prompt (with description overrides)
///
/// Does NOT own:
/// - Tool execution logic (server.rs decides how to call tools)
/// - Plugin loading from disk (PluginManager handles this)
/// - Override file watching (PluginManager handles this, calls update_overrides)
pub struct ToolRegistry {
    /// Static tool metadata (set once at startup).
    tools: HashMap<String, ToolMeta>,
    /// Base tool definitions (set once at startup).
    defs: HashMap<String, omnish_llm::tool::ToolDef>,
    /// tool_name -> effective description (updated at runtime by override reload).
    descriptions: RwLock<HashMap<String, String>>,
    /// tool_name -> override params (updated at runtime by override reload).
    override_params: RwLock<HashMap<String, HashMap<String, serde_json::Value>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            defs: HashMap::new(),
            descriptions: RwLock::new(HashMap::new()),
            override_params: RwLock::new(HashMap::new()),
        }
    }

    /// Register a tool's metadata (startup only).
    pub fn register(&mut self, meta: ToolMeta) {
        self.tools.insert(meta.name.clone(), meta);
    }

    /// Register a tool definition for LLM prompt construction (startup only).
    pub fn register_def(&mut self, def: omnish_llm::tool::ToolDef) {
        self.defs.insert(def.name.clone(), def);
    }

    /// Get display name for a tool. Falls back to tool_name if not registered.
    pub fn display_name(&self, tool_name: &str) -> &str {
        self.tools.get(tool_name)
            .map(|m| m.display_name.as_str())
            .unwrap_or(tool_name)
    }

    /// Get formatter name for a tool. Falls back to "default".
    pub fn formatter_name(&self, tool_name: &str) -> &str {
        self.tools.get(tool_name)
            .map(|m| m.formatter.as_str())
            .unwrap_or("default")
    }

    /// Get status template for a tool. Falls back to "".
    pub fn status_template(&self, tool_name: &str) -> &str {
        self.tools.get(tool_name)
            .map(|m| m.status_template.as_str())
            .unwrap_or("")
    }

    /// Get status text for a tool call. Uses custom_status if set, otherwise interpolates template.
    pub fn status_text(&self, tool_name: &str, input: &serde_json::Value) -> String {
        if let Some(meta) = self.tools.get(tool_name) {
            if let Some(ref custom) = meta.custom_status {
                return custom(tool_name, input);
            }
            interpolate_template(&meta.status_template, input)
        } else {
            String::new()
        }
    }

    /// Get plugin type for a tool.
    pub fn plugin_type(&self, tool_name: &str) -> Option<PluginType> {
        self.tools.get(tool_name).and_then(|m| m.plugin_type)
    }

    /// Get plugin directory name for a tool.
    pub fn plugin_name(&self, tool_name: &str) -> Option<&str> {
        self.tools.get(tool_name).and_then(|m| m.plugin_name.as_deref())
    }

    /// Whether a tool is registered at all (used for execution dispatch).
    pub fn is_known(&self, tool_name: &str) -> bool {
        self.tools.contains_key(tool_name)
    }

    /// Get override params for a tool (from tool.override.json).
    pub fn override_params(&self, tool_name: &str) -> Option<HashMap<String, serde_json::Value>> {
        let lock = self.override_params.read().unwrap();
        lock.get(tool_name).cloned()
    }

    /// Collect all tool definitions with description overrides applied.
    pub fn all_defs(&self) -> Vec<omnish_llm::tool::ToolDef> {
        let descs = self.descriptions.read().unwrap();
        self.defs.values().map(|def| {
            let mut d = def.clone();
            if let Some(desc) = descs.get(&d.name) {
                d.description = desc.clone();
            }
            d
        }).collect()
    }

    /// Update description overrides and override params (called at runtime by file watcher).
    pub fn update_overrides(
        &self,
        descriptions: HashMap<String, String>,
        new_override_params: HashMap<String, HashMap<String, serde_json::Value>>,
    ) {
        *self.descriptions.write().unwrap() = descriptions;
        *self.override_params.write().unwrap() = new_override_params;
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
```

Note: `override_params()` returns `Option<HashMap<...>>` (cloned) rather than `Option<&HashMap<...>>` because the data is behind a `RwLock`.

- [ ] **Step 4: Add module declaration in lib.rs**

Add `pub mod tool_registry;` to `crates/omnish-daemon/src/lib.rs`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p omnish-daemon tool_registry`
Expected: All 6 tests PASS

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-daemon/src/tool_registry.rs crates/omnish-daemon/src/lib.rs
git commit -m "feat: add ToolRegistry for unified tool metadata management"
```

---

### Task 2: Add PluginManager.register_all() to Populate ToolRegistry

**Files:**
- Modify: `crates/omnish-daemon/src/plugin.rs`

- [ ] **Step 1: Write the failing test**

Note: the builtin tool.json defines `"display_name": "Bash"` for the bash tool (not the raw name).

```rust
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
    let mgr = PluginManager::load(tmp.path());
    let mut reg = ToolRegistry::new();
    mgr.register_all(&mut reg);
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon test_register_all`
Expected: FAIL - `register_all` doesn't exist

- [ ] **Step 3: Implement register_all on PluginManager**

Add to `impl PluginManager`:

```rust
/// Register all plugin tools into a ToolRegistry.
/// Includes tool metadata (display_name, formatter, status_template)
/// and tool definitions (for LLM prompt construction).
pub fn register_all(&self, registry: &mut crate::tool_registry::ToolRegistry) {
    for plugin in &self.plugins {
        for te in &plugin.tools {
            registry.register(crate::tool_registry::ToolMeta {
                name: te.def.name.clone(),
                display_name: te.display_name.clone(),
                formatter: te.formatter.clone(),
                status_template: te.status_template.clone(),
                custom_status: None,
                plugin_type: Some(plugin.plugin_type),
                plugin_name: Some(plugin.dir_name.clone()),
            });
            registry.register_def(te.def.clone());
        }
    }
    // Apply current overrides
    let cache = self.prompt_cache.read().unwrap();
    registry.update_overrides(cache.descriptions.clone(), cache.override_params.clone());
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p omnish-daemon test_register_all`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs
git commit -m "feat: add PluginManager.register_all() for ToolRegistry population"
```

---

### Task 3: Add CommandQueryTool.register() for Built-in Tool Metadata

**Files:**
- Modify: `crates/omnish-daemon/src/tools/command_query.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_register_command_query_tools() {
    use crate::tool_registry::ToolRegistry;
    let mut reg = ToolRegistry::new();
    CommandQueryTool::register(&mut reg);
    assert_eq!(reg.display_name("omnish_list_history"), "History");
    assert_eq!(reg.display_name("omnish_get_output"), "GetOutput");
    // Definitions should be registered too
    let defs = reg.all_defs();
    assert!(defs.iter().any(|d| d.name == "omnish_list_history"));
    assert!(defs.iter().any(|d| d.name == "omnish_get_output"));
}

#[test]
fn test_register_custom_status_text() {
    use crate::tool_registry::ToolRegistry;
    let mut reg = ToolRegistry::new();
    CommandQueryTool::register(&mut reg);
    let input = serde_json::json!({"seq": 3, "command": "git status"});
    assert_eq!(reg.status_text("omnish_get_output", &input), "[3] git status");
    let input2 = serde_json::json!({"count": 10});
    assert_eq!(reg.status_text("omnish_list_history", &input2), "last 10");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon test_register_command_query`
Expected: FAIL - `register` doesn't exist

- [ ] **Step 3: Implement CommandQueryTool::register()**

Add a static method that registers both tool metadata and definitions. The tool definitions here are the canonical source - the existing `definitions()` instance method will be removed in Task 7.

```rust
/// Register command query tool metadata and definitions with a ToolRegistry.
/// This is a static method - it doesn't need a CommandQueryTool instance.
pub fn register(registry: &mut crate::tool_registry::ToolRegistry) {
    use crate::tool_registry::{ToolMeta, CustomStatusFn};
    use std::sync::Arc;

    let status_fn: CustomStatusFn = Arc::new(|tool_name, input| {
        match tool_name {
            "omnish_list_history" => {
                let count = input["count"].as_u64().unwrap_or(20);
                format!("last {}", count)
            }
            "omnish_get_output" => {
                let seq = input["seq"].as_u64().unwrap_or(0);
                let command = input["command"].as_str().unwrap_or("");
                if command.is_empty() {
                    format!("[{}]", seq)
                } else {
                    format!("[{}] {}", seq, command)
                }
            }
            _ => String::new(),
        }
    });

    // Register metadata for omnish_list_history
    registry.register(ToolMeta {
        name: "omnish_list_history".to_string(),
        display_name: "History".to_string(),
        formatter: "default".to_string(),
        status_template: String::new(),
        custom_status: Some(status_fn.clone()),
        plugin_type: None,
        plugin_name: None,
    });

    // Register metadata for omnish_get_output
    registry.register(ToolMeta {
        name: "omnish_get_output".to_string(),
        display_name: "GetOutput".to_string(),
        formatter: "default".to_string(),
        status_template: String::new(),
        custom_status: Some(status_fn),
        plugin_type: None,
        plugin_name: None,
    });

    // Register tool definitions (for LLM prompt)
    registry.register_def(omnish_llm::tool::ToolDef {
        name: "omnish_list_history".to_string(),
        description: "List recent shell command history. \
            The last 5 commands are provided in <system-reminder> at the end of each user message, \
            so you do NOT need to call this unless you need older commands.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "count": {
                    "type": "integer",
                    "description": "Number of recent commands to list (default 20)"
                }
            }
        }),
    });

    registry.register_def(omnish_llm::tool::ToolDef {
        name: "omnish_get_output".to_string(),
        description: "Get the full output of a specific shell command by its sequence number. \
            Use omnish_list_history to find the sequence number first.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "seq": {
                    "type": "integer",
                    "description": "Command sequence number (from omnish_list_history or <system-reminder>)"
                },
                "command": {
                    "type": "string",
                    "description": "The command string at that seq (must match the recorded command)"
                }
            },
            "required": ["seq", "command"]
        }),
    });
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p omnish-daemon test_register_command_query`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/tools/command_query.rs
git commit -m "feat: add CommandQueryTool::register() for ToolRegistry"
```

---

### Task 4: Wire ToolRegistry into Server - Replace Metadata Queries

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`
- Modify: `crates/omnish-daemon/src/main.rs`

This is the main integration task. Replace all `is_command_query` branches and `plugin_mgr.tool_*()` metadata calls with `tool_registry.*()` calls.

There are **4 locations** in server.rs that use plugin_mgr for tool metadata:
1. `run_agent_loop` pre-execution (lines ~1130-1152)
2. `run_agent_loop` post-execution (lines ~1225-1259)
3. `run_agent_loop` client-side tool dispatch (lines ~1168-1200)
4. `handle_tool_result` client result handler (lines ~969-998)

Plus `reconstruct_history` (handled in Task 5).

- [ ] **Step 1: Add tool_registry to DaemonServer and create at startup**

In `main.rs` (line ~258), after `PluginManager::load()`:

```rust
let plugin_mgr = Arc::new(omnish_daemon::plugin::PluginManager::load(&plugins_dir));

// Build unified tool registry from plugins + built-in tools
let mut tool_registry = omnish_daemon::tool_registry::ToolRegistry::new();
plugin_mgr.register_all(&mut tool_registry);
omnish_daemon::tools::command_query::CommandQueryTool::register(&mut tool_registry);
let tool_registry = Arc::new(tool_registry);
```

Add `tool_registry: Arc<ToolRegistry>` field to `DaemonServer` struct (line ~114).
Update `DaemonServer::new()` to accept and store it.
Update `handle_message()` to pass `&tool_registry` through.

- [ ] **Step 2: Update `build_chat_setup` to use registry.all_defs()**

`build_chat_setup` still creates a `CommandQueryTool` instance for execution (it holds live command data). Only the tool definitions list changes to come from the registry.

Change from:
```rust
let mut tools = command_query_tool.definitions();
tools.extend(plugin_mgr.all_tools());
```

To:
```rust
let tools = tool_registry.all_defs();
```

Update `build_chat_setup` signature to accept `&ToolRegistry` instead of `&PluginManager` (for the tools list only - the function still creates `CommandQueryTool` for execution).

- [ ] **Step 3: Update `run_agent_loop` pre-execution metadata - remove `is_command_query` branches**

Replace the pre-execution metadata block (lines ~1130-1152):

From:
```rust
let is_command_query = tc.name == "omnish_list_history" || tc.name == "omnish_get_output";
let display_name = if is_command_query {
    CommandQueryTool::display_name(&tc.name).to_string()
} else {
    plugin_mgr.tool_display_name(&tc.name).unwrap_or(&tc.name).to_string()
};
let formatter_name = plugin_mgr.tool_formatter(&tc.name).unwrap_or("default");
let status_template = plugin_mgr.tool_status_template(&tc.name).unwrap_or("").to_string();
let fmt = formatter::get_formatter(formatter_name);
let mut fmt_out = fmt.format(&FormatInput { ... });
if is_command_query {
    fmt_out.param_desc = state.command_query_tool.status_text(&tc.name, &tc.input);
}
```

To:
```rust
let display_name = tool_registry.display_name(&tc.name).to_string();
let formatter_name = tool_registry.formatter_name(&tc.name);
let status_template = tool_registry.status_template(&tc.name).to_string();
let fmt = formatter::get_formatter(formatter_name);
let mut fmt_out = fmt.format(&FormatInput {
    tool_name: tc.name.clone(),
    display_name: display_name.clone(),
    status_template,
    params: tc.input.clone(),
    output: None,
    is_error: None,
});
fmt_out.param_desc = tool_registry.status_text(&tc.name, &tc.input);
```

Note: `status_text()` is now called unconditionally - for plugin tools it interpolates the template (same result as before), for built-in tools it calls the custom function.

- [ ] **Step 4: Update `run_agent_loop` post-execution metadata block (lines ~1225-1259)**

Same pattern - remove `is_command_query` check, use `tool_registry` uniformly:

```rust
let post_fmt = formatter::get_formatter(tool_registry.formatter_name(&tc.name));
let post_display = tool_registry.display_name(&tc.name).to_string();
let post_template = tool_registry.status_template(&tc.name).to_string();
let mut post_out = post_fmt.format(&FormatInput {
    tool_name: tc.name.clone(),
    display_name: post_display.clone(),
    status_template: post_template,
    params: tc.input.clone(),
    output: Some(result.content.clone()),
    is_error: Some(result.is_error),
});
post_out.param_desc = tool_registry.status_text(&tc.name, &tc.input);
```

- [ ] **Step 5: Update `handle_tool_result` client result handler (lines ~969-998)**

This is the handler for incoming client-side tool results. Replace plugin_mgr metadata calls:

From:
```rust
let fmt = formatter::get_formatter(
    plugin_mgr.tool_formatter(&tc.name).unwrap_or("default")
);
let display_name = plugin_mgr.tool_display_name(&tc.name)
    .unwrap_or(&tc.name).to_string();
let status_template = plugin_mgr.tool_status_template(&tc.name)
    .unwrap_or("").to_string();
```

To:
```rust
let fmt = formatter::get_formatter(tool_registry.formatter_name(&tc.name));
let display_name = tool_registry.display_name(&tc.name).to_string();
let status_template = tool_registry.status_template(&tc.name).to_string();
```

Update `handle_tool_result` signature to accept `&Arc<ToolRegistry>`.

- [ ] **Step 6: Update execution dispatch - use plugin_type for routing**

The execution dispatch (line ~1212) uses `is_command_query` to decide whether to call `command_query_tool.execute()` or `plugin_executable()`. Replace with `plugin_type()` check:

From:
```rust
let mut result = if tc.name == "omnish_list_history" || tc.name == "omnish_get_output" {
    state.command_query_tool.execute(&tc.name, &merged_input)
} else if let Some(exe) = plugin_mgr.plugin_executable(&tc.name) {
    execute_daemon_plugin(&exe, &tc.name, &merged_input).await
} else {
    omnish_llm::tool::ToolResult {
        tool_use_id: String::new(),
        content: format!("Unknown daemon tool: {}", tc.name),
        is_error: true,
    }
};
```

To:
```rust
let mut result = if tool_registry.plugin_type(&tc.name).is_some() {
    // Plugin tool - execute via plugin executable
    if let Some(exe) = plugin_mgr.plugin_executable(&tc.name) {
        execute_daemon_plugin(&exe, &tc.name, &merged_input).await
    } else {
        omnish_llm::tool::ToolResult {
            tool_use_id: String::new(),
            content: format!("Unknown daemon tool: {}", tc.name),
            is_error: true,
        }
    }
} else if tool_registry.is_known(&tc.name) {
    // Known built-in tool (no plugin) - execute directly
    state.command_query_tool.execute(&tc.name, &merged_input)
} else {
    // Completely unknown tool
    omnish_llm::tool::ToolResult {
        tool_use_id: String::new(),
        content: format!("Unknown tool: {}", tc.name),
        is_error: true,
    }
};
```

This correctly routes: plugin tools -> plugin_executable, known built-in tools -> command_query_tool, unknown tools -> error.

- [ ] **Step 7: Update client-side tool dispatch to use registry**

Replace `plugin_mgr.tool_plugin_type()` with `tool_registry.plugin_type()`:

```rust
let ptype = tool_registry.plugin_type(&tc.name);
```

Replace `plugin_mgr.tool_override_params()` with `tool_registry.override_params()`:

```rust
if let Some(override_params) = tool_registry.override_params(&tc.name) {
    merge_tool_params(&mut merged_input, &override_params);
}
```

Replace `plugin_mgr.tool_plugin_name()` with `tool_registry.plugin_name()`:

```rust
plugin_name: tool_registry.plugin_name(&tc.name).unwrap_or("builtin").to_string(),
```

- [ ] **Step 8: Run cargo check to verify compilation**

Run: `cargo check -p omnish-daemon`
Expected: No errors

- [ ] **Step 9: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/main.rs
git commit -m "refactor: use ToolRegistry for all tool metadata in agent loop"
```

---

### Task 5: Update reconstruct_history to Use ToolRegistry

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs` (the `reconstruct_history` function and callers)

- [ ] **Step 1: Change reconstruct_history signature**

From:
```rust
fn reconstruct_history(
    raw_messages: &[serde_json::Value],
    plugin_mgr: &PluginManager,
) -> Vec<serde_json::Value> {
```

To:
```rust
fn reconstruct_history(
    raw_messages: &[serde_json::Value],
    tool_registry: &crate::tool_registry::ToolRegistry,
) -> Vec<serde_json::Value> {
```

- [ ] **Step 2: Update metadata lookups inside reconstruct_history**

Replace (lines ~1507-1513):
```rust
let formatter_name = plugin_mgr.tool_formatter(&tool_name).unwrap_or("default");
let fmt = formatter::get_formatter(formatter_name);
let display_name = plugin_mgr.tool_display_name(&tool_name).unwrap_or(&tool_name).to_string();
let status_template = plugin_mgr.tool_status_template(&tool_name).unwrap_or("").to_string();
```

With:
```rust
let fmt = formatter::get_formatter(tool_registry.formatter_name(&tool_name));
let display_name = tool_registry.display_name(&tool_name).to_string();
let status_template = tool_registry.status_template(&tool_name).to_string();
```

Also add `param_desc` override via `status_text` (previously missing for command query tools in history reconstruction):
```rust
let fmt_out = fmt.format(&FormatInput { ... });
// Use registry's status_text for proper param_desc (handles both plugin template interpolation and built-in custom functions)
let param_desc = tool_registry.status_text(&tool_name, &input);
let param_desc = if param_desc.is_empty() { fmt_out.param_desc } else { param_desc };
```

- [ ] **Step 3: Update all callers of reconstruct_history**

There are 3 call sites:
1. `build_resume_response()` at line ~1613 - update signature to take `&ToolRegistry`
2. ChatStart handler at line ~578 - pass `tool_registry` instead of `plugin_mgr`
3. ChatStart auto-resume handler at line ~653 - pass `tool_registry` instead of `plugin_mgr`

Update `build_resume_response`:
```rust
fn build_resume_response(
    thread_id: &str,
    conv_mgr: &ConversationManager,
    tool_registry: &crate::tool_registry::ToolRegistry,
    llm_backend: &Option<Arc<dyn LlmBackend>>,
) -> serde_json::Value {
    let raw_msgs = conv_mgr.load_raw_messages(thread_id);
    let history = reconstruct_history(&raw_msgs, tool_registry);
    ...
```

- [ ] **Step 4: Run cargo check**

Run: `cargo check -p omnish-daemon`
Expected: No errors

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "refactor: reconstruct_history uses ToolRegistry for all tools"
```

---

### Task 6: Update Override Reload to Flow Through ToolRegistry

**Files:**
- Modify: `crates/omnish-daemon/src/plugin.rs`
- Modify: `crates/omnish-daemon/src/main.rs`

The existing `PluginManager::reload_overrides()` updates the internal `prompt_cache`. We need the overrides to also flow to ToolRegistry.

- [ ] **Step 1: Make reload_overrides return the computed overrides**

Change `reload_overrides` to return the computed descriptions and override_params:

```rust
/// Re-read all tool.override.json files, update internal cache, and return
/// the computed overrides for ToolRegistry.
pub fn reload_overrides(&self) -> (HashMap<String, String>, HashMap<String, HashMap<String, serde_json::Value>>) {
    // ... existing logic to build descriptions/override_params ...
    let mut cache = self.prompt_cache.write().unwrap();
    cache.descriptions = descriptions.clone();
    cache.override_params = override_params.clone();
    (descriptions, override_params)
}
```

- [ ] **Step 2: Update watch_with to accept and update ToolRegistry**

`watch_with` is defined in `plugin.rs` (line 378) and spawned in `main.rs` (line 278). Update it to accept an `Arc<ToolRegistry>`:

In `plugin.rs`:
```rust
/// Start watching plugin overrides using a shared file watcher receiver.
/// Updates both internal cache and the external ToolRegistry.
pub async fn watch_with(self: &Arc<Self>, mut rx: tokio::sync::watch::Receiver<()>, registry: Arc<crate::tool_registry::ToolRegistry>) {
    tracing::info!("watching plugin overrides via shared file watcher: {}", self.plugins_dir.display());
    while rx.changed().await.is_ok() {
        tracing::info!("tool.override.json changed, reloading...");
        let (descs, params) = self.reload_overrides();
        registry.update_overrides(descs, params);
    }
}
```

In `main.rs`, update the spawn call (line ~278):
```rust
let plugin_mgr_watcher = Arc::clone(&plugin_mgr);
let tool_registry_watcher = Arc::clone(&tool_registry);
let plugin_rx = file_watcher.watch(plugins_dir.clone());
tokio::spawn(async move { plugin_mgr_watcher.watch_with(plugin_rx, tool_registry_watcher).await });
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check -p omnish-daemon`
Expected: No errors

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs crates/omnish-daemon/src/main.rs
git commit -m "refactor: override reload flows through ToolRegistry"
```

---

### Task 7: Remove Redundant Methods and Clean Up

**Files:**
- Modify: `crates/omnish-daemon/src/plugin.rs`
- Modify: `crates/omnish-daemon/src/tools/command_query.rs`
- Modify: `crates/omnish-daemon/src/server.rs`

- [ ] **Step 1: Remove metadata query methods from PluginManager**

Remove these methods (now served by ToolRegistry):
- `tool_display_name()`
- `tool_formatter()`
- `tool_status_template()`
- `tool_status_text()`
- `tool_plugin_type()`
- `tool_plugin_name()`
- `tool_override_params()`
- `all_tools()`

Also remove the now-unused `interpolate_template` function from `plugin.rs` (the canonical version is in `tool_registry.rs`).

Keep:
- `load()` - plugin loading from disk
- `plugin_executable()` - returns executable path for a tool's plugin
- `reload_overrides()` - re-reads override files (now returns tuple)
- `watch_with()` - file watcher
- `register_all()` - populates ToolRegistry

- [ ] **Step 2: Remove redundant methods from CommandQueryTool**

Remove these methods (now served by ToolRegistry):
- `definitions()` - tool defs are now registered via `CommandQueryTool::register()`
- `display_name()` - now in registry
- `status_text()` - now in registry via `custom_status`

Keep:
- `new()` - creates instance with live command data
- `list_history()` - used by server for `/history` command
- `build_system_reminder()` - used by server for system-reminder injection
- `execute()` - executes tool calls at runtime
- `register()` - registers metadata with ToolRegistry

- [ ] **Step 3: Update any remaining references in server.rs**

Ensure server.rs no longer calls any removed PluginManager or CommandQueryTool methods. The only PluginManager methods server.rs should call are:
- `plugin_executable()` - for tool execution

- [ ] **Step 4: Update tests**

- Remove or update PluginManager tests that call removed methods (the functionality is now tested through ToolRegistry tests and `test_register_all`).
- Keep PluginManager tests for `load()`, `plugin_executable()`, `reload_overrides()`.

- [ ] **Step 5: Run all tests**

Run: `cargo test -p omnish-daemon`
Expected: All tests PASS

- [ ] **Step 6: Run cargo clippy**

Run: `cargo clippy -p omnish-daemon`
Expected: No warnings from changed code

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs crates/omnish-daemon/src/tools/command_query.rs crates/omnish-daemon/src/server.rs
git commit -m "refactor: remove redundant metadata methods from PluginManager and CommandQueryTool"
```

---

### Task 8: Final Verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test -p omnish-daemon`
Expected: All tests PASS

- [ ] **Step 2: Run cargo clippy on full project**

Run: `cargo clippy --all`
Expected: No new warnings

- [ ] **Step 3: Build check**

Run: `cargo check`
Expected: Clean build

- [ ] **Step 4: Verify no remaining is_command_query references**

Run: `grep -r "is_command_query" crates/omnish-daemon/src/`
Expected: No matches

- [ ] **Step 5: Verify no remaining plugin_mgr.tool_ metadata calls in server.rs**

Run: `grep "plugin_mgr\.tool_display_name\|plugin_mgr\.tool_formatter\|plugin_mgr\.tool_status\|plugin_mgr\.tool_plugin_type\|plugin_mgr\.tool_plugin_name\|plugin_mgr\.tool_override_params\|plugin_mgr\.all_tools" crates/omnish-daemon/src/server.rs`
Expected: No matches
