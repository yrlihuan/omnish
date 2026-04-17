# Client-Side Plugin Execution Design

## Problem

The bash tool currently runs on the daemon, but it should run on the client - the daemon may be on a different machine. Plugins need a classification to distinguish daemon-side vs client-side execution.

## Plugin Classification

- `PluginType` enum: `DaemonTool` / `ClientTool`
- `Plugin` trait gains `fn plugin_type(&self) -> PluginType` (default `DaemonTool`)
- Built-in plugins declare in code: `BashTool` → `ClientTool`, `CommandQueryTool` → `DaemonTool`
- External plugins declare in `initialize` response: `"plugin_type": "client_tool"`
- `PluginType` is internal only - not serialized into any wire protocol or LLM tool schema

## Protocol Messages

Two new message variants:

- `ChatToolCall` (daemon → client): `{ request_id, thread_id, tool_name, tool_call_id, input }`
- `ChatToolResult` (client → daemon): `{ request_id, thread_id, tool_call_id, content, is_error }`

## Data Flow

```
Client                              Daemon
  | ChatMessage ------------------>  |
  |                                  | LLM response with tool calls
  |                                  | daemon-side tools: execute directly
  |                                  | client-side tools: pause agent loop
  |                                  | cache AgentLoopState(request_id -> extra_messages)
  | <------------------ ChatToolCall |
  | client PluginManager executes    |
  | ChatToolResult ----------------> |
  |                                  | restore agent loop, continue LLM
  | <---------------- ChatResponse   |
```

## Daemon Side

- `PluginManager` loads ALL plugins; `all_tools()` returns all definitions for LLM
- Agent loop checks `plugin.plugin_type()` when executing tool calls:
  - `DaemonTool` → call `call_tool()` directly
  - `ClientTool` → send `ChatToolCall`, cache state to `HashMap<String, AgentLoopState>`, await
- On receiving `ChatToolResult`: retrieve cached state, resume agent loop

## Client Side

- Client runs its own `PluginManager` with client-side plugins registered (e.g., `BashTool`)
- In `call_stream` receive loop, on `ChatToolCall`:
  - Display status (e.g., "executing: ls -la...")
  - Call local `PluginManager.call_tool()`
  - Send `ChatToolResult` back to daemon via `rpc.call()`
- Continue waiting for subsequent stream messages

## Timeout

- Daemon sets timeout on cached AgentLoopState (60s default)
- On timeout, inject error result into agent loop and continue

## Agent Loop State Cache

```rust
struct AgentLoopState {
    extra_messages: Vec<serde_json::Value>,
    tools: Vec<ToolDef>,
    pending_tool_calls: Vec<ToolCall>,
    completed_results: Vec<ToolResult>,
    iteration: usize,
    cm: ChatMessage,  // original request context
}
```

Keyed by `request_id` in a `HashMap<String, AgentLoopState>` on the daemon.

## External Plugin Support

External plugins declare type in their JSON-RPC `initialize` response:

```json
{
  "name": "my-plugin",
  "plugin_type": "client_tool",
  "tools": [...]
}
```

`ExternalPlugin` stores the declared type and returns it via `plugin_type()`.

Client-side external plugins are loaded by both daemon (for tool definitions) and client (for execution).
