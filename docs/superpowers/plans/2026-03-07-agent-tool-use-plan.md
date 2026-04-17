# Agent Tool-Use Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Enable the chat-mode LLM to actively query the daemon for command history and output using Anthropic tool-use, with an extensible framework for adding more tools later.

**Architecture:** Add a `Tool` trait in omnish-llm, extend `LlmRequest`/`LlmResponse` to support tool definitions and tool_use responses, implement an agent loop in the daemon that iterates between LLM calls and tool execution, stream intermediate `ChatToolStatus` messages to the client, and implement a `command_query` tool as the first concrete tool.

**Tech Stack:** Rust, Anthropic Messages API (tool_use), omnish-llm, omnish-protocol (bincode), omnish-transport (framed RPC), omnish-daemon

---

### Task 1: Add Tool trait and types to omnish-llm

**Files:**
- Create: `crates/omnish-llm/src/tool.rs`
- Modify: `crates/omnish-llm/src/lib.rs`

**Step 1: Create `crates/omnish-llm/src/tool.rs`**

```rust
use serde::{Deserialize, Serialize};

/// Definition of a tool that can be provided to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// Result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

/// Trait for implementing tools that the LLM can call.
/// New tools are added by implementing this trait and registering at startup.
pub trait Tool: Send + Sync {
    /// Returns the tool definition (name, description, JSON schema) for the LLM.
    fn definition(&self) -> ToolDef;
    /// Executes the tool with the given input and returns the result.
    fn execute(&self, input: &serde_json::Value) -> ToolResult;
}
```

**Step 2: Add `pub mod tool;` to `crates/omnish-llm/src/lib.rs`**

Add after line 4 (`pub mod openai_compat;`):

```rust
pub mod tool;
```

**Step 3: Verify build**

Run: `cargo build -p omnish-llm`
Expected: compiles with no errors

**Step 4: Commit**

```bash
git add crates/omnish-llm/src/tool.rs crates/omnish-llm/src/lib.rs
git commit -m "feat(llm): add Tool trait and types for agent tool-use framework"
```

---

### Task 2: Extend LlmRequest and LlmResponse for tool-use

**Files:**
- Modify: `crates/omnish-llm/src/backend.rs`

**Step 1: Add tool-related types and extend LlmRequest/LlmResponse**

Replace the entire contents of `crates/omnish-llm/src/backend.rs` with:

```rust
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::tool::{ToolCall, ToolDef};

/// Use case for LLM requests - determines which model to use
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UseCase {
    /// Auto-completion - fast, lightweight suggestions
    Completion,
    /// Analysis - deeper context understanding
    Analysis,
    /// Chat mode - conversational interaction
    Chat,
}

impl Default for UseCase {
    fn default() -> Self {
        UseCase::Analysis
    }
}

/// A block of content in an LLM response.
#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text(String),
    ToolUse(ToolCall),
}

/// Why the LLM stopped generating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub context: String,
    pub query: Option<String>,
    pub trigger: TriggerType,
    pub session_ids: Vec<String>,
    /// Use case for this request - determines which model to use
    pub use_case: UseCase,
    /// Maximum content characters for context (model-specific limit)
    pub max_content_chars: Option<usize>,
    pub conversation: Vec<omnish_protocol::message::ChatTurn>,
    /// Optional system prompt (e.g., chat mode system prompt).
    pub system_prompt: Option<String>,
    /// Whether to enable extended thinking mode (e.g., Claude extended thinking, DeepSeek R1).
    /// None means use backend default. Set to false to disable, true to enable.
    pub enable_thinking: Option<bool>,
    /// Tool definitions to provide to the LLM. Empty means no tools.
    pub tools: Vec<ToolDef>,
    /// Extra messages for agent loop (tool_use + tool_result exchanges).
    /// These are raw serde_json::Value objects appended after conversation + query.
    pub extra_messages: Vec<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub enum TriggerType {
    Manual,
    AutoError,
    AutoPattern,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub model: String,
    /// Thinking content from models that support it
    pub thinking: Option<String>,
}

impl LlmResponse {
    /// Extract concatenated text from all Text blocks.
    /// Convenience method for callers that don't use tool-use.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Extract all tool calls from the response.
    pub fn tool_calls(&self) -> Vec<&ToolCall> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse(tc) => Some(tc),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
pub trait LlmBackend: Send + Sync {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse>;
    fn name(&self) -> &str;
    /// Returns the maximum content characters limit for this backend's model
    fn max_content_chars(&self) -> Option<usize> {
        None
    }
    /// Returns the maximum content characters limit for the given use case
    fn max_content_chars_for_use_case(&self, _use_case: UseCase) -> Option<usize> {
        self.max_content_chars()
    }
}
```

Note: `extra_messages` is a `Vec<serde_json::Value>` to carry the raw tool_use/tool_result message exchanges during the agent loop. This avoids encoding Anthropic-specific message formats into the trait.

**Step 2: Verify build (will fail - callers need updating)**

Run: `cargo build -p omnish-llm 2>&1 | head -30`
Expected: compilation errors in `anthropic.rs` and `openai_compat.rs` - these will be fixed in the next tasks.

---

### Task 3: Update Anthropic backend for tool-use

**Files:**
- Modify: `crates/omnish-llm/src/anthropic.rs`

**Step 1: Replace `crates/omnish-llm/src/anthropic.rs` with tool-use support**

```rust
use crate::backend::{ContentBlock, LlmBackend, LlmRequest, LlmResponse, StopReason};
use crate::tool::ToolCall;
use anyhow::Result;
use async_trait::async_trait;

pub struct AnthropicBackend {
    pub model: String,
    pub api_key: String,
    pub client: reqwest::Client,
}

/// Strip thinking tags from LLM response content.
fn strip_thinking(content: &str) -> String {
    content.replace("\n<think>", "").replace("</think>", "")
}

#[async_trait]
impl LlmBackend for AnthropicBackend {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let client = &self.client;

        let messages: Vec<serde_json::Value> = if req.conversation.is_empty() && req.extra_messages.is_empty() {
            // Existing single-turn behavior
            let user_content = crate::template::build_user_content(
                &req.context,
                req.query.as_deref(),
            );
            vec![serde_json::json!({"role": "user", "content": user_content})]
        } else {
            // Multi-turn: conversation history + current query + extra (tool) messages
            let mut msgs = Vec::new();
            for (i, turn) in req.conversation.iter().enumerate() {
                let content = if i == 0 && !req.context.is_empty() {
                    // Prepend terminal context to first user message
                    format!("Terminal context:\n{}\n\n{}", req.context, turn.content)
                } else {
                    turn.content.clone()
                };
                msgs.push(serde_json::json!({"role": &turn.role, "content": content}));
            }
            // Append current query as user message (before extra messages on first call)
            if req.extra_messages.is_empty() {
                if let Some(ref q) = req.query {
                    msgs.push(serde_json::json!({"role": "user", "content": q}));
                }
            }
            // Append extra messages (tool_use assistant + tool_result user exchanges)
            msgs.extend(req.extra_messages.clone());
            msgs
        };

        // Build request body
        let mut body_map = serde_json::Map::new();
        body_map.insert("model".to_string(), serde_json::Value::String(self.model.clone()));
        body_map.insert("max_tokens".to_string(), serde_json::Value::Number(4096.into()));
        body_map.insert("messages".to_string(), serde_json::Value::Array(messages));

        // Add system prompt if provided
        if let Some(ref system) = req.system_prompt {
            body_map.insert("system".to_string(), serde_json::Value::String(system.clone()));
        }

        // Add tools if provided
        if !req.tools.is_empty() {
            let tools_json: Vec<serde_json::Value> = req.tools
                .iter()
                .map(|t| serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                }))
                .collect();
            body_map.insert("tools".to_string(), serde_json::Value::Array(tools_json));
        }

        // Add thinking parameter if explicitly disabled
        if req.enable_thinking == Some(false) {
            let mut thinking_map = serde_json::Map::new();
            thinking_map.insert("type".to_string(), serde_json::Value::String("enabled".to_string()));
            thinking_map.insert("disabled_reason".to_string(), serde_json::Value::String("disabled_by_client".to_string()));
            body_map.insert("thinking".to_string(), serde_json::Value::Object(thinking_map));
        }

        let body = serde_json::Value::Object(body_map);

        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let json: serde_json::Value = resp.json().await?;

        // Check for API errors
        if !status.is_success() {
            let error_msg = json["error"]["message"]
                .as_str()
                .unwrap_or("Unknown API error");
            let error_type = json["error"]["type"]
                .as_str()
                .unwrap_or("unknown");
            return Err(anyhow::anyhow!(
                "Anthropic API error ({}): {} - {}",
                status,
                error_type,
                error_msg
            ));
        }

        // Parse stop_reason
        let stop_reason = match json["stop_reason"].as_str() {
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::MaxTokens,
            _ => StopReason::EndTurn,
        };

        // Extract content blocks
        let mut thinking: Option<String> = None;
        let mut content_blocks = Vec::new();

        for block in json["content"].as_array().unwrap_or(&vec![]) {
            match block["type"].as_str() {
                Some("thinking") => {
                    thinking = block["thinking"].as_str().map(|s| s.to_string());
                }
                Some("text") => {
                    let text = strip_thinking(block["text"].as_str().unwrap_or(""));
                    if !text.is_empty() {
                        content_blocks.push(ContentBlock::Text(text));
                    }
                }
                Some("tool_use") => {
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    let name = block["name"].as_str().unwrap_or("").to_string();
                    let input = block["input"].clone();
                    content_blocks.push(ContentBlock::ToolUse(ToolCall { id, name, input }));
                }
                _ => {}
            }
        }

        if content_blocks.is_empty() && stop_reason == StopReason::EndTurn {
            return Err(anyhow::anyhow!("Invalid response format: no content blocks found"));
        }

        Ok(LlmResponse {
            content: content_blocks,
            stop_reason,
            model: self.model.clone(),
            thinking,
        })
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}
```

**Key changes from the original:**
- `max_tokens` increased from 1024 to 4096 (agent responses may be longer)
- Added `tools` array in request JSON when non-empty
- Parses `tool_use` content blocks (id, name, input)
- Parses `stop_reason` into `StopReason` enum
- Returns `Vec<ContentBlock>` instead of a single string
- Handles `extra_messages` for agent loop continuation

**Step 2: Verify build**

Run: `cargo build -p omnish-llm 2>&1 | head -20`
Expected: `omnish-llm` itself may compile, but downstream crates (daemon, client) will fail because they use the old `LlmResponse` fields. That's expected.

---

### Task 4: Update OpenAI-compatible backend for new response type

**Files:**
- Modify: `crates/omnish-llm/src/openai_compat.rs`

**Step 1: Update return type to use new LlmResponse**

The OpenAI backend does NOT support tools. Update it to return the new `LlmResponse` format with `content: Vec<ContentBlock>` and `stop_reason: StopReason::EndTurn`, and add `tools: Vec<ToolDef>` (ignored) and `extra_messages: Vec<serde_json::Value>` (ignored) to the request handling.

Change the `Ok(LlmResponse { ... })` at the end of `complete()` (around line 116-120) from:

```rust
Ok(LlmResponse {
    content,
    model: self.model.clone(),
    thinking,
})
```

to:

```rust
Ok(LlmResponse {
    content: vec![ContentBlock::Text(content)],
    stop_reason: StopReason::EndTurn,
    model: self.model.clone(),
    thinking,
})
```

And update the import at line 1 from:

```rust
use crate::backend::{LlmBackend, LlmRequest, LlmResponse};
```

to:

```rust
use crate::backend::{ContentBlock, LlmBackend, LlmRequest, LlmResponse, StopReason};
```

**Step 2: Verify omnish-llm builds**

Run: `cargo build -p omnish-llm`
Expected: compiles with no errors

**Step 3: Commit Tasks 1-4 together**

```bash
git add crates/omnish-llm/
git commit -m "feat(llm): add tool-use framework with Tool trait and ContentBlock response"
```

---

### Task 5: Update all LlmResponse callers in daemon and client

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`
- Modify: `crates/omnish-client/src/main.rs` (if it references `LlmResponse` directly)

Everywhere the old `response.content` (String) is used, change to `response.text()`.

**Step 1: Update `crates/omnish-daemon/src/server.rs`**

In `handle_message` for `Message::ChatMessage` (around line 244-248), change:

```rust
match backend.complete(&llm_req).await {
    Ok(response) => {
        tracing::info!("Chat LLM completed in {:?} (thread={})", start.elapsed(), cm.thread_id);
        conv_mgr.append_exchange(&cm.thread_id, &cm.query, &response.content);
        response.content
    }
```

to:

```rust
match backend.complete(&llm_req).await {
    Ok(response) => {
        tracing::info!("Chat LLM completed in {:?} (thread={})", start.elapsed(), cm.thread_id);
        let text = response.text();
        conv_mgr.append_exchange(&cm.thread_id, &cm.query, &text);
        text
    }
```

Also update the `LlmRequest` construction at line 231-241 to include the new fields:

```rust
let llm_req = LlmRequest {
    context,
    query: Some(cm.query.clone()),
    trigger: TriggerType::Manual,
    session_ids: vec![cm.session_id.clone()],
    use_case,
    max_content_chars: max_context_chars,
    conversation,
    system_prompt: Some(omnish_llm::template::CHAT_SYSTEM_PROMPT.to_string()),
    enable_thinking: None,
    tools: vec![],
    extra_messages: vec![],
};
```

Do the same for ALL other `LlmRequest` constructions in the file (search for `LlmRequest {`). There are at least:
- `handle_llm_request` (~line 666)
- `try_warmup_kv_cache` (~line 274)
- `handle_completion_request` (~line 709)

For each, add `tools: vec![]` and `extra_messages: vec![]`.

For ALL `response.content` usages where a String is expected, change to `response.text()`.

Also in `handle_llm_request` (~line 684-694) update thinking logging:

```rust
if let Some(ref thinking) = response.thinking {
```

This doesn't change since `thinking` is still `Option<String>`.

**Step 2: Update `crates/omnish-client/src/main.rs`**

Search for any direct use of `LlmResponse` or `response.content`. The client doesn't use LlmResponse directly - it communicates via protocol messages. No changes expected here for this task.

**Step 3: Verify full workspace build**

Run: `cargo build`
Expected: all crates compile

**Step 4: Run all tests**

Run: `cargo test`
Expected: all tests pass

**Step 5: Commit**

```bash
git add crates/omnish-daemon/ crates/omnish-client/
git commit -m "refactor: update all callers for new LlmResponse ContentBlock format"
```

---

### Task 6: Add ChatToolStatus message to protocol

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs`

**Step 1: Add ChatToolStatus struct** (after ChatInterrupt, around line 205)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolStatus {
    pub request_id: String,
    pub thread_id: String,
    pub tool_name: String,
    pub status: String,
}
```

**Step 2: Add to Message enum** (after `ChatInterrupt(ChatInterrupt)`, around line 25)

```rust
ChatToolStatus(ChatToolStatus),
```

**Step 3: Add test for round-trip serialization**

Add to the `tests` module:

```rust
#[test]
fn test_frame_with_chat_tool_status() {
    let frame = Frame {
        request_id: 40,
        payload: Message::ChatToolStatus(ChatToolStatus {
            request_id: "req1".to_string(),
            thread_id: "thread1".to_string(),
            tool_name: "command_query".to_string(),
            status: "查询命令历史...".to_string(),
        }),
    };
    let bytes = frame.to_bytes().unwrap();
    let decoded = Frame::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.request_id, 40);
    if let Message::ChatToolStatus(cts) = decoded.payload {
        assert_eq!(cts.tool_name, "command_query");
        assert_eq!(cts.status, "查询命令历史...");
    } else {
        panic!("expected ChatToolStatus");
    }
}
```

**Step 4: Verify build and test**

Run: `cargo test -p omnish-protocol`
Expected: all tests pass including new one

**Step 5: Commit**

```bash
git add crates/omnish-protocol/
git commit -m "feat(protocol): add ChatToolStatus message type for agent tool-use"
```

---

### Task 7: Add streaming support to RPC transport

**Files:**
- Modify: `crates/omnish-transport/src/rpc_server.rs`
- Modify: `crates/omnish-transport/src/rpc_client.rs`

**Step 1: Add streaming handler support to RPC server**

The current server handler signature is `Fn(Message) -> Future<Output = Message>` (returns one message). For the agent loop, the daemon needs to send multiple messages (ChatToolStatus + final ChatResponse) for a single incoming ChatMessage.

Add a new `serve_with_stream` method to `RpcServer`, or modify the existing `serve`. The simplest approach: change the handler to return `Vec<Message>` instead of `Message`. The server writes all of them with the same `request_id`.

In `crates/omnish-transport/src/rpc_server.rs`, change the handler type in `serve` from:

```rust
F: Fn(Message) -> Pin<Box<dyn Future<Output = Message> + Send>> + Send + Sync + 'static,
```

to:

```rust
F: Fn(Message) -> Pin<Box<dyn Future<Output = Vec<Message>> + Send>> + Send + Sync + 'static,
```

In `spawn_connection` (line 209-214), change:

```rust
tokio::spawn(async move {
    let response_payload = handler(frame.payload).await;
    if let Err(e) = write_reply(&writer, frame.request_id, response_payload).await {
        tracing::error!("failed to write response: {}", e);
    }
});
```

to:

```rust
tokio::spawn(async move {
    let responses = handler(frame.payload).await;
    for response_payload in responses {
        if let Err(e) = write_reply(&writer, frame.request_id, response_payload).await {
            tracing::error!("failed to write response: {}", e);
            break;
        }
    }
});
```

**Step 2: Add `call_stream` to RPC client**

In `crates/omnish-transport/src/rpc_client.rs`, the current `call()` uses `oneshot::channel` (single response). Add a new `call_stream()` that returns an `mpsc::Receiver<Message>`:

```rust
/// Send a message and receive multiple responses (for streaming).
/// The receiver yields messages until the sender is dropped (connection closes or
/// the server stops sending for this request_id).
pub async fn call_stream(&self, msg: Message) -> Result<mpsc::Receiver<Message>> {
    let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let frame = Frame {
        request_id,
        payload: msg,
    };
    let (reply_tx, reply_rx) = mpsc::channel(16);

    let inner = self.inner.lock().await;
    if !inner.connected.load(Ordering::SeqCst) {
        return Err(anyhow::anyhow!("not connected"));
    }
    inner
        .tx
        .send(WriteRequest { frame, reply_tx: ReplyTx::Stream(reply_tx) })
        .await
        .map_err(|_| anyhow::anyhow!("write task closed"))?;
    drop(inner);

    Ok(reply_rx)
}
```

To support both `call()` (oneshot) and `call_stream()` (mpsc), change `WriteRequest` to use an enum for the reply sender:

```rust
enum ReplyTx {
    Once(oneshot::Sender<Message>),
    Stream(mpsc::Sender<Message>),
}

struct WriteRequest {
    frame: Frame,
    reply_tx: ReplyTx,
}
```

Update `call()` to use `ReplyTx::Once(reply_tx)`.

In `read_loop`, change the pending map from `HashMap<u64, oneshot::Sender<Message>>` to `HashMap<u64, ReplyTx>`. When a frame arrives:

```rust
if let Some(tx) = map.get(&frame.request_id) {
    match tx {
        ReplyTx::Once(_) => {
            // Remove and send (oneshot consumed)
            if let Some(ReplyTx::Once(tx)) = map.remove(&frame.request_id) {
                let _ = tx.send(frame.payload);
            }
        }
        ReplyTx::Stream(tx) => {
            // Keep in map, send via mpsc. Remove if send fails (receiver dropped).
            if tx.try_send(frame.payload).is_err() {
                map.remove(&frame.request_id);
            }
        }
    }
}
```

The stream receiver is closed when the client drops the `mpsc::Receiver`. The server signals "end of stream" by sending the final `ChatResponse` - the client receives it and stops reading.

**Step 3: Update `handle_message` in daemon to return `Vec<Message>`**

In `crates/omnish-daemon/src/server.rs`, change `handle_message` return type:

```rust
async fn handle_message(...) -> Vec<Message> {
```

For all existing branches, wrap the single return value in `vec![...]`:

```rust
// Example: Message::Ack becomes:
vec![Message::Ack]

// Example: Message::ChatResponse(...) becomes:
vec![Message::ChatResponse(...)]
```

Update the closure in `run()` accordingly:

```rust
Box::pin(async move { handle_message(msg, mgr, &llm, &task_mgr, &conv_mgr).await })
```

**Step 4: Verify build and all tests pass**

Run: `cargo test`
Expected: all tests pass

**Step 5: Commit**

```bash
git add crates/omnish-transport/ crates/omnish-daemon/src/server.rs
git commit -m "feat(transport): support streaming multi-message responses for agent loop"
```

---

### Task 8: Implement CommandQueryTool

**Files:**
- Create: `crates/omnish-daemon/src/tools/mod.rs`
- Create: `crates/omnish-daemon/src/tools/command_query.rs`
- Modify: `crates/omnish-daemon/src/lib.rs`

**Step 1: Create `crates/omnish-daemon/src/tools/mod.rs`**

```rust
pub mod command_query;
```

**Step 2: Create `crates/omnish-daemon/src/tools/command_query.rs`**

```rust
use omnish_context::StreamReader;
use omnish_llm::tool::{Tool, ToolCall, ToolDef, ToolResult};
use omnish_store::command::CommandRecord;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Maximum lines to return from get_output to prevent huge responses.
const MAX_OUTPUT_LINES: usize = 500;
/// Maximum bytes to return from get_output.
const MAX_OUTPUT_BYTES: usize = 50_000;

pub struct CommandQueryTool {
    commands: Arc<RwLock<Vec<CommandRecord>>>,
    stream_reader: Arc<dyn StreamReader>,
}

impl CommandQueryTool {
    pub fn new(
        commands: Arc<RwLock<Vec<CommandRecord>>>,
        stream_reader: Arc<dyn StreamReader>,
    ) -> Self {
        Self { commands, stream_reader }
    }

    fn list_history(&self, count: usize) -> String {
        let commands = self.commands.blocking_read();
        if commands.is_empty() {
            return "No commands in history.".to_string();
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let start = commands.len().saturating_sub(count);
        let mut lines = Vec::new();
        for (i, cmd) in commands[start..].iter().enumerate() {
            let seq = start + i + 1; // 1-based
            let cmd_line = cmd.command_line.as_deref().unwrap_or("(unknown)");
            let exit = cmd.exit_code.map(|c| format!("exit {}", c)).unwrap_or_default();
            let ago = format_ago(now_ms, cmd.started_at);
            lines.push(format!("[seq={}] {}  ({}, {})", seq, cmd_line, exit, ago));
        }
        lines.join("\n")
    }

    fn get_output(&self, seq: usize) -> String {
        let commands = self.commands.blocking_read();
        if seq == 0 || seq > commands.len() {
            return format!("Error: seq {} out of range (1-{})", seq, commands.len());
        }
        let cmd = &commands[seq - 1];
        if cmd.stream_length == 0 {
            return "(no output recorded)".to_string();
        }
        match self.stream_reader.read_command_output(cmd.stream_offset, cmd.stream_length) {
            Ok(entries) => {
                let mut raw = Vec::new();
                for entry in &entries {
                    if entry.direction == 1 { // Output direction
                        raw.extend_from_slice(&entry.data);
                    }
                }
                let text = omnish_context::format_utils::strip_ansi_codes(&raw);
                // Skip first line (echoed command)
                let text = match text.find('\n') {
                    Some(pos) => text[pos + 1..].trim_start().to_string(),
                    None => text,
                };
                // Truncate by lines and bytes
                let mut result = String::new();
                let mut line_count = 0;
                for line in text.lines() {
                    if line_count >= MAX_OUTPUT_LINES || result.len() + line.len() > MAX_OUTPUT_BYTES {
                        result.push_str(&format!("\n... (truncated, {} total lines)", text.lines().count()));
                        break;
                    }
                    if line_count > 0 { result.push('\n'); }
                    result.push_str(line);
                    line_count += 1;
                }
                result
            }
            Err(e) => format!("Error reading output: {}", e),
        }
    }
}

impl Tool for CommandQueryTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: "command_query".to_string(),
            description: "Query shell command history and get full command output. Use list_history first to see available commands, then get_output with a seq number to see the full output of a specific command.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list_history", "get_output"],
                        "description": "Action to perform"
                    },
                    "seq": {
                        "type": "integer",
                        "description": "Command sequence number (required for get_output, obtained from list_history)"
                    },
                    "count": {
                        "type": "integer",
                        "description": "Number of recent commands to list (default 20, only for list_history)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, input: &serde_json::Value) -> ToolResult {
        let action = input["action"].as_str().unwrap_or("");
        let tool_use_id = String::new(); // Filled by caller

        match action {
            "list_history" => {
                let count = input["count"].as_u64().unwrap_or(20) as usize;
                let content = self.list_history(count);
                ToolResult { tool_use_id, content, is_error: false }
            }
            "get_output" => {
                let seq = input["seq"].as_u64().unwrap_or(0) as usize;
                if seq == 0 {
                    return ToolResult {
                        tool_use_id,
                        content: "Error: 'seq' is required for get_output".to_string(),
                        is_error: true,
                    };
                }
                let content = self.get_output(seq);
                ToolResult { tool_use_id, content, is_error: false }
            }
            _ => ToolResult {
                tool_use_id,
                content: format!("Error: unknown action '{}'", action),
                is_error: true,
            },
        }
    }
}

fn format_ago(now_ms: u64, started_at: u64) -> String {
    let diff_s = now_ms.saturating_sub(started_at) / 1000;
    if diff_s < 60 { format!("{}s ago", diff_s) }
    else if diff_s < 3600 { format!("{}m ago", diff_s / 60) }
    else if diff_s < 86400 { format!("{}h ago", diff_s / 3600) }
    else { format!("{}d ago", diff_s / 86400) }
}
```

Note: this code uses `blocking_read()` on the `RwLock` because the `Tool::execute` trait method is synchronous. The commands lock is held briefly (just cloning/reading). If this becomes a bottleneck later, we can make `execute` async.

The `strip_ansi_codes` function may need to be checked - it might be named differently in `omnish_context::format_utils`. Check with:

Run: `grep -n 'pub fn strip_ansi\|pub fn strip' crates/omnish-context/src/format_utils.rs`

Adjust the function name if different. Also check if `entry.direction == 1` matches the output direction - verify against `omnish_store::stream::StreamEntry`.

**Step 3: Add `pub mod tools;` to `crates/omnish-daemon/src/lib.rs`**

**Step 4: Verify build**

Run: `cargo build -p omnish-daemon`

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/tools/ crates/omnish-daemon/src/lib.rs
git commit -m "feat(daemon): implement CommandQueryTool for agent tool-use"
```

---

### Task 9: Implement agent loop in daemon chat handler

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

**Step 1: Modify ChatMessage handler to support agent loop**

Replace the `Message::ChatMessage(cm) => { ... }` block in `handle_message` (lines 212-264) with the agent loop version. The function now returns `Vec<Message>` (from Task 7).

```rust
Message::ChatMessage(cm) => {
    if let Some(ref backend) = llm {
        let conversation = conv_mgr.load_messages(&cm.thread_id);
        let use_case = UseCase::Chat;
        let max_context_chars = backend.max_content_chars_for_use_case(use_case);

        let context = if conversation.is_empty() {
            let dummy_req = Request {
                request_id: cm.request_id.clone(),
                session_id: cm.session_id.clone(),
                query: String::new(),
                scope: RequestScope::AllSessions,
            };
            resolve_chat_context(&dummy_req, mgr, max_context_chars).await.unwrap_or_default()
        } else {
            String::new()
        };

        // Collect tool definitions
        let tools: Vec<omnish_llm::tool::ToolDef> = registered_tools
            .iter()
            .map(|t| t.definition())
            .collect();

        let mut llm_req = LlmRequest {
            context,
            query: Some(cm.query.clone()),
            trigger: TriggerType::Manual,
            session_ids: vec![cm.session_id.clone()],
            use_case,
            max_content_chars: max_context_chars,
            conversation,
            system_prompt: Some(omnish_llm::template::CHAT_SYSTEM_PROMPT.to_string()),
            enable_thinking: None,
            tools,
            extra_messages: vec![],
        };

        let mut messages = Vec::new(); // Accumulated response messages
        let max_iterations = 5;

        let start = std::time::Instant::now();
        for iteration in 0..max_iterations {
            match backend.complete(&llm_req).await {
                Ok(response) => {
                    if response.stop_reason == StopReason::ToolUse {
                        let tool_calls = response.tool_calls();
                        if tool_calls.is_empty() {
                            break;
                        }

                        // Build assistant message with tool_use blocks
                        let assistant_content: Vec<serde_json::Value> = response.content.iter().map(|b| {
                            match b {
                                ContentBlock::Text(t) => serde_json::json!({"type": "text", "text": t}),
                                ContentBlock::ToolUse(tc) => serde_json::json!({
                                    "type": "tool_use",
                                    "id": tc.id,
                                    "name": tc.name,
                                    "input": tc.input,
                                }),
                            }
                        }).collect();
                        llm_req.extra_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": assistant_content,
                        }));

                        // Execute each tool call
                        let mut tool_results = Vec::new();
                        for tc in &tool_calls {
                            // Send status to client
                            let status_text = match tc.name.as_str() {
                                "command_query" => {
                                    match tc.input["action"].as_str() {
                                        Some("list_history") => "查询命令历史...".to_string(),
                                        Some("get_output") => format!("获取命令输出 [{}]...",
                                            tc.input["seq"].as_u64().unwrap_or(0)),
                                        _ => format!("执行 {}...", tc.name),
                                    }
                                }
                                _ => format!("执行 {}...", tc.name),
                            };
                            messages.push(Message::ChatToolStatus(ChatToolStatus {
                                request_id: cm.request_id.clone(),
                                thread_id: cm.thread_id.clone(),
                                tool_name: tc.name.clone(),
                                status: status_text,
                            }));

                            // Find and execute tool
                            let mut result = omnish_llm::tool::ToolResult {
                                tool_use_id: tc.id.clone(),
                                content: format!("Unknown tool: {}", tc.name),
                                is_error: true,
                            };
                            for tool in registered_tools.iter() {
                                if tool.definition().name == tc.name {
                                    result = tool.execute(&tc.input);
                                    result.tool_use_id = tc.id.clone();
                                    break;
                                }
                            }
                            tool_results.push(result);
                        }

                        // Build user message with tool_result blocks
                        let result_content: Vec<serde_json::Value> = tool_results.iter().map(|r| {
                            serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": r.tool_use_id,
                                "content": r.content,
                                "is_error": r.is_error,
                            })
                        }).collect();
                        llm_req.extra_messages.push(serde_json::json!({
                            "role": "user",
                            "content": result_content,
                        }));

                        // Continue loop for next LLM call
                        continue;
                    }

                    // EndTurn or MaxTokens - extract final text
                    let text = response.text();
                    tracing::info!(
                        "Chat LLM completed in {:?} ({} tool iterations, thread={})",
                        start.elapsed(), iteration, cm.thread_id
                    );
                    conv_mgr.append_exchange(&cm.thread_id, &cm.query, &text);
                    messages.push(Message::ChatResponse(ChatResponse {
                        request_id: cm.request_id.clone(),
                        thread_id: cm.thread_id.clone(),
                        content: text,
                    }));
                    return messages;
                }
                Err(e) => {
                    tracing::error!("Chat LLM failed: {}", e);
                    messages.push(Message::ChatResponse(ChatResponse {
                        request_id: cm.request_id.clone(),
                        thread_id: cm.thread_id.clone(),
                        content: format!("Error: {}", e),
                    }));
                    return messages;
                }
            }
        }

        // Exhausted iterations
        tracing::warn!("Agent loop exhausted {} iterations (thread={})", max_iterations, cm.thread_id);
        let text = "(Agent reached maximum tool call limit)".to_string();
        conv_mgr.append_exchange(&cm.thread_id, &cm.query, &text);
        messages.push(Message::ChatResponse(ChatResponse {
            request_id: cm.request_id,
            thread_id: cm.thread_id,
            content: text,
        }));
        messages
    } else {
        vec![Message::ChatResponse(ChatResponse {
            request_id: cm.request_id,
            thread_id: cm.thread_id,
            content: "(LLM backend not configured)".to_string(),
        })]
    }
}
```

**Step 2: Pass `registered_tools` into `handle_message`**

Add `registered_tools: &[Box<dyn Tool>]` parameter to `handle_message`. Update the `DaemonServer` struct to hold tools, and pass them through `serve`.

In `DaemonServer::new()`, add `tools: Vec<Box<dyn Tool>>` parameter.

In `run()`, pass `&tools` into the closure.

**Step 3: Register CommandQueryTool at startup**

In `DaemonServer::new()` or wherever the daemon is initialized (check `crates/omnish-daemon/src/main.rs` or similar), create and register the `CommandQueryTool`:

```rust
let tools: Vec<Box<dyn omnish_llm::tool::Tool>> = vec![
    Box::new(CommandQueryTool::new(commands_arc, stream_reader_arc)),
];
```

The exact construction depends on how commands and stream_reader are accessed. The commands for the current session's `CommandRecord` list and the `FileStreamReader` need to be accessible. This may require adjusting `CommandQueryTool` to take a `SessionManager` Arc instead, since it needs to access the active session's data.

**Step 4: Import new types**

Add to the imports in `server.rs`:

```rust
use omnish_llm::backend::{ContentBlock, StopReason};
use omnish_protocol::message::ChatToolStatus;
```

**Step 5: Verify build and tests**

Run: `cargo build && cargo test`

**Step 6: Commit**

```bash
git add crates/omnish-daemon/
git commit -m "feat(daemon): implement agent loop with tool execution and streaming status"
```

---

### Task 10: Handle ChatToolStatus in client

**Files:**
- Modify: `crates/omnish-client/src/main.rs`

**Step 1: Change `rpc.call()` to `rpc.call_stream()` for ChatMessage**

In `run_chat_loop` (around line 1994-2021), replace:

```rust
let rpc_result = rpc.call(chat_msg);
```

with:

```rust
let rpc_result = rpc.call_stream(chat_msg);
```

**Step 2: Update the response handling**

Replace the `tokio::select!` response handling to read from the stream receiver:

```rust
tokio::select! {
    result = rpc_result => {
        let _ = stop_tx.send(());
        match result {
            Ok(mut rx) => {
                while let Some(msg) = rx.recv().await {
                    match msg {
                        Message::ChatToolStatus(cts) => {
                            // Display tool status hint
                            let hint = format!("\r\x1b[2m\u{1f527} {}\x1b[0m\x1b[K\r\n", cts.status);
                            nix::unistd::write(std::io::stdout(), hint.as_bytes()).ok();
                        }
                        Message::ChatResponse(resp) if resp.request_id == req_id => {
                            // Clear thinking indicator
                            nix::unistd::write(std::io::stdout(), b"\r\x1b[K").ok();
                            let output = display::render_response(&resp.content);
                            nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();

                            let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
                            let separator = display::render_separator(cols);
                            let sep_line = format!("{}\r\n", separator);
                            nix::unistd::write(std::io::stdout(), sep_line.as_bytes()).ok();
                            break;
                        }
                        _ => break,
                    }
                }
            }
            Err(_) => {
                nix::unistd::write(std::io::stdout(), b"\r\x1b[K").ok();
                let err = display::render_error("Failed to receive chat response");
                nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
            }
        }
    }
    _ = interrupt => {
        // Ctrl-C handling stays the same
```

**Step 3: Verify build**

Run: `cargo build`

**Step 4: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat(client): handle ChatToolStatus messages with tool hint display"
```

---

### Task 11: End-to-end verification

**Step 1: Build release**

Run: `cargo build --release`

**Step 2: Run all unit tests**

Run: `cargo test`
Expected: all tests pass

**Step 3: Manual test**

1. Start omnish daemon and client
2. Execute a few shell commands (e.g., `ls`, `cargo build`, `git status`)
3. Enter chat mode with `:`
4. Ask: "刚才执行了哪些命令"
5. Verify: LLM uses `command_query(list_history)` tool, you see `🔧 查询命令历史...` hint, then the answer
6. Ask: "上一个命令的输出是什么"
7. Verify: LLM uses `command_query(get_output, seq=N)`, you see `🔧 获取命令输出 [N]...` hint, then the full output

**Step 4: Final commit if any fixes needed**

```bash
git commit -m "fix: adjustments from end-to-end testing"
```
