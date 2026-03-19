# Plugin Refactor: Short-lived Processes with JSON Definitions

## Problem

The current plugin system uses long-lived subprocesses with JSON-RPC over stdin/stdout.
This creates complexity (protocol handling, process lifecycle management) and makes
tool cancellation difficult (can't send cancel messages while blocked on execution).

## Design

Separate tool definitions from tool execution:

- **Definitions**: `tool.json` files in plugin directories (single source of truth)
- **Execution**: short-lived process per tool call (stdin JSON → stdout JSON → exit)

### Directory Layout

```
~/.omnish/plugins/
  builtin/
    tool.json                # bash, read, edit, write definitions
  {external-plugin-name}/
    tool.json                # external plugin definitions
    {external-plugin-name}   # executable
```

Source tree mirror (for built-in plugins):

```
crates/omnish-plugin/plugins/
  builtin/
    tool.json
```

### tool.json Format

```json
{
  "plugin_type": "client_tool",
  "tools": [
    {
      "name": "bash",
      "description": "Execute a shell command and return its output...",
      "input_schema": {
        "type": "object",
        "properties": {
          "command": { "type": "string", "description": "The shell command to execute" },
          "shell": { "type": "string", "description": "Shell to use (default: bash)" },
          "cwd": { "type": "string", "description": "Working directory" },
          "timeout": { "type": "number", "description": "Timeout in seconds (default: 30)" }
        },
        "required": ["command"]
      },
      "status_template": "执行: {command}",
      "sandboxed": true
    }
  ]
}
```

Fields:
- `status_template`: uses `{field_name}` interpolation from the tool input
- `sandboxed`: whether Landlock filesystem sandbox is applied (default: true).
  Tools needing write access (edit, write) set this to `false`.

### Execution Protocol

One tool call per process invocation:

- **stdin**: `{"name": "bash", "input": {"command": "ls", "timeout": 30}}\n`
- **stdout**: `{"content": "file1\nfile2", "is_error": false}\n`
- Process exits after writing output.

Built-in tools: `omnish-plugin` binary (no CLI args, dispatches by `name` field in stdin).
External tools: `~/.omnish/plugins/{name}/{name}` executable (same protocol).

### Daemon Changes

`PluginManager` becomes metadata-only. Internal data structure:

```rust
struct PluginInfo {
    dir_name: String,           // "builtin" or external plugin name
    plugin_type: PluginType,    // from tool.json
    tools: Vec<ToolEntry>,      // parsed from tool.json
}

struct ToolEntry {
    def: ToolDef,               // name, description, input_schema (sent to LLM)
    status_template: String,    // for status text interpolation
    sandboxed: bool,            // for Landlock decision
}

struct PluginManager {
    plugins: Vec<PluginInfo>,
    tool_index: HashMap<String, (usize, usize)>,  // tool_name → (plugin_idx, tool_idx)
}
```

Methods:
- `load(plugins_dir)`: scan `*/tool.json`, parse, build index. Log warning and skip
  on malformed JSON, missing files, or duplicate tool names.
- `all_tools()`: return `Vec<ToolDef>` from all plugins
- `tool_status_text(tool_name, input)`: look up `status_template`, interpolate `{field}` with input values
- `tool_plugin_type(tool_name)`: look up `plugin_type`
- `tool_plugin_name(tool_name)`: return `dir_name` for populating `ChatToolCall.plugin_name`
- `tool_sandboxed(tool_name)`: return `sandboxed` flag

No more `ExternalPlugin` struct. No more live process management.
No `call_tool()` on PluginManager — all plugin tool execution goes through
the client (client_tool) or short-lived process spawning at the call site.

`command_query` remains a daemon-side per-request tool (needs live session data),
not a plugin. It keeps its own `status_text()` logic.

### Protocol Changes

`ChatToolCall` message adds two fields:

```rust
pub struct ChatToolCall {
    pub request_id: String,
    pub thread_id: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub input: String,
    pub plugin_name: String,    // NEW: "builtin" or external plugin dir name
    pub sandboxed: bool,        // NEW: whether to apply Landlock sandbox
}
```

### Client Changes

`ClientPluginManager` spawns a fresh process per tool call:

1. Determine executable from `plugin_name`:
   - `"builtin"` → `omnish-plugin` (sibling of client binary)
   - other → `~/.omnish/plugins/{plugin_name}/{plugin_name}`
2. Spawn process with `pre_exec` (Landlock sandbox if `sandboxed` is true)
3. Write JSON input to stdin, close stdin
4. Read JSON output from stdout
5. Process exits naturally

No more process caching (`HashMap<String, PluginProcess>`).

CWD injection: same as current — if `cwd` is available from the shell,
inject it into the tool input before writing to stdin.

### omnish-plugin Binary

Simplified from JSON-RPC loop to single-shot:

```
fn main():
    read one JSON line from stdin
    parse {"name": "...", "input": {...}}
    dispatch to BashTool/ReadTool/EditTool/WriteTool
    write {"content": "...", "is_error": false} to stdout
    exit
```

No CLI arguments needed. Tool structs keep `execute()` method only.
`Tool` trait simplified to just `fn execute(&self, input: &Value) -> ToolResult`.
`definition()` and `status_text()` removed.

### Sandboxing

Same Landlock `pre_exec` approach. Applied each time a process is spawned.
The `sandboxed` field in tool.json controls whether sandbox is applied.
`edit` and `write` set `sandboxed: false` (need full filesystem write access).

### What Gets Removed

- `JsonRpcRequest`, `JsonRpcResponse`, `ExecuteResult` structs
- `PluginProcess` struct (long-lived process management)
- `ExternalPlugin` struct from daemon
- `Plugin` trait (replaced by simplified `Tool` trait with execute-only)
- JSON-RPC methods: `initialize`, `tool/execute`, `tool/status_text`, `shutdown`
- `PROMPT.md` / `PROMPT_*.md` description customization (will be replaced later)
- Tool `definition()` and `status_text()` from Rust code
- In-process fallback when `omnish-plugin` binary is missing (binary is now required)

### What Stays

- Tool execution logic (BashTool::run, ReadTool::execute, etc.)
- Landlock sandbox (`apply_sandbox()`)
- `command_query` tool (daemon-side, per-request, with its own status_text)
- `config.plugins.enabled` for external plugin discovery
