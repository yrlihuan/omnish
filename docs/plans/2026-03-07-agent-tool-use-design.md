# Agent Tool-Use Design

## Goal

Let the chat-mode LLM actively query the daemon environment for information using tool-use (function calling), instead of passively receiving pre-built context.

## Architecture

Extend the LLM backend to support Anthropic tool-use. Add a `Tool` trait for defining and executing tools. Implement an agent loop in the daemon that iterates between LLM calls and tool execution until the LLM produces a final answer. Start with a single `command_query` tool; framework is designed to be extensible.

## Constraints

- Only Anthropic backend supports tool-use (others unchanged)
- Maximum 5 tool calls per chat turn
- User sees brief status hints during tool execution
- Non-tool chat and completions are unaffected

---

## 1. Tool Trait (omnish-llm/src/tool.rs)

New file defining the extensible tool interface:

```rust
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDef;
    fn execute(&self, input: &serde_json::Value) -> ToolResult;
}
```

New tools are added by implementing this trait and registering them at startup.

## 2. LLM Backend Changes (omnish-llm/src/backend.rs)

Extend request/response types:

```rust
// LlmRequest: add tools field
pub tools: Vec<ToolDef>,  // empty = no tools

// LlmResponse: replace String content with content blocks
pub enum ContentBlock {
    Text(String),
    ToolUse(ToolCall),
}

pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
}

pub struct LlmResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub model: String,
    pub thinking: Option<String>,
}
```

Add `response.text()` helper to extract concatenated text for non-tool callers.

When `tools` is empty, Anthropic backend omits the `tools` field from the API request (backwards compatible).

## 3. Anthropic Backend (omnish-llm/src/anthropic.rs)

- Include `"tools"` array in request JSON when `req.tools` is non-empty
- Parse `"tool_use"` content blocks (extract id, name, input)
- Parse `"stop_reason"` field into `StopReason` enum

## 4. Protocol Extension (omnish-protocol/src/message.rs)

New message type for tool execution status:

```rust
pub struct ChatToolStatus {
    pub request_id: String,
    pub thread_id: String,
    pub tool_name: String,
    pub status: String,  // human-readable, e.g. "查询命令历史..."
}
```

Added to `Message` enum. Client renders as dim hint line.

## 5. Agent Loop (omnish-daemon/src/server.rs)

Replace single LLM call in `ChatMessage` handler with agent loop:

```
ChatMessage arrives
  -> build LlmRequest with tool definitions
  -> loop (max 5 iterations):
      -> backend.complete()
      -> if stop_reason == EndTurn: extract text, break
      -> if stop_reason == ToolUse:
          -> for each ToolCall:
              -> send ChatToolStatus to client
              -> find tool by name, execute()
              -> collect ToolResult
          -> append tool_use + tool_result to messages
          -> continue loop
  -> send ChatResponse with final text
```

Tools are registered at daemon startup as `Vec<Box<dyn Tool>>`.

## 6. Streaming Chat (client + daemon)

Current `rpc.call()` is request-response. Chat with tools needs streaming:

**Daemon**: writes multiple messages on the same connection (ChatToolStatus..., then ChatResponse).

**Client**: after sending ChatMessage, enters receive loop:
- `ChatToolStatus` -> render dim hint (e.g. "🔧 查询命令历史...")
- `ChatResponse` -> render answer, break loop

The underlying framed protocol already supports sequential message reads. No transport layer changes needed.

## 7. CommandQuery Tool (omnish-daemon/src/tools/)

Single tool with two actions:

**Definition**:
```json
{
  "name": "command_query",
  "description": "Query shell command history and get command output",
  "input_schema": {
    "type": "object",
    "properties": {
      "action": { "type": "string", "enum": ["list_history", "get_output"] },
      "seq": { "type": "integer", "description": "Command sequence number (for get_output)" },
      "count": { "type": "integer", "description": "Number of recent commands (for list_history, default 20)" }
    },
    "required": ["action"]
  }
}
```

**list_history**: returns recent commands with seq, command line, exit code, relative time.

**get_output**: reads full output from stream.bin for the given seq (strip ANSI, cap at 500 lines / 50KB). Uses same `StreamReader` logic as `build_context`.

**Dependencies**: needs `CommandRecord` list + `StreamReader` from SessionManager, passed in at construction.

## 8. Data Flow Example

```
User: "刚才那个编译错误是什么意思"

Client -> ChatMessage
Daemon -> LLM(tools=[command_query])
       <- tool_use: command_query(list_history, count=5)
       -> ChatToolStatus("🔧 查询命令历史...")
       -> execute -> "seq=42 cargo build (exit 1) ..."
       -> LLM(messages + tool_result)
       <- tool_use: command_query(get_output, seq=42)
       -> ChatToolStatus("🔧 获取命令输出 [42]...")
       -> execute -> full cargo build error output
       -> LLM(messages + tool_result)
       <- text: "这个错误是..." (stop_reason=EndTurn)
       -> ChatResponse("这个错误是...")
Client <- ChatToolStatus (render hint)
       <- ChatToolStatus (render hint)
       <- ChatResponse (render answer)
```

## 9. Files Changed

| Module | Change |
|--------|--------|
| omnish-llm/src/tool.rs | New: Tool trait, ToolDef, ToolCall, ToolResult |
| omnish-llm/src/backend.rs | Extend LlmRequest (tools), LlmResponse (ContentBlock, StopReason) |
| omnish-llm/src/anthropic.rs | Send tools, parse tool_use + stop_reason |
| omnish-protocol/src/message.rs | New ChatToolStatus message type |
| omnish-daemon/src/server.rs | Agent loop, tool registration, streaming send |
| omnish-daemon/src/tools/ | New: CommandQueryTool implementation |
| omnish-client/src/main.rs | Receive loop for ChatToolStatus + ChatResponse |

## 10. What Does NOT Change

- Shell completion (auto-complete) — no tools involved
- Non-chat LLM requests (`handle_llm_request`) — unchanged
- Chat with empty tools list — identical to current behavior
- OpenAI-compatible backend — no tool support, falls back to current behavior
- Transport layer — framed protocol already supports sequential messages
