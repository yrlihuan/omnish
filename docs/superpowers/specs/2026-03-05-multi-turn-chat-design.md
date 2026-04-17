# Multi-Turn Chat Mode Design

Issue: #110

## Overview

Transform the existing single-turn chat into a persistent multi-turn conversation mode. Users enter chat mode with `:`, see previous conversation context, and exchange multiple messages before exiting with ESC/Ctrl-C. Conversations are stored as independent threads, persist across sessions, and are sent to the LLM as proper multi-turn message arrays.

## Design Decisions

- **Thread model:** Independent threads (like ChatGPT conversations), not tied to terminal sessions
- **Thread selection:** Automatically resume most recent thread; `/new` starts a fresh one
- **Thread ID:** UUID
- **Storage:** One JSONL file per thread at `~/.omnish/threads/{uuid}.jsonl`
- **History display:** Show only last Q&A exchange in gray on re-entry, with earlier message count
- **LLM integration:** Proper multi-turn messages (alternating user/assistant), not stuffed into single message
- **Context budget:** No truncation for now; add later if needed
- **Input UX:** `> ` prompt in chat loop; ESC/Ctrl-C exits to shell
- **Interceptor:** No changes needed; existing `:` prefix detection works as-is

## Protocol Changes

Four new message types in `omnish-protocol`:

```rust
// Client -> Daemon: enter chat mode
Message::ChatStart {
    request_id: String,
    session_id: String,
    new_thread: bool,         // true = /new, false = resume most recent
}

// Daemon -> Client: thread ready with history summary
Message::ChatReady {
    request_id: String,
    thread_id: String,
    last_exchange: Option<(String, String)>,  // (user_query, assistant_reply)
    earlier_count: u32,
}

// Client -> Daemon: send a message in the thread
Message::ChatMessage {
    request_id: String,
    session_id: String,
    thread_id: String,
    query: String,
}

// Daemon -> Client: LLM response
Message::ChatResponse {
    request_id: String,
    thread_id: String,
    content: String,
}
```

New shared struct:

```rust
pub struct ChatTurn {
    pub role: String,      // "user" or "assistant"
    pub content: String,
}
```

## Storage Format

Thread files at `~/.omnish/threads/{uuid}.jsonl`:

```jsonl
{"role":"user","content":"how do I find large files","ts":"2026-03-05T10:00:00Z"}
{"role":"assistant","content":"Use `du -sh * | sort -rh | head`...","ts":"2026-03-05T10:00:03Z"}
```

No index file. Most recent thread determined by file modification time.

## ConversationManager

New struct in `omnish-daemon`:

```rust
pub struct ConversationManager {
    threads_dir: PathBuf,  // ~/.omnish/threads/
}

impl ConversationManager {
    pub fn new(threads_dir: PathBuf) -> Self;
    pub fn create_thread(&self) -> String;
    pub fn get_latest_thread(&self) -> Option<String>;
    pub fn append_exchange(&self, thread_id: &str, query: &str, response: &str);
    pub fn get_last_exchange(&self, thread_id: &str) -> (Option<(String, String)>, u32);
    pub fn load_messages(&self, thread_id: &str) -> Vec<ChatTurn>;
}
```

Held by `Server` alongside existing `SessionManager`.

## LLM Backend Changes

`LlmRequest` gains a new field:

```rust
pub struct LlmRequest {
    // ... existing fields ...
    pub conversation: Vec<ChatTurn>,  // NEW: previous turns (empty for non-chat)
}
```

When `conversation` is non-empty, Anthropic/OpenAI backends build multi-turn message arrays:
- First user message: terminal context + first user query
- Alternating assistant/user messages from history
- Final user message: current query

When empty, behavior is unchanged (completion, analysis use cases).

## Client Changes

Only `main.rs` changes. When `InterceptAction::Chat` is received:

1. Send `ChatStart { new_thread: false }` to daemon
2. Receive `ChatReady` with thread info
3. Display gray history if present:
   - `"(N earlier messages)"` if earlier_count > 0
   - Last user query and assistant reply in dim gray
4. Show `> ` prompt
5. Loop: collect input -> send `ChatMessage` -> receive `ChatResponse` -> render -> `> `
6. ESC or Ctrl-C: exit loop, return to shell prompt

Special input in chat loop:
- `/new`: send `ChatStart { new_thread: true }`, reset thread context

## Components NOT Changed

- `InputInterceptor` (existing `:` detection works)
- Completion system
- Tracker (command boundary detection)
- Transport layer
- PTY layer

---

# Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add persistent multi-turn chat mode with cross-session conversation threads.

**Architecture:** Hybrid approach - client owns input loop and display, daemon owns thread storage and LLM calls. New `ConversationManager` in daemon handles thread CRUD. Four new protocol message types connect them.

**Tech Stack:** Rust, serde/serde_json (JSONL storage), uuid (thread IDs), omnish-protocol (bincode framing)

---

### Task 1: Protocol - Add ChatTurn and new message types

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs:8-24`

**Step 1: Add ChatTurn struct and four new message variants**

Add after the existing `CompletionSummary` struct (before `impl Message`), and add variants to the `Message` enum:

```rust
// Add to Message enum (after CompletionSummary variant, before Ack):
    ChatStart(ChatStart),
    ChatReady(ChatReady),
    ChatMessage(ChatMessage),
    ChatResponse(ChatResponse),

// Add new structs (before impl Message):

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatStart {
    pub request_id: String,
    pub session_id: String,
    pub new_thread: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatReady {
    pub request_id: String,
    pub thread_id: String,
    pub last_exchange: Option<(String, String)>,
    pub earlier_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub request_id: String,
    pub session_id: String,
    pub thread_id: String,
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub request_id: String,
    pub thread_id: String,
    pub content: String,
}
```

**Step 2: Add round-trip test**

```rust
#[test]
fn test_frame_with_chat_start() {
    let frame = Frame {
        request_id: 30,
        payload: Message::ChatStart(ChatStart {
            request_id: "abc".to_string(),
            session_id: "sess1".to_string(),
            new_thread: false,
        }),
    };
    let bytes = frame.to_bytes().unwrap();
    let decoded = Frame::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.request_id, 30);
    assert!(matches!(decoded.payload, Message::ChatStart(_)));
}
```

**Step 3: Run tests**

Run: `cargo test -p omnish-protocol`
Expected: All tests pass including new round-trip test.

**Step 4: Commit**

```
feat(protocol): add ChatStart/ChatReady/ChatMessage/ChatResponse message types (issue #110)
```

---

### Task 2: LLM - Add conversation field to LlmRequest

**Files:**
- Modify: `crates/omnish-llm/src/backend.rs:22-32`
- Modify: `crates/omnish-daemon/src/server.rs` (all LlmRequest constructions)

**Step 1: Add conversation field to LlmRequest**

In `backend.rs`, add field to `LlmRequest`:

```rust
pub struct LlmRequest {
    pub context: String,
    pub query: Option<String>,
    pub trigger: TriggerType,
    pub session_ids: Vec<String>,
    pub use_case: UseCase,
    pub max_content_chars: Option<usize>,
    pub conversation: Vec<omnish_protocol::message::ChatTurn>,
}
```

**Step 2: Fix all existing LlmRequest construction sites**

In `server.rs`, add `conversation: vec![]` to all existing `LlmRequest { ... }` blocks:
- `try_warmup_kv_cache` (line ~215)
- `handle_llm_request` (line ~428)
- `handle_completion_request` (line ~503)

**Step 3: Run tests**

Run: `cargo test -p omnish-llm -p omnish-daemon`
Expected: All existing tests pass (conversation field is empty everywhere).

**Step 4: Commit**

```
feat(llm): add conversation field to LlmRequest for multi-turn chat (issue #110)
```

---

### Task 3: LLM backends - Build multi-turn message arrays

**Files:**
- Modify: `crates/omnish-llm/src/anthropic.rs:18-33`
- Modify: `crates/omnish-llm/src/openai_compat.rs:33-47`

**Step 1: Update AnthropicBackend::complete()**

Replace the single-message body construction with multi-turn logic:

```rust
async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse> {
    let client = &self.client;

    let messages: Vec<serde_json::Value> = if req.conversation.is_empty() {
        // Existing single-turn behavior
        let user_content = crate::template::build_user_content(
            &req.context,
            req.query.as_deref(),
        );
        vec![serde_json::json!({"role": "user", "content": user_content})]
    } else {
        // Multi-turn: conversation history + current query
        let mut msgs = Vec::new();
        for turn in &req.conversation {
            msgs.push(serde_json::json!({
                "role": turn.role,
                "content": turn.content
            }));
        }
        // Append current query as final user message
        if let Some(ref q) = req.query {
            msgs.push(serde_json::json!({"role": "user", "content": q}));
        }
        msgs
    };

    let body = serde_json::json!({
        "model": self.model,
        "max_tokens": 1024,
        "messages": messages
    });
    // ... rest unchanged ...
```

**Step 2: Apply same pattern to OpenAiCompatBackend::complete()**

Same multi-turn logic, same structure.

**Step 3: Run tests**

Run: `cargo test -p omnish-llm`
Expected: All existing tests pass.

**Step 4: Commit**

```
feat(llm): support multi-turn conversation in Anthropic and OpenAI backends (issue #110)
```

---

### Task 4: ConversationManager - Thread storage and retrieval

**Files:**
- Create: `crates/omnish-daemon/src/conversation_mgr.rs`
- Modify: `crates/omnish-daemon/src/lib.rs`

**Step 1: Implement ConversationManager with tests**

```rust
use anyhow::Result;
use omnish_protocol::message::ChatTurn;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct StoredMessage {
    role: String,
    content: String,
    ts: String,
}

pub struct ConversationManager {
    threads_dir: PathBuf,
}

impl ConversationManager {
    pub fn new(threads_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&threads_dir).ok();
        Self { threads_dir }
    }

    pub fn create_thread(&self) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        // Create empty file to establish the thread
        let path = self.threads_dir.join(format!("{}.jsonl", id));
        std::fs::File::create(&path).ok();
        id
    }

    pub fn get_latest_thread(&self) -> Option<String> {
        let mut entries: Vec<_> = std::fs::read_dir(&self.threads_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "jsonl"))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.metadata().ok().and_then(|m| m.modified().ok())));
        entries.first().map(|e| {
            e.path().file_stem().unwrap().to_string_lossy().to_string()
        })
    }

    pub fn append_exchange(&self, thread_id: &str, query: &str, response: &str) {
        use std::io::Write;
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        let mut file = std::fs::OpenOptions::new()
            .create(true).append(true).open(&path).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let user_msg = StoredMessage { role: "user".into(), content: query.into(), ts: now.clone() };
        let asst_msg = StoredMessage { role: "assistant".into(), content: response.into(), ts: now };
        writeln!(file, "{}", serde_json::to_string(&user_msg).unwrap()).ok();
        writeln!(file, "{}", serde_json::to_string(&asst_msg).unwrap()).ok();
    }

    pub fn get_last_exchange(&self, thread_id: &str) -> (Option<(String, String)>, u32) {
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return (None, 0),
        };
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        let total_messages = lines.len() as u32;
        if lines.len() < 2 {
            return (None, 0);
        }
        let user: StoredMessage = serde_json::from_str(lines[lines.len() - 2]).unwrap();
        let asst: StoredMessage = serde_json::from_str(lines[lines.len() - 1]).unwrap();
        let earlier = total_messages.saturating_sub(2);
        (Some((user.content, asst.content)), earlier)
    }

    pub fn load_messages(&self, thread_id: &str) -> Vec<ChatTurn> {
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        content.lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str::<StoredMessage>(l).ok())
            .map(|m| ChatTurn { role: m.role, content: m.content })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_get_latest() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        assert!(mgr.get_latest_thread().is_none());
        let id = mgr.create_thread();
        assert_eq!(mgr.get_latest_thread(), Some(id));
    }

    #[test]
    fn test_append_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        mgr.append_exchange(&id, "hello", "hi there");
        mgr.append_exchange(&id, "how are you", "doing well");
        let msgs = mgr.load_messages(&id);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content, "hi there");
    }

    #[test]
    fn test_get_last_exchange() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        let (ex, count) = mgr.get_last_exchange(&id);
        assert!(ex.is_none());
        assert_eq!(count, 0);

        mgr.append_exchange(&id, "q1", "a1");
        mgr.append_exchange(&id, "q2", "a2");
        let (ex, count) = mgr.get_last_exchange(&id);
        assert_eq!(ex, Some(("q2".into(), "a2".into())));
        assert_eq!(count, 2); // 2 earlier messages (q1, a1)
    }
}
```

**Step 2: Register module**

In `crates/omnish-daemon/src/lib.rs`, add:
```rust
pub mod conversation_mgr;
```

**Step 3: Add uuid and chrono deps if not already present**

Check `crates/omnish-daemon/Cargo.toml` - add `uuid = { version = "1", features = ["v4"] }` and `chrono = { version = "0.4", features = ["serde"] }` if missing.

**Step 4: Run tests**

Run: `cargo test -p omnish-daemon -- conversation_mgr`
Expected: All 3 tests pass.

**Step 5: Commit**

```
feat(daemon): add ConversationManager for thread storage (issue #110)
```

---

### Task 5: Daemon server - Handle ChatStart and ChatMessage

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs:11-24` (DaemonServer struct)
- Modify: `crates/omnish-daemon/src/server.rs:54-191` (handle_message)
- Modify: `crates/omnish-daemon/src/main.rs:139` (initialization)

**Step 1: Add ConversationManager to DaemonServer**

```rust
use omnish_daemon::conversation_mgr::ConversationManager;

pub struct DaemonServer {
    session_mgr: Arc<SessionManager>,
    llm_backend: Option<Arc<dyn LlmBackend>>,
    task_mgr: Arc<Mutex<TaskManager>>,
    conv_mgr: Arc<ConversationManager>,
}
```

Update `new()` to accept and store `conv_mgr`.

**Step 2: Pass conv_mgr through serve() into handle_message**

Clone `conv_mgr` alongside `mgr`, `llm`, `task_mgr` in the `serve()` closure. Add `conv_mgr` parameter to `handle_message`.

**Step 3: Handle ChatStart**

In `handle_message`, add match arm:

```rust
Message::ChatStart(cs) => {
    let thread_id = if cs.new_thread {
        conv_mgr.create_thread()
    } else {
        conv_mgr.get_latest_thread().unwrap_or_else(|| conv_mgr.create_thread())
    };
    let (last_exchange, earlier_count) = conv_mgr.get_last_exchange(&thread_id);
    Message::ChatReady(ChatReady {
        request_id: cs.request_id,
        thread_id,
        last_exchange,
        earlier_count,
    })
}
```

**Step 4: Handle ChatMessage**

Add match arm with LLM call using conversation history:

```rust
Message::ChatMessage(cm) => {
    let content = if let Some(ref backend) = llm {
        let conversation = conv_mgr.load_messages(&cm.thread_id);
        let use_case = UseCase::Chat;
        let max_context_chars = backend.max_content_chars_for_use_case(use_case);

        // Get terminal context for the first message (when conversation is empty)
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

        let llm_req = LlmRequest {
            context,
            query: Some(cm.query.clone()),
            trigger: TriggerType::Manual,
            session_ids: vec![cm.session_id.clone()],
            use_case,
            max_content_chars: max_context_chars,
            conversation,
        };

        let start = std::time::Instant::now();
        match backend.complete(&llm_req).await {
            Ok(response) => {
                tracing::info!("Chat LLM completed in {:?} (thread={})", start.elapsed(), cm.thread_id);
                conv_mgr.append_exchange(&cm.thread_id, &cm.query, &response.content);
                response.content
            }
            Err(e) => {
                tracing::error!("Chat LLM failed: {}", e);
                format!("Error: {}", e)
            }
        }
    } else {
        "(LLM backend not configured)".to_string()
    };

    Message::ChatResponse(ChatResponse {
        request_id: cm.request_id,
        thread_id: cm.thread_id,
        content,
    })
}
```

**Step 5: Initialize ConversationManager in main.rs**

In `main.rs` after `omnish_dir` is set:

```rust
let conv_mgr = Arc::new(ConversationManager::new(omnish_dir.join("threads")));
let server = DaemonServer::new(session_mgr, llm_backend, task_mgr, conv_mgr);
```

**Step 6: Run tests**

Run: `cargo test -p omnish-daemon`
Expected: All tests pass. May need to update `DaemonServer::new` call in existing tests.

**Step 7: Commit**

```
feat(daemon): handle ChatStart/ChatMessage with ConversationManager (issue #110)
```

---

### Task 6: Client - Chat mode loop

**Files:**
- Modify: `crates/omnish-client/src/main.rs:455-491` (InterceptAction::Chat handler)
- Modify: `crates/omnish-client/src/display.rs` (add chat history rendering)
- Modify: `crates/omnish-client/src/command.rs` (add /new command)

**Step 1: Add display functions for chat mode**

In `display.rs`, add:

```rust
/// Render previous chat history in dim gray for chat mode re-entry.
pub fn render_chat_history(last_exchange: Option<&(String, String)>, earlier_count: u32) -> String {
    let mut output = String::new();
    if earlier_count > 0 {
        output.push_str(&format!("\r\n\x1b[2;37m({} earlier messages)\x1b[0m", earlier_count));
    }
    if let Some((query, reply)) = last_exchange {
        output.push_str(&format!("\r\n\x1b[2;37m> {}\x1b[0m", query));
        for line in reply.lines() {
            output.push_str(&format!("\r\n\x1b[2;37m{}\x1b[0m", line.trim_end()));
        }
    }
    output
}

/// Render the chat mode prompt: "> " in cyan.
pub fn render_chat_prompt() -> String {
    "\r\n\x1b[36m> \x1b[0m".to_string()
}
```

**Step 2: Replace InterceptAction::Chat handler with chat loop**

In `main.rs`, replace the `InterceptAction::Chat(msg)` arm body. The new logic:

1. Ignore the `msg` content (`:` trigger only, no payload)
2. Send `ChatStart` → receive `ChatReady`
3. Render history + `> ` prompt
4. Enter a loop reading raw bytes from stdin:
   - Collect bytes until Enter (build line buffer, handle backspace, echo chars)
   - On Enter: if line is `/new`, send new `ChatStart`; if line is empty, show prompt again; otherwise send `ChatMessage`, show thinking, display response, show `> ` prompt
   - On ESC or Ctrl-C: break loop, dismiss UI, restore shell
5. After loop: `proxy.write_all(b"\x15\r").ok()` to clear readline

The input collection in the chat loop reads raw bytes from stdin fd (already in raw mode). It needs to handle:
- Printable bytes: echo and append to buffer
- Backspace (0x7f): remove last byte, re-render line
- Enter (0x0d): submit buffer
- ESC (0x1b): exit chat mode
- Ctrl-C (0x03): exit chat mode

**Step 3: Add /new to command dispatch**

In `command.rs`, the `/new` command is handled inside the chat loop, not through `dispatch()`. No changes needed to command.rs - `/new` is checked directly in the chat loop before calling dispatch.

**Step 4: Build and manually test**

Run: `cargo build -p omnish-client`
Expected: Compiles without errors.

Manual test: start daemon + client, type `:`, verify `> ` prompt appears, type a question, verify response, type another question, verify multi-turn, press ESC to exit.

**Step 5: Commit**

```
feat(client): implement multi-turn chat mode loop (issue #110)
```

---

### Task 7: Integration - First user message includes terminal context

**Files:**
- Modify: `crates/omnish-llm/src/anthropic.rs`
- Modify: `crates/omnish-llm/src/openai_compat.rs`

**Step 1: Wrap terminal context into first user message**

When `conversation` is non-empty and `context` is non-empty (first message in thread), prepend the terminal context to the first user message:

```rust
// In the multi-turn branch:
let mut msgs = Vec::new();
for (i, turn) in req.conversation.iter().enumerate() {
    let content = if i == 0 && !req.context.is_empty() {
        // Prepend terminal context to first user message
        format!("Terminal context:\n{}\n\n{}", req.context, turn.content)
    } else {
        turn.content.clone()
    };
    msgs.push(serde_json::json!({"role": turn.role, "content": content}));
}
```

This ensures the LLM has terminal context for the first exchange, but subsequent messages in the same thread don't repeat it.

**Step 2: Run tests**

Run: `cargo test -p omnish-llm`
Expected: All tests pass.

**Step 3: Commit**

```
feat(llm): prepend terminal context to first conversation message (issue #110)
```

---

### Task 8: Final testing and cleanup

**Step 1: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 2: Manual end-to-end test**

1. Start daemon and client
2. Type `:` → verify `> ` prompt appears
3. Type a question → verify response
4. Type follow-up → verify it references first question (multi-turn works)
5. Press ESC → verify return to shell
6. Type `:` again → verify last exchange shown in gray
7. Type `/new` → verify fresh thread
8. Exit and restart client → type `:` → verify history persists

**Step 3: Commit any fixes, push, close issue**

```
git push
glab issue note 110 -m "Implemented multi-turn chat mode. Commits: ..."
glab issue close 110
```
