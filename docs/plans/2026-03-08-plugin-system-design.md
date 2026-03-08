# Plugin System Design

## Goal

Add a tool plugin system to omnish, allowing official and user plugins to extend the LLM's available tools through a unified interface.

## Architecture Decisions

1. **Protocol-first** — JSON-RPC 2.0 over stdin/stdout (JSONL). The protocol is the core contract; official plugins are "inlined protocol" implementations.
2. **Long-running process** — Daemon spawns plugin at startup, keeps it running until daemon exit.
3. **Unified interface** — `Plugin` trait abstracts both official (inline Rust) and external (JSON-RPC subprocess) plugins.
4. **Pure function** — Plugins don't access daemon resources. They only process the `input` JSON from the LLM's tool call.
5. **Convention-based discovery** — `~/.omnish/plugins/{name}/{name}` executable.
6. **Config-driven enablement** — `daemon.toml` declares which plugins to load.

## JSON-RPC Protocol

Three methods: `initialize`, `tool/execute`, `shutdown`. Messages are newline-delimited JSON (JSONL) over stdin/stdout.

### Lifecycle

```
daemon                          plugin (subprocess)
  |                                |
  |--- spawn process ------------->|
  |--- stdin: initialize --------->|
  |<-- stdout: tools list ---------|
  |                                |
  |  (LLM requests tool call)      |
  |--- stdin: tool/execute ------->|
  |<-- stdout: result -------------|
  |                                |
  |--- stdin: shutdown ----------->|
  |                         (exit) |
```

### initialize

```jsonc
// daemon → plugin
{"jsonrpc": "2.0", "method": "initialize", "id": 1, "params": {}}

// plugin → daemon
{"jsonrpc": "2.0", "id": 1, "result": {
  "name": "weather",
  "tools": [
    {
      "name": "get_weather",
      "description": "Get current weather for a city",
      "input_schema": {
        "type": "object",
        "properties": {"city": {"type": "string"}},
        "required": ["city"]
      }
    }
  ]
}}
```

### tool/execute

```jsonc
// daemon → plugin
{"jsonrpc": "2.0", "method": "tool/execute", "id": 3, "params": {
  "name": "get_weather",
  "input": {"city": "Shanghai"}
}}

// plugin → daemon
{"jsonrpc": "2.0", "id": 3, "result": {
  "content": "Shanghai: 22°C, partly cloudy",
  "is_error": false
}}
```

### shutdown

```jsonc
// daemon → plugin
{"jsonrpc": "2.0", "method": "shutdown", "id": 2, "params": {}}
```

Plugin should exit cleanly. Daemon waits 1s, then kills if still running.

## Configuration

```toml
# ~/.omnish/daemon.toml

[plugins]
enabled = ["weather", "jira"]
```

Plugin executable path: `~/.omnish/plugins/{name}/{name}`

```
~/.omnish/plugins/
├── weather/
│   └── weather
├── jira/
│   └── jira
```

If a directory or executable doesn't exist, daemon logs a warning and skips.

## Daemon Internal Architecture

### Plugin trait

```rust
trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn tools(&self) -> Vec<ToolDef>;
    fn execute(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult;
}
```

Both official plugins (e.g., `CommandQueryTool`) and external plugins implement this trait.

### PluginHandle (external plugin adapter)

```rust
struct PluginHandle {
    name: String,
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    tools: Vec<ToolDef>,
}
```

Implements `Plugin` by forwarding `execute()` calls as JSON-RPC `tool/execute` messages over stdin/stdout.

### PluginManager

```rust
struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginManager {
    fn all_tools(&self) -> Vec<ToolDef>;
    fn execute(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult;
}
```

- `all_tools()` — collects tool definitions from all plugins (for LLM request)
- `execute()` — finds plugin by tool name, delegates execution

### Startup flow

1. Read `config.plugins.enabled`
2. For each name:
   - Spawn `~/.omnish/plugins/{name}/{name}`
   - Send `initialize`, wait for response (5s timeout)
   - Collect tool definitions
   - Store as `PluginHandle` in `PluginManager`
3. Register official plugins (e.g., `CommandQueryTool`) in same `PluginManager`

### Integration with handle_chat_message

```rust
let tools = plugin_mgr.all_tools();
// ... in agent loop ...
let result = plugin_mgr.execute(&tc.name, &tc.input);
```

### Shutdown flow

1. Send `shutdown` to each external plugin
2. Wait 1s for clean exit
3. Kill remaining processes

### Timeouts

- `initialize`: 5s
- `tool/execute`: 10s
- `shutdown`: 1s

## Files Changed

| File | Change |
|------|--------|
| `crates/omnish-daemon/src/plugin.rs` | New: `Plugin` trait, `PluginManager`, `PluginHandle` |
| `crates/omnish-daemon/src/tools/command_query.rs` | Implement `Plugin` trait (migrate from `Tool`) |
| `crates/omnish-daemon/src/server.rs` | `handle_chat_message` uses `PluginManager` |
| `crates/omnish-daemon/src/main.rs` | Initialize `PluginManager`, load plugins |
| `crates/omnish-common/src/config.rs` | Add `PluginsConfig { enabled: Vec<String> }` |

## Not in scope

- Task plugins (future)
- Plugin resource access API (future)
- Directory auto-scanning
- MCP protocol compatibility (protocol structure leaves room, but not implemented)
