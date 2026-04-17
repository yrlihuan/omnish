# Plugin System Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a tool plugin system that allows official and user plugins to extend the LLM's available tools through a unified `Plugin` trait and JSON-RPC 2.0 protocol.

**Architecture:** Define a `Plugin` trait in `omnish-daemon`. Official plugins (e.g., `CommandQueryTool`) implement it directly. External plugins run as long-lived subprocesses communicating via stdin/stdout JSON-RPC, wrapped by a `PluginHandle` adapter. A `PluginManager` aggregates all plugins and replaces the current ad-hoc tool registration in `handle_chat_message`.

**Tech Stack:** Rust, serde_json, tokio::process, JSON-RPC 2.0

---

### Task 1: Add PluginsConfig to daemon config

**Files:**
- Modify: `crates/omnish-common/src/config.rs`

**Step 1: Add PluginsConfig struct**

Add after the `TasksConfig` section (line 144):

```rust
// ---------------------------------------------------------------------------
// Plugins config
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Clone)]
pub struct PluginsConfig {
    /// List of enabled plugin names. Each corresponds to a directory
    /// under ~/.omnish/plugins/{name}/ containing a {name} executable.
    #[serde(default)]
    pub enabled: Vec<String>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self { enabled: vec![] }
    }
}
```

**Step 2: Add plugins field to DaemonConfig**

```rust
pub struct DaemonConfig {
    // ... existing fields ...
    #[serde(default)]
    pub plugins: PluginsConfig,
}
```

Update `Default for DaemonConfig` to include `plugins: PluginsConfig::default()`.

**Step 3: Build and check**

Run: `cargo build -p omnish-common 2>&1 | tail -5`
Expected: Compiles.

**Step 4: Commit**

```bash
git add crates/omnish-common/src/config.rs
git commit -m "feat(config): add PluginsConfig for tool plugin system"
```

---

### Task 2: Create Plugin trait and PluginManager

**Files:**
- Create: `crates/omnish-daemon/src/plugin.rs`
- Modify: `crates/omnish-daemon/src/lib.rs` (add `pub mod plugin;`)

**Step 1: Create plugin.rs with Plugin trait and PluginManager**

```rust
use omnish_llm::tool::{ToolDef, ToolResult};

/// Unified plugin interface for both official (inline) and external (subprocess) plugins.
pub trait Plugin: Send + Sync {
    /// Plugin name (for logging and identification).
    fn name(&self) -> &str;
    /// Tool definitions this plugin provides (sent to LLM).
    fn tools(&self) -> Vec<ToolDef>;
    /// Execute a tool by name with the given input.
    fn execute(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult;
}

/// Manages all registered plugins (official + external).
pub struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self { plugins: vec![] }
    }

    /// Register a plugin.
    pub fn register(&mut self, plugin: Box<dyn Plugin>) {
        tracing::info!("Registered plugin '{}' with {} tools",
            plugin.name(),
            plugin.tools().len());
        self.plugins.push(plugin);
    }

    /// Collect all tool definitions from all plugins.
    pub fn all_tools(&self) -> Vec<ToolDef> {
        self.plugins.iter().flat_map(|p| p.tools()).collect()
    }

    /// Find the plugin that owns the given tool name and execute it.
    pub fn execute(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult {
        for plugin in &self.plugins {
            if plugin.tools().iter().any(|t| t.name == tool_name) {
                return plugin.execute(tool_name, input);
            }
        }
        ToolResult {
            tool_use_id: String::new(),
            content: format!("Unknown tool: {}", tool_name),
            is_error: true,
        }
    }
}
```

**Step 2: Add module to lib.rs**

Add `pub mod plugin;` to `crates/omnish-daemon/src/lib.rs`.

**Step 3: Build and check**

Run: `cargo build -p omnish-daemon 2>&1 | tail -5`
Expected: Compiles.

**Step 4: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs crates/omnish-daemon/src/lib.rs
git commit -m "feat(daemon): add Plugin trait and PluginManager"
```

---

### Task 3: Migrate CommandQueryTool to Plugin trait

**Files:**
- Modify: `crates/omnish-daemon/src/tools/command_query.rs`

**Step 1: Implement Plugin trait for CommandQueryTool**

Keep the existing `Tool` trait implementation and add `Plugin` implementation:

```rust
use crate::plugin::Plugin;

impl Plugin for CommandQueryTool {
    fn name(&self) -> &str {
        "command_query"
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![self.definition()]
    }

    fn execute_plugin(&self, _tool_name: &str, input: &serde_json::Value) -> ToolResult {
        self.execute(input)
    }
}
```

Wait - the `Plugin` trait's `execute` method conflicts with `Tool::execute` since they have the same name. Two solutions:

**Option A**: Rename `Plugin::execute` to something else (e.g., `call_tool`).
**Option B**: Remove `Tool` trait impl from `CommandQueryTool` (it's only used via `Plugin` now).

Go with **Option A** - rename to `call_tool` in the `Plugin` trait to avoid name collision:

```rust
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn tools(&self) -> Vec<ToolDef>;
    fn call_tool(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult;
}
```

Update `PluginManager::execute` to call `call_tool` instead.

Then implement for `CommandQueryTool`:

```rust
impl Plugin for CommandQueryTool {
    fn name(&self) -> &str {
        "command_query"
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![self.definition()]
    }

    fn call_tool(&self, _tool_name: &str, input: &serde_json::Value) -> ToolResult {
        self.execute(input)
    }
}
```

**Step 2: Build and check**

Run: `cargo build -p omnish-daemon 2>&1 | tail -5`
Expected: Compiles.

**Step 3: Commit**

```bash
git add crates/omnish-daemon/src/tools/command_query.rs crates/omnish-daemon/src/plugin.rs
git commit -m "feat(tools): implement Plugin trait for CommandQueryTool"
```

---

### Task 4: Add ExternalPlugin (JSON-RPC subprocess adapter)

**Files:**
- Modify: `crates/omnish-daemon/src/plugin.rs`

**Step 1: Add JSON-RPC types**

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    method: String,
    id: u64,
    params: serde_json::Value,
}

#[derive(Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    id: u64,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct InitializeResult {
    #[allow(dead_code)]
    name: String,
    tools: Vec<ToolDef>,
}

#[derive(Deserialize)]
struct ExecuteResult {
    content: String,
    #[serde(default)]
    is_error: bool,
}
```

**Step 2: Add ExternalPlugin struct**

```rust
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

pub struct ExternalPlugin {
    plugin_name: String,
    stdin: Mutex<std::io::BufWriter<ChildStdin>>,
    stdout: Mutex<BufReader<ChildStdout>>,
    child: Mutex<Child>,
    tool_defs: Vec<ToolDef>,
    next_id: Mutex<u64>,
}

impl ExternalPlugin {
    /// Spawn a plugin subprocess and initialize it.
    /// Returns None if the plugin fails to start or initialize.
    pub fn spawn(name: &str, executable: &std::path::Path) -> Option<Self> {
        let mut child = match Command::new(executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to spawn plugin '{}': {}", name, e);
                return None;
            }
        };

        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;

        let mut plugin = Self {
            plugin_name: name.to_string(),
            stdin: Mutex::new(std::io::BufWriter::new(stdin)),
            stdout: Mutex::new(BufReader::new(stdout)),
            child: Mutex::new(child),
            tool_defs: vec![],
            next_id: Mutex::new(1),
        };

        // Send initialize request
        match plugin.send_request("initialize", serde_json::json!({})) {
            Ok(result) => {
                match serde_json::from_value::<InitializeResult>(result) {
                    Ok(init) => {
                        tracing::info!("Plugin '{}' initialized with {} tools",
                            name, init.tools.len());
                        plugin.tool_defs = init.tools;
                        Some(plugin)
                    }
                    Err(e) => {
                        tracing::warn!("Plugin '{}' initialize response invalid: {}", name, e);
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Plugin '{}' initialize failed: {}", name, e);
                None
            }
        }
    }

    fn send_request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let id = {
            let mut next = self.next_id.lock().unwrap();
            let id = *next;
            *next += 1;
            id
        };

        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            method: method.to_string(),
            id,
            params,
        };

        let msg = serde_json::to_string(&req).map_err(|e| e.to_string())?;

        {
            let mut stdin = self.stdin.lock().unwrap();
            writeln!(stdin, "{}", msg).map_err(|e| format!("write to plugin: {}", e))?;
            stdin.flush().map_err(|e| format!("flush to plugin: {}", e))?;
        }

        let mut line = String::new();
        {
            let mut stdout = self.stdout.lock().unwrap();
            stdout.read_line(&mut line).map_err(|e| format!("read from plugin: {}", e))?;
        }

        let resp: JsonRpcResponse = serde_json::from_str(&line)
            .map_err(|e| format!("parse response: {}", e))?;

        if resp.id != id {
            return Err(format!("response id mismatch: expected {}, got {}", id, resp.id));
        }

        if let Some(err) = resp.error {
            return Err(format!("plugin error: {}", err));
        }

        resp.result.ok_or_else(|| "empty result".to_string())
    }

    /// Send shutdown and kill the process.
    pub fn shutdown(&self) {
        // Best-effort shutdown
        let _ = self.send_request("shutdown", serde_json::json!({}));
        std::thread::sleep(std::time::Duration::from_secs(1));
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

impl Plugin for ExternalPlugin {
    fn name(&self) -> &str {
        &self.plugin_name
    }

    fn tools(&self) -> Vec<ToolDef> {
        self.tool_defs.clone()
    }

    fn call_tool(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult {
        let params = serde_json::json!({
            "name": tool_name,
            "input": input,
        });

        match self.send_request("tool/execute", params) {
            Ok(result) => {
                match serde_json::from_value::<ExecuteResult>(result) {
                    Ok(exec) => ToolResult {
                        tool_use_id: String::new(),
                        content: exec.content,
                        is_error: exec.is_error,
                    },
                    Err(e) => ToolResult {
                        tool_use_id: String::new(),
                        content: format!("Invalid plugin response: {}", e),
                        is_error: true,
                    },
                }
            }
            Err(e) => ToolResult {
                tool_use_id: String::new(),
                content: format!("Plugin error: {}", e),
                is_error: true,
            },
        }
    }
}

impl Drop for ExternalPlugin {
    fn drop(&mut self) {
        self.shutdown();
    }
}
```

**Step 3: Add load_external_plugins to PluginManager**

```rust
impl PluginManager {
    // ... existing methods ...

    /// Load external plugins from ~/.omnish/plugins/ based on enabled list.
    pub fn load_external_plugins(&mut self, enabled: &[String]) {
        let plugins_dir = omnish_common::config::omnish_dir().join("plugins");
        for name in enabled {
            let executable = plugins_dir.join(name).join(name);
            if !executable.exists() {
                tracing::warn!("Plugin '{}' executable not found at {}",
                    name, executable.display());
                continue;
            }
            if let Some(plugin) = ExternalPlugin::spawn(name, &executable) {
                self.register(Box::new(plugin));
            }
        }
    }
}
```

**Step 4: Build and check**

Run: `cargo build -p omnish-daemon 2>&1 | tail -5`
Expected: Compiles.

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs
git commit -m "feat(plugin): add ExternalPlugin with JSON-RPC subprocess communication"
```

---

### Task 5: Integrate PluginManager into DaemonServer

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs` (DaemonServer struct, handle_chat_message)
- Modify: `crates/omnish-daemon/src/main.rs` (initialization)

**Step 1: Add PluginManager to DaemonServer**

```rust
use omnish_daemon::plugin::PluginManager;

pub struct DaemonServer {
    session_mgr: Arc<SessionManager>,
    llm_backend: Option<Arc<dyn LlmBackend>>,
    task_mgr: Arc<Mutex<TaskManager>>,
    conv_mgr: Arc<ConversationManager>,
    plugin_mgr: Arc<PluginManager>,
}

impl DaemonServer {
    pub fn new(
        session_mgr: Arc<SessionManager>,
        llm_backend: Option<Arc<dyn LlmBackend>>,
        task_mgr: Arc<Mutex<TaskManager>>,
        conv_mgr: Arc<ConversationManager>,
        plugin_mgr: Arc<PluginManager>,
    ) -> Self {
        Self { session_mgr, llm_backend, task_mgr, conv_mgr, plugin_mgr }
    }
}
```

**Step 2: Update handle_chat_message to use PluginManager**

Replace the current tool registration block (lines 248-263):

```rust
// Old:
let (commands, stream_reader) = mgr.get_all_commands_with_reader().await;
let command_query_tool = ...;
let registered_tools: Vec<Box<dyn Tool>> = vec![Box::new(command_query_tool)];
let tools: Vec<omnish_llm::tool::ToolDef> = registered_tools.iter().map(...).collect();

// New:
let tools = plugin_mgr.all_tools();
```

Replace tool execution in the agent loop (lines 339-346):

```rust
// Old:
for tool in registered_tools.iter() {
    if tool.definition().name == tc.name {
        result = tool.execute(&tc.input);
        result.tool_use_id = tc.id.clone();
        break;
    }
}

// New:
let mut result = plugin_mgr.call_tool(&tc.name, &tc.input);
result.tool_use_id = tc.id.clone();
```

Update `handle_chat_message` signature to take `plugin_mgr: &Arc<PluginManager>` instead of building tools locally. The `CommandQueryTool` needs fresh command data per call, so we need to handle this.

**Important consideration:** `CommandQueryTool` is constructed with per-request data (`commands`, `stream_reader`). With PluginManager, we need to reconstruct it each time. Two options:

a) Register a fresh `CommandQueryTool` each request - not ideal since PluginManager is shared
b) Keep `CommandQueryTool` construction in `handle_chat_message` but merge its tools with plugin_mgr tools

Go with **b)** - `handle_chat_message` builds `CommandQueryTool` per-request, gets its definition, and combines with `plugin_mgr.all_tools()`:

```rust
async fn handle_chat_message(
    cm: ChatMessage,
    mgr: &SessionManager,
    llm: &Option<Arc<dyn LlmBackend>>,
    conv_mgr: &Arc<ConversationManager>,
    plugin_mgr: &Arc<PluginManager>,
) -> Vec<Message> {
    // ... backend check ...

    // Build per-request official tool
    let (commands, stream_reader) = mgr.get_all_commands_with_reader().await;
    let command_query_tool = omnish_daemon::tools::command_query::CommandQueryTool::new(
        commands, stream_reader,
    );
    let command_list = command_query_tool.list_history(20);

    // Combine official + plugin tools
    let mut tools = vec![command_query_tool.definition()];
    tools.extend(plugin_mgr.all_tools());

    // ... build LlmRequest with tools ...

    // In agent loop, execute tool:
    // Try official tool first, then plugin manager
    let mut result = if tc.name == "command_query" {
        command_query_tool.execute(&tc.input)
    } else {
        plugin_mgr.call_tool(&tc.name, &tc.input)
    };
    result.tool_use_id = tc.id.clone();
}
```

**Step 3: Update handle_message to pass plugin_mgr**

Find where `handle_chat_message` is called and pass `plugin_mgr`.

**Step 4: Update main.rs to build PluginManager**

```rust
use omnish_daemon::plugin::PluginManager;

// After ConversationManager creation (line 146):
let mut plugin_mgr = PluginManager::new();
plugin_mgr.load_external_plugins(&config.plugins.enabled);
let plugin_mgr = Arc::new(plugin_mgr);

let server = DaemonServer::new(session_mgr, llm_backend, task_mgr, conv_mgr, plugin_mgr);
```

**Step 5: Build and check**

Run: `cargo build -p omnish-daemon 2>&1 | tail -10`
Expected: Compiles.

**Step 6: Run tests**

Run: `cargo test --workspace 2>&1 | grep -E "FAILED|test result"`
Expected: All pass.

**Step 7: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/main.rs
git commit -m "feat(daemon): integrate PluginManager into DaemonServer and chat handler"
```

---

### Task 6: Add tests for PluginManager

**Files:**
- Modify: `crates/omnish-daemon/src/plugin.rs`

**Step 1: Add unit tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct MockPlugin {
        plugin_name: String,
        tool_defs: Vec<ToolDef>,
    }

    impl MockPlugin {
        fn new(name: &str, tools: Vec<(&str, &str)>) -> Self {
            Self {
                plugin_name: name.to_string(),
                tool_defs: tools.into_iter().map(|(n, d)| ToolDef {
                    name: n.to_string(),
                    description: d.to_string(),
                    input_schema: serde_json::json!({"type": "object"}),
                }).collect(),
            }
        }
    }

    impl Plugin for MockPlugin {
        fn name(&self) -> &str { &self.plugin_name }
        fn tools(&self) -> Vec<ToolDef> { self.tool_defs.clone() }
        fn call_tool(&self, tool_name: &str, _input: &serde_json::Value) -> ToolResult {
            ToolResult {
                tool_use_id: String::new(),
                content: format!("mock result from {}", tool_name),
                is_error: false,
            }
        }
    }

    #[test]
    fn test_plugin_manager_empty() {
        let mgr = PluginManager::new();
        assert!(mgr.all_tools().is_empty());
    }

    #[test]
    fn test_plugin_manager_register_and_list_tools() {
        let mut mgr = PluginManager::new();
        mgr.register(Box::new(MockPlugin::new("weather", vec![
            ("get_weather", "Get weather"),
            ("get_forecast", "Get forecast"),
        ])));
        mgr.register(Box::new(MockPlugin::new("calc", vec![
            ("calculate", "Calculate expression"),
        ])));
        let tools = mgr.all_tools();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].name, "get_weather");
        assert_eq!(tools[2].name, "calculate");
    }

    #[test]
    fn test_plugin_manager_execute_routes_correctly() {
        let mut mgr = PluginManager::new();
        mgr.register(Box::new(MockPlugin::new("weather", vec![
            ("get_weather", "Get weather"),
        ])));
        mgr.register(Box::new(MockPlugin::new("calc", vec![
            ("calculate", "Calculate"),
        ])));

        let result = mgr.call_tool("get_weather", &serde_json::json!({}));
        assert!(!result.is_error);
        assert!(result.content.contains("get_weather"));

        let result = mgr.call_tool("calculate", &serde_json::json!({}));
        assert!(!result.is_error);
        assert!(result.content.contains("calculate"));
    }

    #[test]
    fn test_plugin_manager_unknown_tool() {
        let mgr = PluginManager::new();
        let result = mgr.call_tool("nonexistent", &serde_json::json!({}));
        assert!(result.is_error);
        assert!(result.content.contains("Unknown tool"));
    }
}
```

**Step 2: Run tests**

Run: `cargo test -p omnish-daemon -- plugin 2>&1`
Expected: All 4 tests pass.

**Step 3: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs
git commit -m "test(plugin): add PluginManager unit tests"
```

---

### Task 7: Full workspace verification

**Step 1: Build workspace**

Run: `cargo build --workspace 2>&1 | tail -5`
Expected: Clean build.

**Step 2: Run all tests**

Run: `cargo test --workspace 2>&1 | grep -E "FAILED|test result"`
Expected: All pass.

**Step 3: Manual test with no plugins configured**

Start daemon and client, verify chat works normally (no plugins enabled = same behavior as before).

**Step 4: Manual test with a simple plugin**

Create a test plugin:

```bash
mkdir -p ~/.omnish/plugins/echo
cat > ~/.omnish/plugins/echo/echo << 'SCRIPT'
#!/usr/bin/env python3
import json, sys

for line in sys.stdin:
    req = json.loads(line)
    method = req["method"]
    rid = req["id"]

    if method == "initialize":
        resp = {"jsonrpc": "2.0", "id": rid, "result": {
            "name": "echo",
            "tools": [{"name": "echo", "description": "Echo back the input",
                       "input_schema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}]
        }}
    elif method == "tool/execute":
        text = req["params"]["input"].get("text", "")
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"content": f"Echo: {text}", "is_error": False}}
    elif method == "shutdown":
        resp = {"jsonrpc": "2.0", "id": rid, "result": {}}
        print(json.dumps(resp), flush=True)
        break
    else:
        resp = {"jsonrpc": "2.0", "id": rid, "error": {"message": f"Unknown method: {method}"}}

    print(json.dumps(resp), flush=True)
SCRIPT
chmod +x ~/.omnish/plugins/echo/echo
```

Add to daemon.toml:
```toml
[plugins]
enabled = ["echo"]
```

Restart daemon, enter chat, ask "echo hello world" - LLM should call the echo tool.
