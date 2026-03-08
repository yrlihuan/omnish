# Chat Context Redesign Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Maximize KV cache hit rate on conversation resume by storing raw API messages (serde_json::Value) and replaying them byte-for-byte via extra_messages.

**Architecture:** Replace StoredMessage {role, content, ts} with raw serde_json::Value per line in JSONL. All messages (history + new) flow through LlmRequest.extra_messages, bypassing the conversation field entirely. Dynamic content (<system-reminder> with command list) is appended only to the last user message.

**Tech Stack:** Rust, serde_json, omnish-daemon, omnish-llm

---

### Task 1: Rewrite ConversationManager storage format

**Files:**
- Modify: `crates/omnish-daemon/src/conversation_mgr.rs:1-225`

**Step 1: Replace StoredMessage with raw JSON**

Remove `StoredMessage` struct and change `threads` to `Vec<serde_json::Value>`:

```rust
// DELETE these lines (7-12):
// #[derive(Serialize, Deserialize, Clone)]
// struct StoredMessage {
//     role: String,
//     content: String,
//     ts: String,
// }

pub struct ConversationManager {
    threads_dir: PathBuf,
    /// In-memory store: thread_id → raw API messages (serde_json::Value).
    threads: Mutex<HashMap<String, Vec<serde_json::Value>>>,
}
```

**Step 2: Update `new()` to load raw JSON values**

```rust
let msgs: Vec<serde_json::Value> = content
    .lines()
    .filter(|l| !l.is_empty())
    .filter_map(|l| serde_json::from_str(l).ok())
    .collect();
```

No change needed — `serde_json::from_str::<serde_json::Value>` works on any valid JSON line.

**Step 3: Replace `append_exchange` with `append_messages`**

```rust
/// Append raw API messages. Writes to both memory and disk (append-only).
pub fn append_messages(&self, thread_id: &str, messages: &[serde_json::Value]) {
    // Update memory
    self.threads
        .lock()
        .unwrap()
        .entry(thread_id.to_string())
        .or_default()
        .extend(messages.iter().cloned());

    // Append to disk
    use std::io::Write;
    let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        for msg in messages {
            writeln!(file, "{}", serde_json::to_string(msg).unwrap()).ok();
        }
    }
}
```

**Step 4: Replace `load_messages` with `load_raw_messages`**

```rust
/// Load all raw messages for API replay. No processing — returns as stored.
pub fn load_raw_messages(&self, thread_id: &str) -> Vec<serde_json::Value> {
    let threads = self.threads.lock().unwrap();
    threads.get(thread_id).cloned().unwrap_or_default()
}
```

**Step 5: Rewrite `get_last_exchange` to extract text from raw JSON**

```rust
/// Get the last exchange and count of earlier user messages.
/// Extracts text from raw JSON messages.
pub fn get_last_exchange(&self, thread_id: &str) -> (Option<(String, String)>, u32) {
    let threads = self.threads.lock().unwrap();
    let msgs = match threads.get(thread_id) {
        Some(m) => m.clone(),
        None => return (None, 0),
    };
    drop(threads);

    // Count user input messages (content is String, not Array with tool_result)
    let user_inputs: Vec<&serde_json::Value> = msgs
        .iter()
        .filter(|m| Self::is_user_input(m))
        .collect();
    let total = user_inputs.len() as u32;
    if total == 0 {
        return (None, 0);
    }

    // Find the last user input and collect assistant text after it
    let last_user_idx = msgs
        .iter()
        .rposition(|m| Self::is_user_input(m))
        .unwrap();
    let user_text = Self::extract_text(&msgs[last_user_idx]);

    // Collect all assistant text blocks after the last user input
    let assistant_text: String = msgs[last_user_idx + 1..]
        .iter()
        .filter(|m| m["role"].as_str() == Some("assistant"))
        .map(|m| Self::extract_text(m))
        .collect::<Vec<_>>()
        .join("\n");

    let earlier = total.saturating_sub(1);
    if assistant_text.is_empty() {
        (None, earlier)
    } else {
        (Some((user_text, assistant_text)), earlier)
    }
}
```

**Step 6: Add helper methods for text extraction**

```rust
/// Check if a message is a user input (not a tool_result).
fn is_user_input(msg: &serde_json::Value) -> bool {
    if msg["role"].as_str() != Some("user") {
        return false;
    }
    // User input has String content; tool_result has Array content
    msg["content"].is_string()
}

/// Extract readable text from a message.
/// - String content: return as-is (strip <system-reminder> blocks)
/// - Array content: concatenate all "text" type blocks
fn extract_text(msg: &serde_json::Value) -> String {
    match &msg["content"] {
        serde_json::Value::String(s) => {
            // Strip <system-reminder>...</system-reminder> blocks
            if let Some(pos) = s.find("\n\n<system-reminder>") {
                s[..pos].to_string()
            } else {
                s.clone()
            }
        }
        serde_json::Value::Array(arr) => {
            arr.iter()
                .filter_map(|b| {
                    if b["type"].as_str() == Some("text") {
                        b["text"].as_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => String::new(),
    }
}
```

**Step 7: Rewrite `list_conversations`**

```rust
pub fn list_conversations(&self) -> Vec<(String, std::time::SystemTime, u32, String)> {
    let threads = self.threads.lock().unwrap();
    let mut conversations: Vec<_> = threads
        .iter()
        .filter_map(|(thread_id, msgs)| {
            let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            // Count user input messages
            let exchange_count = msgs.iter().filter(|m| Self::is_user_input(m)).count() as u32;
            // Get last user input text
            let last_question = msgs
                .iter()
                .rev()
                .find(|m| Self::is_user_input(m))
                .map(|m| Self::extract_text(m))
                .unwrap_or_default();
            Some((thread_id.clone(), modified, exchange_count, last_question))
        })
        .collect();
    conversations.sort_by(|a, b| b.1.cmp(&a.1));
    conversations
}
```

**Step 8: Remove `resolve_interrupted` and `INTERRUPTED_MARKER`**

The interrupt resolution logic was built around the old (user, assistant) pair model. With raw JSON messages, an interrupt is simply stored as:
```json
{"role":"assistant","content":"<event>user interrupted</event>"}
```
This doesn't need special resolution — the LLM sees the interrupt marker in the conversation history and understands the context. Delete `resolve_interrupted`, `INTERRUPTED_MARKER`, and their usage.

**Step 9: Remove `ChatTurn` import**

Delete `use omnish_protocol::message::ChatTurn;` from the imports.

**Step 10: Build and check**

Run: `cargo build -p omnish-daemon 2>&1 | head -50`
Expected: Compile errors in server.rs callers (will fix in Task 2). No errors in conversation_mgr.rs itself.

**Step 11: Commit**

```bash
git add crates/omnish-daemon/src/conversation_mgr.rs
git commit -m "refactor(conversation_mgr): store raw JSON messages for KV cache optimization"
```

---

### Task 2: Update server.rs callers

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs:200-500`

**Step 1: Update `handle_chat_message` to use raw messages via extra_messages**

Replace the current flow (lines 225-411) with:

```rust
async fn handle_chat_message(
    cm: ChatMessage,
    mgr: &SessionManager,
    llm: &Option<Arc<dyn LlmBackend>>,
    conv_mgr: &Arc<ConversationManager>,
) -> Vec<Message> {
    let backend = match llm {
        Some(b) => b,
        None => {
            return vec![Message::ChatResponse(ChatResponse {
                request_id: cm.request_id,
                thread_id: cm.thread_id,
                content: "(LLM backend not configured)".to_string(),
            })];
        }
    };

    // Load prior messages for replay
    let prior_messages = conv_mgr.load_raw_messages(&cm.thread_id);
    let use_case = UseCase::Chat;
    let max_context_chars = backend.max_content_chars_for_use_case(use_case);

    // Build tools from current session data
    let (commands, stream_reader) = mgr.get_all_commands_with_reader().await;
    let command_query_tool = omnish_daemon::tools::command_query::CommandQueryTool::new(
        commands,
        stream_reader,
    );

    // Include recent command list in user message as <system-reminder>
    let command_list = command_query_tool.list_history(20);
    let user_content = format!(
        "{}\n\n<system-reminder>Recent commands:\n{}\n</system-reminder>",
        cm.query, command_list
    );

    let registered_tools: Vec<Box<dyn Tool>> = vec![Box::new(command_query_tool)];
    let tools: Vec<omnish_llm::tool::ToolDef> = registered_tools
        .iter()
        .map(|t| t.definition())
        .collect();

    // Build extra_messages: prior history + new user message
    let mut extra_messages = prior_messages;
    extra_messages.push(serde_json::json!({
        "role": "user",
        "content": user_content,
    }));

    let mut llm_req = LlmRequest {
        context: String::new(),         // Not used — everything in extra_messages
        query: None,                     // Not used — user message in extra_messages
        trigger: TriggerType::Manual,
        session_ids: vec![cm.session_id.clone()],
        use_case,
        max_content_chars: max_context_chars,
        conversation: vec![],            // Not used — everything in extra_messages
        system_prompt: Some(omnish_llm::template::CHAT_SYSTEM_PROMPT.to_string()),
        enable_thinking: None,
        tools,
        extra_messages,
    };

    // Track new messages added this turn (for storage)
    let prior_len = llm_req.extra_messages.len();

    let mut messages = Vec::new();
    let max_iterations = 5;
    let start = std::time::Instant::now();

    for iteration in 0..max_iterations {
        match backend.complete(&llm_req).await {
            Ok(response) => {
                if response.stop_reason == StopReason::ToolUse {
                    // ... (tool execution loop — same as current, no changes needed)
                    // The existing code that builds assistant_content and tool_results
                    // and pushes to llm_req.extra_messages stays identical.
                    let tool_calls = response.tool_calls();
                    if tool_calls.is_empty() {
                        break;
                    }

                    let assistant_content: Vec<serde_json::Value> = response
                        .content
                        .iter()
                        .map(|b| match b {
                            ContentBlock::Text(t) => {
                                serde_json::json!({"type": "text", "text": t})
                            }
                            ContentBlock::ToolUse(tc) => serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.name,
                                "input": tc.input,
                            }),
                        })
                        .collect();
                    llm_req.extra_messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": assistant_content,
                    }));

                    let mut tool_results = Vec::new();
                    for tc in &tool_calls {
                        let status_text = match tc.name.as_str() {
                            "command_query" => match tc.input["action"].as_str() {
                                Some("list_history") => "查询命令历史...".to_string(),
                                Some("get_output") => format!(
                                    "获取命令输出 [{}]...",
                                    tc.input["seq"].as_u64().unwrap_or(0)
                                ),
                                _ => format!("执行 {}...", tc.name),
                            },
                            _ => format!("执行 {}...", tc.name),
                        };
                        messages.push(Message::ChatToolStatus(ChatToolStatus {
                            request_id: cm.request_id.clone(),
                            thread_id: cm.thread_id.clone(),
                            tool_name: tc.name.clone(),
                            status: status_text,
                        }));

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

                    let result_content: Vec<serde_json::Value> = tool_results
                        .iter()
                        .map(|r| {
                            serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": r.tool_use_id,
                                "content": r.content,
                                "is_error": r.is_error,
                            })
                        })
                        .collect();
                    llm_req.extra_messages.push(serde_json::json!({
                        "role": "user",
                        "content": result_content,
                    }));

                    continue;
                }

                // EndTurn or MaxTokens — build final assistant message and store
                let assistant_content: Vec<serde_json::Value> = response
                    .content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text(t) => {
                            serde_json::json!({"type": "text", "text": t})
                        }
                        ContentBlock::ToolUse(tc) => serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": tc.input,
                        }),
                    })
                    .collect();
                llm_req.extra_messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": assistant_content,
                }));

                let text = response.text();
                tracing::info!(
                    "Chat LLM completed in {:?} ({} tool iterations, thread={})",
                    start.elapsed(),
                    iteration,
                    cm.thread_id
                );

                // Store only the new messages from this turn
                let new_messages = &llm_req.extra_messages[prior_len - 1..];
                conv_mgr.append_messages(&cm.thread_id, new_messages);

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

    // Exhausted iterations — store partial conversation
    let new_messages = &llm_req.extra_messages[prior_len - 1..];
    conv_mgr.append_messages(&cm.thread_id, new_messages);
    let text = "(Agent reached maximum tool call limit)".to_string();
    messages.push(Message::ChatResponse(ChatResponse {
        request_id: cm.request_id,
        thread_id: cm.thread_id,
        content: text,
    }));
    messages
}
```

Key changes:
- `load_messages` → `load_raw_messages`, result goes into `extra_messages`
- `context` field is empty — not used
- `query` is None — user message already in extra_messages with `<system-reminder>` appended
- `conversation` is empty — everything via extra_messages
- `append_exchange` → `append_messages` with the new messages slice
- Final assistant response is also stored as raw JSON (not just text)

**Step 2: Update ChatInterrupt handler**

```rust
Message::ChatInterrupt(ci) => {
    conv_mgr.append_messages(&ci.thread_id, &[
        serde_json::json!({"role": "user", "content": ci.query}),
        serde_json::json!({"role": "assistant", "content": "<event>user interrupted</event>"}),
    ]);
    tracing::info!("Chat interrupted by user (thread={})", ci.thread_id);
    Message::Ack
}
```

**Step 3: Update `/context chat:` display handler**

```rust
if let Some(thread_id) = sub.strip_prefix("context chat:") {
    let msgs = conv_mgr.load_raw_messages(thread_id);
    if msgs.is_empty() {
        return cmd_display("(empty conversation)");
    }
    let mut output = format!("Chat thread: {}\n\n", thread_id);
    for msg in &msgs {
        let role = msg["role"].as_str().unwrap_or("unknown");
        let label = if role == "user" { "User" } else { "Assistant" };
        let text = ConversationManager::extract_text_public(msg);
        if !text.is_empty() {
            output.push_str(&format!("[{}] {}\n\n", label, text));
        }
    }
    return cmd_display(output);
}
```

Note: We need to make `extract_text` public (or add a public wrapper) for this. Add to conversation_mgr.rs:

```rust
/// Public accessor for extract_text (used by server.rs display).
pub fn extract_text_public(msg: &serde_json::Value) -> String {
    Self::extract_text(msg)
}
```

**Step 4: Build and check**

Run: `cargo build -p omnish-daemon 2>&1 | head -50`
Expected: Compiles successfully (or only warnings).

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/conversation_mgr.rs
git commit -m "refactor(server): use raw JSON messages for chat context replay"
```

---

### Task 3: Update CHAT_SYSTEM_PROMPT with Tools section

**Files:**
- Modify: `crates/omnish-llm/src/template.rs:55-85`

**Step 1: Add Tools section to CHAT_SYSTEM_PROMPT**

After the `## Guidelines` section, add:

```rust
pub const CHAT_SYSTEM_PROMPT: &str = "\
You are the omnish chat assistant. omnish is a transparent shell wrapper that \
records terminal sessions, provides inline command completion, and offers an \
integrated chat interface for asking questions about terminal activity.\n\
\n\
You have access to the user's recent terminal context (commands and their output) \
from all active sessions. Use this context to provide relevant, accurate answers.\n\
\n\
## Chat Mode\n\
\n\
The user is in omnish's chat mode. In chat mode:\n\
- Conversations are persistent and can be resumed across sessions\n\
- The terminal context from recent commands is available to you\n\
- The user can ask about errors, commands, workflows, or anything related to their terminal activity\n\
\n\
## Available Commands (for user reference)\n\
\n\
- /help — Show available commands\n\
- /resume [N] — Resume a previous conversation (N = index from /thread list)\n\
- /thread list — List all conversation threads\n\
- /thread del [N] — Delete a conversation thread\n\
- /context — Show the current LLM context\n\
- /sessions — List active terminal sessions\n\
- ESC or Ctrl-D (on empty input) — Exit chat mode\n\
\n\
## Tools\n\
\n\
You have access to the command_query tool to inspect command output:\n\
- Use get_output(seq) to retrieve the full output of a specific command\n\
- The recent command list is provided at the end of the user's message in <system-reminder>\n\
- You do NOT need to call list_history — the command list is already provided\n\
\n\
## Guidelines\n\
\n\
- Be concise and direct\n\
- When the user asks about errors, reference the specific commands and output from the context\n\
- For shell command questions, provide working examples\n\
- Respond in the same language the user uses";
```

**Step 2: Build and check**

Run: `cargo build -p omnish-llm 2>&1 | head -20`
Expected: Compiles. Existing test `test_chat_system_prompt_mentions_all_commands` should still pass.

**Step 3: Run tests**

Run: `cargo test -p omnish-llm 2>&1 | tail -20`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add crates/omnish-llm/src/template.rs
git commit -m "feat(template): add Tools section to CHAT_SYSTEM_PROMPT"
```

---

### Task 4: Rewrite conversation_mgr tests

**Files:**
- Modify: `crates/omnish-daemon/src/conversation_mgr.rs:227-400`

**Step 1: Rewrite all tests for new API**

Replace the entire `#[cfg(test)]` module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": text})
    }

    fn assistant_msg(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "assistant", "content": text})
    }

    fn assistant_with_tool_use() -> serde_json::Value {
        serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Let me check..."},
                {"type": "tool_use", "id": "toolu_1", "name": "command_query", "input": {"action": "get_output", "seq": 1}}
            ]
        })
    }

    fn tool_result_msg() -> serde_json::Value {
        serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "output data", "is_error": false}
            ]
        })
    }

    #[test]
    fn test_create_and_get_latest() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        assert!(mgr.get_latest_thread().is_none());
        let id = mgr.create_thread();
        assert_eq!(mgr.get_latest_thread(), Some(id));
    }

    #[test]
    fn test_append_and_load_raw() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        mgr.append_messages(&id, &[user_msg("hello"), assistant_msg("hi there")]);
        mgr.append_messages(&id, &[user_msg("how are you"), assistant_msg("doing well")]);
        let msgs = mgr.load_raw_messages(&id);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "hello");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "hi there");
    }

    #[test]
    fn test_get_last_exchange() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();

        let (ex, count) = mgr.get_last_exchange(&id);
        assert!(ex.is_none());
        assert_eq!(count, 0);

        mgr.append_messages(&id, &[user_msg("q1"), assistant_msg("a1")]);
        mgr.append_messages(&id, &[user_msg("q2"), assistant_msg("a2")]);

        let (ex, count) = mgr.get_last_exchange(&id);
        assert_eq!(ex, Some(("q2".into(), "a2".into())));
        assert_eq!(count, 1); // 1 earlier user input
    }

    #[test]
    fn test_empty_thread_load() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        let msgs = mgr.load_raw_messages(&id);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_tool_use_messages_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        mgr.append_messages(&id, &[
            user_msg("check my output"),
            assistant_with_tool_use(),
            tool_result_msg(),
            assistant_msg("Here's what I found"),
        ]);
        let msgs = mgr.load_raw_messages(&id);
        assert_eq!(msgs.len(), 4);
        // tool_result is not a "user input"
        assert!(!ConversationManager::is_user_input(&msgs[2]));
        // user_msg is a "user input"
        assert!(ConversationManager::is_user_input(&msgs[0]));
    }

    #[test]
    fn test_get_last_exchange_with_tool_use() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        mgr.append_messages(&id, &[
            user_msg("check my output"),
            assistant_with_tool_use(),
            tool_result_msg(),
            assistant_msg("Here's what I found"),
        ]);
        let (ex, count) = mgr.get_last_exchange(&id);
        // Should extract user input text and final assistant text
        assert_eq!(ex, Some(("check my output".into(), "Let me check...\nHere's what I found".into())));
        assert_eq!(count, 0); // only 1 user input, so 0 earlier
    }

    #[test]
    fn test_system_reminder_stripped_from_display() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        mgr.append_messages(&id, &[
            serde_json::json!({"role": "user", "content": "what happened\n\n<system-reminder>Recent commands:\n[seq=1] ls\n</system-reminder>"}),
            assistant_msg("Everything looks fine"),
        ]);
        let (ex, _) = mgr.get_last_exchange(&id);
        assert_eq!(ex, Some(("what happened".into(), "Everything looks fine".into())));
    }

    #[test]
    fn test_interrupt_stored_as_raw_messages() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        mgr.append_messages(&id, &[
            user_msg("tell me a story"),
            assistant_msg("<event>user interrupted</event>"),
        ]);
        let msgs = mgr.load_raw_messages(&id);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1]["content"], "<event>user interrupted</event>");
    }

    #[test]
    fn test_delete_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id1 = mgr.create_thread();
        let id2 = mgr.create_thread();
        mgr.append_messages(&id1, &[user_msg("q1"), assistant_msg("a1")]);
        mgr.append_messages(&id2, &[user_msg("q2"), assistant_msg("a2")]);

        assert!(mgr.delete_thread(&id1));
        assert!(mgr.load_raw_messages(&id1).is_empty());
        assert!(!dir.path().join(format!("{}.jsonl", id1)).exists());

        assert_eq!(mgr.load_raw_messages(&id2).len(), 2);
        assert!(!mgr.delete_thread(&id1));

        let convs = mgr.list_conversations();
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].0, id2);
    }

    #[test]
    fn test_load_from_disk_on_startup() {
        let dir = tempfile::tempdir().unwrap();
        let mgr1 = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr1.create_thread();
        mgr1.append_messages(&id, &[user_msg("hello"), assistant_msg("world")]);
        drop(mgr1);

        let mgr2 = ConversationManager::new(dir.path().to_path_buf());
        let msgs = mgr2.load_raw_messages(&id);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["content"], "hello");
        assert_eq!(msgs[1]["content"], "world");
    }
}
```

**Step 2: Make `is_user_input` accessible in tests**

Change `fn is_user_input` from private to `pub(crate)` (or keep private if tests are in the same module — they are, so no change needed).

**Step 3: Run tests**

Run: `cargo test -p omnish-daemon -- conversation 2>&1`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add crates/omnish-daemon/src/conversation_mgr.rs
git commit -m "test(conversation_mgr): rewrite tests for raw JSON storage format"
```

---

### Task 5: Clean up ChatTurn usage

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs` (ChatTurn may still be used elsewhere)
- Modify: `crates/omnish-llm/src/backend.rs` (LlmRequest.conversation field)

**Step 1: Check if ChatTurn is still used anywhere**

Run: `cargo build --workspace 2>&1 | grep -i "unused\|ChatTurn" | head -20`

If `ChatTurn` and `LlmRequest.conversation` are still used by non-chat code paths (e.g., analysis mode), leave them. If only chat used them, remove them.

Based on the anthropic.rs code, `conversation` is used in the multi-turn path — but with the redesign, chat no longer uses it (passes empty vec + everything in extra_messages). If no other code path uses `conversation`, we can remove it. Otherwise, leave it for now.

**Step 2: Full workspace build**

Run: `cargo build --workspace 2>&1 | tail -20`
Expected: Clean build.

**Step 3: Full test suite**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: All tests pass.

**Step 4: Commit (if changes were made)**

```bash
git add -A
git commit -m "chore: clean up unused ChatTurn references"
```

---

### Task 6: End-to-end verification

**Step 1: Start daemon and client**

```bash
cargo run --bin omnish-daemon &
cargo run --bin omnish-client
```

**Step 2: Test new conversation**

In chat mode:
```
/chat
> what commands did I run recently?
```

Verify: LLM responds with command list context, tool_use works if needed.

**Step 3: Test conversation resume**

```
/chat
> follow up question
```

Verify: Prior messages are replayed correctly, LLM has full context.

**Step 4: Test `/context chat:`**

```
/context chat
```

Verify: Shows conversation with [User] and [Assistant] labels, system-reminder stripped from display.

**Step 5: Test interrupt**

Start a chat, press Ctrl-C during LLM response. Resume conversation and verify interrupt is visible in history.

**Step 6: Check JSONL file on disk**

```bash
cat ~/.local/share/omnish/threads/*.jsonl | python3 -m json.tool --no-ensure-ascii | head -50
```

Verify: Raw JSON format with role/content fields matching Anthropic API format.
