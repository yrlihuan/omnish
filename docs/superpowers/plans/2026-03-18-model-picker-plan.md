# Per-Thread Model Selection Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow users to switch LLM backends per conversation thread using a `/model` picker in chat mode.

**Architecture:** Add `model: Option<String>` to `ChatMessage` and `ThreadMeta`. The daemon handles `__cmd:models` to list backends and uses `ChatMessage.model` to update thread metadata and select backend. The client renders a picker and either sends model-only messages (existing thread) or defers selection to first message (new thread).

**Tech Stack:** Rust, omnish workspace crates (protocol, daemon, client, llm, common)

---

## File Structure

| File | Change | Responsibility |
|------|--------|----------------|
| `crates/omnish-protocol/src/message.rs` | Modify | Add `model` field to `ChatMessage`, bump `PROTOCOL_VERSION` |
| `crates/omnish-daemon/src/conversation_mgr.rs` | Modify | Add `model` field to `ThreadMeta` |
| `crates/omnish-llm/src/factory.rs` | Modify | Add `list_backends()` method to `MultiBackend` |
| `crates/omnish-daemon/src/server.rs` | Modify | Store `LlmConfig`, handle `__cmd:models`, use `ChatMessage.model` for backend selection |
| `crates/omnish-daemon/src/main.rs` | Modify | Pass `LlmConfig` to `DaemonServer` |
| `crates/omnish-client/src/command.rs` | Modify | Add `/model` as chat-only command |
| `crates/omnish-client/src/chat_session.rs` | Modify | Handle `/model`: request, picker, send selection |
| `crates/omnish-client/src/widgets/picker.rs` | Modify | Add `pick_one_at()` with pre-selected index |

---

## Chunk 1: Protocol & Data Model

### Task 1: Add `model` to `ThreadMeta`

**Files:**
- Modify: `crates/omnish-daemon/src/conversation_mgr.rs:5-16`

- [ ] **Step 1: Add field to ThreadMeta**

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ThreadMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}
```

- [ ] **Step 2: Build and verify**

Run: `cargo build -p omnish-daemon`
Expected: PASS (field is optional with serde defaults, no breaking changes)

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-daemon/src/conversation_mgr.rs
git commit -m "feat: add model field to ThreadMeta (#154)"
```

### Task 2: Add `model` to `ChatMessage` protocol

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs:9,200-205,506`

- [ ] **Step 1: Add model field to ChatMessage**

At line 200, add `model` field:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub request_id: String,
    pub session_id: String,
    pub thread_id: String,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}
```

- [ ] **Step 2: Bump PROTOCOL_VERSION**

Change line 9:
```rust
pub const PROTOCOL_VERSION: u32 = 6;
```

- [ ] **Step 3: Update test variant count and ChatMessage test data**

In the `message_variant_guard` test, the count stays the same (no new variant, just a new field). But the `ChatMessage` test data at line 596 needs the new field:
```rust
Message::ChatMessage(ChatMessage {
    request_id: String::new(),
    session_id: String::new(),
    thread_id: String::new(),
    query: String::new(),
    model: None,
}),
```

Also update the roundtrip test at line 483:
```rust
payload: Message::ChatMessage(ChatMessage {
    request_id: "r1".to_string(),
    session_id: "s1".to_string(),
    thread_id: "t1".to_string(),
    query: "hello".to_string(),
    model: None,
}),
```

- [ ] **Step 4: Build and test**

Run: `cargo test -p omnish-protocol`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-protocol/src/message.rs
git commit -m "feat: add model field to ChatMessage, bump protocol to v6 (#154)"
```

---

## Chunk 2: Backend Listing & Daemon Handling

### Task 3: Add `list_backends` to MultiBackend

**Files:**
- Modify: `crates/omnish-llm/src/factory.rs:96-199`

- [ ] **Step 1: Add BackendInfo struct and list_backends method**

Add after the `MultiBackend` struct definition (around line 103):

```rust
/// Info about an available backend for listing purposes.
#[derive(Debug, Clone)]
pub struct BackendInfo {
    pub name: String,
    pub model: String,
}
```

The `MultiBackend` needs access to the original config to list backends. Store it:

```rust
pub struct MultiBackend {
    use_case_backends: RwLock<HashMap<String, Arc<dyn LlmBackend>>>,
    default_backend: Arc<dyn LlmBackend>,
    use_case_max_chars: HashMap<String, Option<usize>>,
    /// Original backend configs for listing.
    backend_configs: Vec<BackendInfo>,
    /// Default chat backend name.
    chat_backend_name: String,
}
```

- [ ] **Step 2: Populate backend_configs and chat_backend_name in MultiBackend::new()**

In `MultiBackend::new()`, before building the struct at line 164, collect backend info:

```rust
let mut backend_configs: Vec<BackendInfo> = llm_config.backends.iter()
    .map(|(name, cfg)| BackendInfo {
        name: name.clone(),
        model: cfg.model.clone(),
    })
    .collect();
backend_configs.sort_by(|a, b| a.name.cmp(&b.name));

let chat_backend_name = llm_config.use_cases
    .get("chat")
    .cloned()
    .unwrap_or_else(|| llm_config.default.clone());
```

Add `backend_configs` and `chat_backend_name` to the struct initialization.

- [ ] **Step 3: Add list_backends and chat_default_name methods**

```rust
impl MultiBackend {
    /// List all available backends.
    pub fn list_backends(&self) -> &[BackendInfo] {
        &self.backend_configs
    }

    /// Default backend name for chat use case.
    pub fn chat_default_name(&self) -> &str {
        &self.chat_backend_name
    }

    /// Get a specific backend by name, if it exists.
    pub fn get_backend_by_name(&self, name: &str) -> Option<Arc<dyn LlmBackend>> {
        self.use_case_backends
            .read()
            .ok()
            .and_then(|backends| {
                // Check use-case backends first, then check if it matches default
                backends.values().next().cloned()
            })
            // For now, we need a different approach - store named backends separately
    }
}
```

Actually, `MultiBackend` only stores backends by use-case name, not by config name. To support `/model` selecting a backend by config name, we need to also store a map of `name → Arc<dyn LlmBackend>`. Add:

```rust
pub struct MultiBackend {
    use_case_backends: RwLock<HashMap<String, Arc<dyn LlmBackend>>>,
    default_backend: Arc<dyn LlmBackend>,
    use_case_max_chars: HashMap<String, Option<usize>>,
    backend_configs: Vec<BackendInfo>,
    chat_backend_name: String,
    /// All backends by config name (for per-thread model selection).
    named_backends: HashMap<String, Arc<dyn LlmBackend>>,
}
```

In `MultiBackend::new()`, build `named_backends` by iterating `llm_config.backends` and creating each backend:

```rust
let mut named_backends = HashMap::new();
for (name, cfg) in &llm_config.backends {
    match create_backend(name, cfg) {
        Ok(backend) => {
            let backend = maybe_wrap_langfuse(backend, &langfuse_config);
            named_backends.insert(name.clone(), backend);
        }
        Err(e) => {
            tracing::warn!("backend '{}' failed to initialize: {}", name, e);
        }
    }
}
```

Note: backends are already created in the use_case loop - to avoid double-creation, create all backends once, then map use_cases to them:

```rust
// First pass: create all backends
let mut named_backends = HashMap::new();
for (name, cfg) in &llm_config.backends {
    match create_backend(name, cfg) {
        Ok(backend) => {
            let backend = maybe_wrap_langfuse(backend, &langfuse_config);
            named_backends.insert(name.clone(), backend);
        }
        Err(e) => {
            tracing::warn!("backend '{}' failed to initialize: {}", name, e);
        }
    }
}

// Second pass: map use cases to backends
let use_case_backends = RwLock::new(HashMap::new());
let mut use_case_max_chars = HashMap::new();
for (use_case_name, backend_name) in &llm_config.use_cases {
    if let Some(backend) = named_backends.get(backend_name) {
        use_case_backends.write().unwrap().insert(use_case_name.clone(), backend.clone());
        if let Some(cfg) = llm_config.backends.get(backend_name) {
            use_case_max_chars.insert(use_case_name.clone(), cfg.max_content_chars);
        }
    } else {
        tracing::warn!("backend '{}' not available for use case '{}'", backend_name, use_case_name);
    }
}

let default_backend = named_backends.get(&llm_config.default)
    .cloned()
    .or_else(|| named_backends.values().next().cloned())
    .ok_or_else(|| anyhow!("no LLM backends could be initialized"))?;
```

Then add the method:

```rust
/// Get backend by config name (for per-thread model override).
pub fn get_backend_by_name(&self, name: &str) -> Option<Arc<dyn LlmBackend>> {
    self.named_backends.get(name).cloned()
}
```

- [ ] **Step 4: Build and test**

Run: `cargo test -p omnish-llm`
Expected: Some tests may need updating due to struct changes. Fix any that fail.

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-llm/src/factory.rs
git commit -m "feat: add list_backends and get_backend_by_name to MultiBackend (#154)"
```

### Task 4: Handle `__cmd:models` in daemon

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`
- Modify: `crates/omnish-daemon/src/main.rs`

- [ ] **Step 1: Store LlmConfig in DaemonServer**

Not LlmConfig directly - store a reference to the `MultiBackend` (which already has `list_backends`). Actually, the LLM backend is already stored as `Option<Arc<dyn LlmBackend>>`. We need to downcast to `MultiBackend` - that's messy.

Better approach: pass `handle_builtin_command` the `llm` reference, and add a `list_backends` method to the `LlmBackend` trait (with a default empty implementation), so `MultiBackend` can override it.

Add to `crates/omnish-llm/src/backend.rs`:

```rust
/// Info about an available backend (for `/model` listing).
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackendInfo {
    pub name: String,
    pub model: String,
}

#[async_trait]
pub trait LlmBackend: Send + Sync {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse>;
    fn name(&self) -> &str;
    fn max_content_chars_for_use_case(&self, _use_case: UseCase) -> Option<usize> { None }
    /// List available backends (only meaningful for MultiBackend).
    fn list_backends(&self) -> Vec<BackendInfo> { vec![] }
    /// Default chat backend name.
    fn chat_default_name(&self) -> &str { "" }
    /// Get backend by config name (for per-thread override).
    fn get_backend_by_name(&self, _name: &str) -> Option<Arc<dyn LlmBackend>> { None }
}
```

Then implement these in `MultiBackend` in `factory.rs`.

- [ ] **Step 2: Handle `__cmd:models` in `handle_builtin_command`**

In `server.rs`, add a new branch in `handle_builtin_command`:

```rust
if sub.starts_with("models") {
    let thread_id = sub.strip_prefix("models ").unwrap_or("").trim();

    if let Some(ref backend) = llm_backend {
        let backends = backend.list_backends();
        if backends.is_empty() {
            return cmd_display("No LLM backends configured".to_string());
        }

        // Determine which backend is selected for this thread
        let selected_name = if !thread_id.is_empty() {
            let meta = conv_mgr.load_meta(thread_id);
            meta.model.unwrap_or_else(|| backend.chat_default_name().to_string())
        } else {
            backend.chat_default_name().to_string()
        };

        let models: Vec<serde_json::Value> = backends.iter().map(|b| {
            serde_json::json!({
                "name": b.name,
                "model": b.model,
                "selected": b.name == selected_name,
            })
        }).collect();

        return serde_json::json!({
            "display": "",
            "models": models,
        });
    } else {
        return cmd_display("No LLM backends configured".to_string());
    }
}
```

- [ ] **Step 3: Handle model-only ChatMessage**

In `handle_chat_message` at line 401, add model handling before the LLM call:

```rust
// Handle model override
if let Some(ref model_name) = cm.model {
    let mut meta = conv_mgr.load_meta(&cm.thread_id);
    meta.model = Some(model_name.clone());
    conv_mgr.save_meta(&cm.thread_id, &meta);
}

// Model-only message (no query) - just acknowledge
if cm.query.is_empty() {
    return vec![Message::Ack];
}
```

For backend selection, after `let backend = llm.as_ref().unwrap();`, add override logic:

```rust
// Check for per-thread model override
let meta = conv_mgr.load_meta(&cm.thread_id);
let effective_backend: Arc<dyn LlmBackend> = if let Some(ref model_name) = meta.model {
    backend.get_backend_by_name(model_name)
        .unwrap_or_else(|| backend.get_backend(use_case))
} else {
    backend.get_backend(use_case)
};
```

Note: `get_backend` is on `MultiBackend` but we have `Arc<dyn LlmBackend>`. Add `get_backend` variant to trait too, or use `get_backend_by_name` with the chat default. Simpler: just add a method `fn get_chat_backend(&self) -> Arc<dyn LlmBackend>` with default returning `self`.

Actually the simplest approach: the `complete()` method is what matters. We just need to call `complete()` on the right backend. If there's a per-thread override, get that backend and call `complete()` directly instead of going through `MultiBackend`.

Refactor `handle_chat_message` to resolve the effective backend early:

```rust
let base_backend = llm.as_ref().unwrap();

// Resolve per-thread model override
let meta = conv_mgr.load_meta(&cm.thread_id);
let effective_backend: Arc<dyn LlmBackend> = meta.model.as_ref()
    .and_then(|name| base_backend.get_backend_by_name(name))
    .unwrap_or_else(|| base_backend.clone());

let use_case = UseCase::Chat;
let max_context_chars = effective_backend.max_content_chars_for_use_case(use_case);
```

Then pass `&Some(effective_backend)` instead of `llm` to `run_agent_loop`.

- [ ] **Step 4: Build and test**

Run: `cargo build -p omnish-daemon`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-llm/src/backend.rs crates/omnish-llm/src/factory.rs crates/omnish-daemon/src/server.rs
git commit -m "feat: handle __cmd:models and per-thread model override (#154)"
```

---

## Chunk 3: Client Integration

### Task 5: Add `pick_one_at` to picker widget

**Files:**
- Modify: `crates/omnish-client/src/widgets/picker.rs:155,289-291`

- [ ] **Step 1: Add initial_cursor param to run_picker**

Change `run_picker` signature:
```rust
fn run_picker(title: &str, items: &[&str], multi: bool, initial_cursor: usize) -> Option<Vec<usize>> {
```

Change `let mut cursor: usize = 0;` to:
```rust
let mut cursor: usize = initial_cursor.min(items.len().saturating_sub(1));
let mut scroll_offset: usize = cursor.saturating_sub(visible_count(items.len()) / 2);
```

- [ ] **Step 2: Update pick_one and pick_many, add pick_one_at**

```rust
pub fn pick_one(title: &str, items: &[&str]) -> Option<usize> {
    run_picker(title, items, false, 0).map(|v| v[0])
}

pub fn pick_one_at(title: &str, items: &[&str], initial: usize) -> Option<usize> {
    run_picker(title, items, false, initial).map(|v| v[0])
}

pub fn pick_many(title: &str, items: &[&str]) -> Option<Vec<usize>> {
    run_picker(title, items, true, 0)
}
```

- [ ] **Step 3: Build and test**

Run: `cargo test -p omnish-client`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/widgets/picker.rs
git commit -m "feat: add pick_one_at with pre-selected index (#154)"
```

### Task 6: Add `/model` command and chat_session handling

**Files:**
- Modify: `crates/omnish-client/src/command.rs:248`
- Modify: `crates/omnish-client/src/chat_session.rs`

- [ ] **Step 1: Register `/model` as chat-only command**

In `command.rs`, update `CHAT_ONLY_COMMANDS`:
```rust
pub const CHAT_ONLY_COMMANDS: &[&str] = &["/resume", "/model"];
```

- [ ] **Step 2: Handle `/model` in chat_session.rs**

In `ChatSession`, add a field to store pending model selection for new threads:
```rust
pending_model: Option<String>,
```

Initialize to `None` in the constructor.

In the main command dispatch loop (where `/resume` is handled), add `/model` handling:

```rust
if trimmed == "/model" {
    self.handle_model(session_id, rpc).await;
    continue;
}
```

Add the handler method:

```rust
/// Strip date suffix from model name for display (e.g. "claude-sonnet-4-5-20250929" → "claude-sonnet-4-5").
fn strip_model_date(model: &str) -> &str {
    if let Some(pos) = model.rfind('-') {
        let suffix = &model[pos + 1..];
        if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
            return &model[..pos];
        }
    }
    model
}

async fn handle_model(&mut self, session_id: &str, rpc: &RpcClient) {
    // Build query with thread_id if available
    let query = match &self.current_thread_id {
        Some(tid) => format!("__cmd:models {}", tid),
        None => "__cmd:models".to_string(),
    };

    let rid = Uuid::new_v4().to_string()[..8].to_string();
    let req = Message::Request(Request {
        request_id: rid.clone(),
        session_id: session_id.to_string(),
        query,
        scope: RequestScope::AllSessions,
    });

    let models = match rpc.call(req).await {
        Ok(Message::Response(resp)) if resp.request_id == rid => {
            match super::parse_cmd_response(&resp.content) {
                Some(json) => json.get("models").and_then(|v| v.as_array()).cloned(),
                None => None,
            }
        }
        _ => None,
    };

    let models = match models {
        Some(m) if !m.is_empty() => m,
        _ => {
            write_stdout(&display::render_error("No LLM backends available"));
            return;
        }
    };

    // Build picker items and find selected index
    let mut selected_idx = 0;
    let item_strings: Vec<String> = models.iter().enumerate().map(|(i, m)| {
        let name = m["name"].as_str().unwrap_or("?");
        let model = m["model"].as_str().unwrap_or("?");
        let short_model = Self::strip_model_date(model);
        if m["selected"].as_bool().unwrap_or(false) {
            selected_idx = i;
        }
        format!("{} ({})", name, short_model)
    }).collect();
    let items: Vec<&str> = item_strings.iter().map(|s| s.as_str()).collect();

    match widgets::picker::pick_one_at("Select model:", &items, selected_idx) {
        Some(idx) if idx < models.len() => {
            let name = models[idx]["name"].as_str().unwrap_or("").to_string();
            let display_name = &item_strings[idx];

            if let Some(ref tid) = self.current_thread_id {
                // Existing thread - send model-only ChatMessage
                let rid = Uuid::new_v4().to_string()[..8].to_string();
                let msg = Message::ChatMessage(omnish_protocol::message::ChatMessage {
                    request_id: rid.clone(),
                    session_id: session_id.to_string(),
                    thread_id: tid.clone(),
                    query: String::new(),
                    model: Some(name),
                });
                match rpc.call(msg).await {
                    Ok(Message::Ack) => {
                        write_stdout(&format!("\x1b[2;90mSwitched to {}\x1b[0m\r\n", display_name));
                    }
                    _ => {
                        write_stdout(&display::render_error("Failed to switch model"));
                    }
                }
            } else {
                // New thread - defer model selection to first message
                self.pending_model = Some(name);
                write_stdout(&format!("\x1b[2;90mModel set to {} (will apply on first message)\x1b[0m\r\n", display_name));
            }
        }
        _ => {} // ESC or no selection - do nothing
    }
}
```

- [ ] **Step 3: Attach pending_model to first ChatMessage**

In the ChatMessage construction (around line 343), attach the pending model:

```rust
let chat_msg = Message::ChatMessage(omnish_protocol::message::ChatMessage {
    request_id: req_id.clone(),
    session_id: session_id.to_string(),
    thread_id: self.current_thread_id.clone().unwrap(),
    query: trimmed.to_string(),
    model: self.pending_model.take(),
});
```

- [ ] **Step 4: Build**

Run: `cargo build -p omnish-client`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/src/command.rs crates/omnish-client/src/chat_session.rs
git commit -m "feat: add /model picker command in chat mode (#154)"
```

---

## Chunk 4: Verification

### Task 7: Build & test full workspace

- [ ] **Step 1: Build release**

Run: `cargo build --release`
Expected: PASS

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace`
Expected: PASS

- [ ] **Step 3: Manual test**

1. Start daemon, start client
2. Enter chat mode (`:`)
3. Type `/model` - should show picker with backends from `daemon.toml`, current one pre-selected
4. Select a different model - should show "Switched to ..."
5. Send a message - should use the selected model (check daemon logs)
6. Exit chat, re-enter, `/resume` the thread - model should still be set
7. Type `/model` on a fresh thread (before first message) - should show picker, selection deferred
8. Send first message - should use the selected model

- [ ] **Step 4: Commit and close issue**

```bash
git push
glab issue note 154 -m "Implemented in <commit>: /model picker for per-thread model selection"
glab issue close 154
```
