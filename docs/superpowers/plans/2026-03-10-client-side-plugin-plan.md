# Client-Side Plugin Execution — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Allow plugins to execute on the client side, enabling the bash tool to run commands in the user's local environment rather than on the daemon host.

**Architecture:** Add `PluginType` classification to `Plugin` trait. During the agent loop, daemon-side tools execute directly; client-side tools pause the loop, send `ChatToolCall` to the client, and resume when `ChatToolResult` arrives. State is cached on the daemon keyed by `request_id`.

**Tech Stack:** Rust, omnish protocol (bincode), tokio async, serde

---

### Task 1: Add PluginType enum and update Plugin trait

**Files:**
- Modify: `crates/omnish-daemon/src/plugin.rs:1-15`

**Step 1: Add PluginType and update trait**

In `crates/omnish-daemon/src/plugin.rs`, add before the Plugin trait:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    DaemonTool,
    ClientTool,
}
```

Add method to Plugin trait with default:

```rust
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn plugin_type(&self) -> PluginType { PluginType::DaemonTool }
    fn tools(&self) -> Vec<ToolDef>;
    fn call_tool(&self, tool_name: &str, input: &serde_json::Value) -> ToolResult;
}
```

**Step 2: Mark BashTool as ClientTool**

In `crates/omnish-daemon/src/tools/bash.rs`, add to `impl Plugin for BashTool`:

```rust
fn plugin_type(&self) -> crate::plugin::PluginType {
    crate::plugin::PluginType::ClientTool
}
```

**Step 3: Add plugin_type to ExternalPlugin**

In `crates/omnish-daemon/src/plugin.rs`, update `InitializeResult`:

```rust
#[derive(Deserialize)]
struct InitializeResult {
    #[allow(dead_code)]
    name: String,
    #[serde(default)]
    plugin_type: Option<String>,
    tools: Vec<ToolDef>,
}
```

In `ExternalPlugin` struct, add field `plugin_type: PluginType`.

In `ExternalPlugin::spawn`, after parsing `InitializeResult`, resolve the type:

```rust
let ptype = match init.plugin_type.as_deref() {
    Some("client_tool") => PluginType::ClientTool,
    _ => PluginType::DaemonTool,
};
plugin.plugin_type = ptype;
```

In `impl Plugin for ExternalPlugin`:

```rust
fn plugin_type(&self) -> PluginType {
    self.plugin_type
}
```

**Step 4: Add helper to PluginManager**

```rust
/// Find the plugin type for a given tool name.
pub fn tool_plugin_type(&self, tool_name: &str) -> Option<PluginType> {
    for plugin in &self.plugins {
        if plugin.tools().iter().any(|t| t.name == tool_name) {
            return Some(plugin.plugin_type());
        }
    }
    None
}
```

**Step 5: Build and test**

Run: `cargo build 2>&1`
Expected: compiles cleanly

Run: `cargo test -p omnish-daemon 2>&1`
Expected: all existing tests pass

**Step 6: Commit**

```
feat(plugin): add PluginType enum (DaemonTool/ClientTool)
```

---

### Task 2: Add ChatToolCall and ChatToolResult protocol messages

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs:12-34` (Message enum)
- Modify: `crates/omnish-protocol/src/message.rs` (message_variant_guard test)

**Step 1: Add structs and variants**

After `ChatToolStatus` struct in `message.rs`, add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolCall {
    pub request_id: String,
    pub thread_id: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolResult {
    pub request_id: String,
    pub thread_id: String,
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
}
```

Add to `Message` enum:

```rust
ChatToolCall(ChatToolCall),
ChatToolResult(ChatToolResult),
```

**Step 2: Update message_variant_guard test**

- Add both variants to the `variants` vec
- Add both to the exhaustive match
- Update `EXPECTED_VARIANT_COUNT` from 21 to 23

**Step 3: Update PROTOCOL_VERSION**

Change `PROTOCOL_VERSION` from 2 to 3 (new wire format variants).

**Step 4: Build and test**

Run: `cargo test -p omnish-protocol 2>&1`
Expected: all tests pass including updated guard

**Step 5: Commit**

```
feat(protocol): add ChatToolCall and ChatToolResult messages
```

---

### Task 3: Refactor agent loop to support client-side tool forwarding

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs:234-445` (handle_chat_message)
- Modify: `crates/omnish-daemon/src/server.rs:65-232` (handle_message)

**Step 1: Add AgentLoopState and pending state map to DaemonServer**

In `server.rs`, add struct:

```rust
struct AgentLoopState {
    llm_req: LlmRequest,
    prior_len: usize,
    pending_tool_calls: Vec<omnish_llm::tool::ToolCall>,
    completed_results: Vec<omnish_llm::tool::ToolResult>,
    messages: Vec<Message>,  // accumulated ChatToolStatus messages
    iteration: usize,
    cm: ChatMessage,
    start: std::time::Instant,
}
```

Add to `DaemonServer`:

```rust
pub struct DaemonServer {
    // ... existing fields ...
    pending_agent_loops: Arc<Mutex<HashMap<String, AgentLoopState>>>,
}
```

Initialize in `new()`:

```rust
pending_agent_loops: Arc::new(Mutex::new(HashMap::new())),
```

Pass `pending_agent_loops` clone through `serve` → handler closure → `handle_message` → `handle_chat_message`.

**Step 2: Split tool execution in agent loop**

In `handle_chat_message`, change the tool execution loop (lines ~332-367). For each tool call:

```rust
let ptype = plugin_mgr.tool_plugin_type(&tc.name);
match ptype {
    Some(PluginType::ClientTool) => {
        // Client-side tool: send ChatToolCall, pause loop
        messages.push(Message::ChatToolStatus(ChatToolStatus {
            request_id: cm.request_id.clone(),
            thread_id: cm.thread_id.clone(),
            tool_name: tc.name.clone(),
            status: status_text,
        }));
        messages.push(Message::ChatToolCall(ChatToolCall {
            request_id: cm.request_id.clone(),
            thread_id: cm.thread_id.clone(),
            tool_name: tc.name.clone(),
            tool_call_id: tc.id.clone(),
            input: tc.input.clone(),
        }));
        // Cache state for resumption
        let state = AgentLoopState {
            llm_req,
            prior_len,
            pending_tool_calls: tool_calls.clone(),
            completed_results: tool_results,
            messages: vec![],
            iteration,
            cm,
            start,
        };
        pending_loops.lock().await.insert(
            state.cm.request_id.clone(),
            state,
        );
        return messages;
    }
    _ => {
        // Daemon-side: execute directly (existing code)
        let mut result = if tc.name == "command_query" {
            command_query_tool.execute(&tc.input)
        } else {
            plugin_mgr.call_tool(&tc.name, &tc.input)
        };
        result.tool_use_id = tc.id.clone();
        tool_results.push(result);
    }
}
```

**Step 3: Handle ChatToolResult in handle_message**

Add a new match arm in `handle_message`:

```rust
Message::ChatToolResult(tr) => {
    return handle_tool_result(tr, mgr, llm, conv_mgr, plugin_mgr, pending_loops).await;
}
```

Implement `handle_tool_result`: look up `AgentLoopState` by `request_id`, add the result to `completed_results`, check if all pending tool calls are fulfilled, if yes resume the agent loop.

**Step 4: Build**

Run: `cargo build 2>&1`
Expected: compiles

**Step 5: Commit**

```
feat(daemon): refactor agent loop for client-side tool forwarding
```

---

### Task 4: Client-side plugin execution

**Files:**
- Modify: `crates/omnish-client/src/main.rs:2039-2067` (chat stream receive loop)

**Step 1: Initialize client PluginManager**

Near the top of `main()`, after config loading, create a client-side PluginManager:

```rust
let client_plugin_mgr = {
    let mut mgr = omnish_daemon::plugin::PluginManager::new();
    mgr.register(Box::new(omnish_daemon::tools::bash::BashTool::new()));
    mgr
};
```

Pass it (or wrap in `Arc`/`Rc`) to the chat loop.

**Step 2: Handle ChatToolCall in stream receive loop**

In the `while let Some(msg) = rx.recv().await` loop (line ~2047), add a match arm:

```rust
Message::ChatToolCall(tc) => {
    // Show status
    let preview: String = tc.input.to_string().chars().take(60).collect();
    let text = format!("\u{1f527} 执行: {}...", preview);
    nix::unistd::write(std::io::stdout(), line_status.show(&text).as_bytes()).ok();

    // Execute locally
    let result = client_plugin_mgr.call_tool(&tc.tool_name, &tc.input);

    // Send result back to daemon
    let result_msg = Message::ChatToolResult(ChatToolResult {
        request_id: tc.request_id.clone(),
        thread_id: tc.thread_id.clone(),
        tool_call_id: tc.tool_call_id,
        content: result.content,
        is_error: result.is_error,
    });
    let _ = rpc.call(result_msg).await;

    // Continue receiving stream (daemon will resume and send more messages)
    // Need to re-enter call_stream for the continuation
}
```

Note: after sending `ChatToolResult`, the daemon resumes the agent loop and sends a new stream of messages. The client needs to call `rpc.call_stream` again or the daemon sends continuation on the same stream. Design choice: daemon sends continuation as response to `ChatToolResult` (simplest — `ChatToolResult` is a new RPC call that returns the remaining stream).

**Step 3: Build and test manually**

Run: `cargo build 2>&1`
Expected: compiles

**Step 4: Commit**

```
feat(client): execute client-side tools locally on ChatToolCall
```

---

### Task 5: Timeout for pending agent loop states

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

**Step 1: Add timeout check**

In `handle_tool_result`, before resuming, check elapsed time:

```rust
if state.start.elapsed() > std::time::Duration::from_secs(60) {
    // Timeout — inject error and return
    let error_result = ToolResult {
        tool_use_id: tr.tool_call_id,
        content: "Client-side tool execution timed out".to_string(),
        is_error: true,
    };
    // ... continue agent loop with error result
}
```

**Step 2: Add periodic cleanup**

Optionally, spawn a background task that sweeps `pending_agent_loops` every 30s and removes entries older than 60s.

**Step 3: Build and test**

Run: `cargo build && cargo test --workspace 2>&1`
Expected: all pass

**Step 4: Commit**

```
feat(daemon): add timeout for pending client-side tool calls
```

---

### Task 6: Move BashTool execution out of daemon registration

**Files:**
- Modify: `crates/omnish-daemon/src/main.rs:149`

**Step 1: Keep BashTool registered in daemon (for tool definitions)**

The daemon still needs `BashTool` registered so its `ToolDef` is included in LLM requests. No change needed — it stays registered. Its `call_tool` on daemon side will never be called because the agent loop checks `plugin_type()` and forwards to client instead.

**Step 2: Verify end-to-end**

Manual test:
1. Start daemon
2. Connect client
3. Enter chat mode
4. Ask LLM to run a bash command (e.g., "list files in current directory")
5. Verify: daemon sends ChatToolCall, client executes locally, result flows back, LLM responds with the output

**Step 3: Commit**

```
test: verify client-side bash tool execution end-to-end
```

---

### Task 7: Update system prompt and clean up

**Files:**
- Modify: `crates/omnish-llm/src/template.rs:80-86`
- Modify: `crates/omnish-daemon/src/server.rs` (remove bash-specific status text)

**Step 1: Update bash tool description in system prompt**

Change the bash section to reflect it runs on the user's machine:

```
### bash\n\
Execute bash commands on the user's machine:\n\
- Use this to run commands, inspect files, check system state, etc.\n\
- Commands run in the user's current working directory.\n\
```

**Step 2: Remove hardcoded bash status from daemon agent loop**

The daemon no longer executes bash directly, so the `"bash" =>` match arm in the status_text block can be removed (status is now shown by the client).

**Step 3: Build and test**

Run: `cargo build && cargo test --workspace 2>&1`
Expected: all pass

**Step 4: Commit and tag**

```
feat: client-side plugin execution for bash tool (issue #195)
```
