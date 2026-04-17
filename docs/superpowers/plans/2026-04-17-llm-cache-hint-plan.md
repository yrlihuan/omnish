# LLM Cache Hint Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lift `cache_control` placement decisions out of `anthropic.rs` into the daemon, exposed as a backend-agnostic `CacheHint` carried on each cacheable unit of `LlmRequest`.

**Architecture:** Introduce `CacheHint` enum (`None | Short | Long`) inlined into `CachedText` (system prompt), `ToolDef.cache`, and a new `TaggedMessage` wrapper for `extra_messages`. Anthropic backend reads each hint, enforces the 4-breakpoint budget by retaining the latest message markers, and writes wire `cache_control` accordingly. OpenAI-compat backend ignores all hints. The dead `conversation: Vec<ChatTurn>` field is removed during the same migration.

**Tech Stack:** Rust workspace, `anyhow`, `serde_json`, `tracing`. Build with `cargo build --release` (per project rule). Tests run via `cargo test -p omnish-llm` (and `-p omnish-daemon` where touched). Integration tests under `tools/integration_tests`.

---

### Task 1: Add `CacheHint` enum

**Files:**
- Modify: `crates/omnish-llm/src/backend.rs` (add enum near top of file, after the existing imports)
- Test: `crates/omnish-llm/tests/cache_hint_test.rs` (create)

- [ ] **Step 1: Write the failing test for CacheHint defaults & equality**

Create `crates/omnish-llm/tests/cache_hint_test.rs`:

```rust
use omnish_llm::backend::CacheHint;

#[test]
fn cache_hint_default_is_none() {
    assert_eq!(CacheHint::default(), CacheHint::None);
}

#[test]
fn cache_hint_variants_distinct() {
    assert_ne!(CacheHint::Short, CacheHint::Long);
    assert_ne!(CacheHint::Short, CacheHint::None);
    assert_ne!(CacheHint::Long, CacheHint::None);
}

#[test]
fn cache_hint_is_copy() {
    let h = CacheHint::Long;
    let h2 = h;
    assert_eq!(h, h2);
}
```

- [ ] **Step 2: Run test to verify it fails (compile error: type not found)**

Run: `cargo test -p omnish-llm --test cache_hint_test`
Expected: FAIL - `cannot find type 'CacheHint' in module 'omnish_llm::backend'`

- [ ] **Step 3: Add CacheHint enum to backend.rs**

Add to `crates/omnish-llm/src/backend.rs`, right after the `BackendInfo` struct (around line 13):

```rust
/// Backend-agnostic cache lifetime hint.
/// Anthropic backend translates this into `cache_control` TTL.
/// OpenAI-compat backend ignores this entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheHint {
    #[default]
    None,
    /// Anthropic: ephemeral with default 5min TTL.
    Short,
    /// Anthropic: ephemeral with `ttl: "1h"`.
    Long,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-llm --test cache_hint_test`
Expected: PASS - all three tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-llm/src/backend.rs crates/omnish-llm/tests/cache_hint_test.rs
git commit -m "feat(llm): add CacheHint enum"
```

---

### Task 2: Add `CachedText` and `TaggedMessage` wrapper types

**Files:**
- Modify: `crates/omnish-llm/src/backend.rs` (add structs after CacheHint)
- Test: `crates/omnish-llm/tests/cache_hint_test.rs` (extend)

- [ ] **Step 1: Add tests for CachedText and TaggedMessage**

Append to `crates/omnish-llm/tests/cache_hint_test.rs`:

```rust
use omnish_llm::backend::{CachedText, TaggedMessage};

#[test]
fn cached_text_constructs_with_hint() {
    let ct = CachedText { text: "hello".into(), cache: CacheHint::Long };
    assert_eq!(ct.text, "hello");
    assert_eq!(ct.cache, CacheHint::Long);
}

#[test]
fn tagged_message_default_hint_is_none() {
    let m = TaggedMessage {
        content: serde_json::json!({"role":"user","content":"hi"}),
        cache: CacheHint::default(),
    };
    assert_eq!(m.cache, CacheHint::None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-llm --test cache_hint_test`
Expected: FAIL - `cannot find type 'CachedText'`, `cannot find type 'TaggedMessage'`

- [ ] **Step 3: Add wrapper structs to backend.rs**

Add to `crates/omnish-llm/src/backend.rs`, right after the `CacheHint` enum:

```rust
/// A cacheable text payload (used for `LlmRequest.system_prompt`).
#[derive(Debug, Clone)]
pub struct CachedText {
    pub text: String,
    pub cache: CacheHint,
}

/// A message wrapped with a cache hint (used for `LlmRequest.extra_messages`).
/// `content` is raw Anthropic-format JSON (canonical internal format).
#[derive(Debug, Clone)]
pub struct TaggedMessage {
    pub content: serde_json::Value,
    pub cache: CacheHint,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-llm --test cache_hint_test`
Expected: PASS - five tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-llm/src/backend.rs crates/omnish-llm/tests/cache_hint_test.rs
git commit -m "feat(llm): add CachedText and TaggedMessage wrappers"
```

---

### Task 3: Add `cache` field to `ToolDef`

**Files:**
- Modify: `crates/omnish-llm/src/tool.rs` (add field)
- Modify: `crates/omnish-daemon/src/plugin.rs` (3 ToolDef construction sites)
- Modify: `crates/omnish-daemon/src/tools/command_query.rs` (2 sites)
- Modify: `crates/omnish-daemon/src/tool_registry.rs` (3 sites in `#[cfg(test)]`)
- Test: `crates/omnish-llm/tests/cache_hint_test.rs` (extend)

The `cache` field is `serde(default)`-ed so existing `tool.json`/serialized forms still deserialize without it.

- [ ] **Step 1: Write the failing test**

Append to `crates/omnish-llm/tests/cache_hint_test.rs`:

```rust
use omnish_llm::tool::ToolDef;

#[test]
fn tool_def_cache_defaults_to_none_on_deserialize() {
    let json = serde_json::json!({
        "name": "my_tool",
        "description": "desc",
        "input_schema": {"type": "object"}
    });
    let td: ToolDef = serde_json::from_value(json).unwrap();
    assert_eq!(td.cache, omnish_llm::backend::CacheHint::None);
}

#[test]
fn tool_def_cache_serialized_roundtrip() {
    let td = ToolDef {
        name: "x".into(),
        description: "y".into(),
        input_schema: serde_json::json!({}),
        cache: omnish_llm::backend::CacheHint::Long,
    };
    let v = serde_json::to_value(&td).unwrap();
    let td2: ToolDef = serde_json::from_value(v).unwrap();
    assert_eq!(td2.cache, omnish_llm::backend::CacheHint::Long);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-llm --test cache_hint_test`
Expected: FAIL - `ToolDef` has no `cache` field; serde fails or struct construction errors.

- [ ] **Step 3: Make `CacheHint` serializable & add field to `ToolDef`**

In `crates/omnish-llm/src/backend.rs`, change the `CacheHint` derive line to also include `Serialize, Deserialize`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum CacheHint {
    #[default]
    None,
    Short,
    Long,
}
```

In `crates/omnish-llm/src/tool.rs`, replace the `ToolDef` struct:

```rust
/// Definition of a tool that can be provided to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// Cache hint applied to this tool's wire entry. Defaults to `None`
    /// so callers (and JSON-deserialized plugin defs) need not specify it.
    #[serde(default)]
    pub cache: crate::backend::CacheHint,
}
```

- [ ] **Step 4: Update all `ToolDef { ... }` literal construction sites to include `cache: CacheHint::None`**

Sites to update (use grep `ToolDef\s*\{` if you need to verify):

`crates/omnish-daemon/src/plugin.rs` - 3 sites (around lines 244, 339, 582). Each currently looks like:

```rust
def: ToolDef {
    name: te.name,
    description: te.description.into_string(),
    input_schema: te.input_schema,
},
```

Change to (add the import at the top of the file if missing: `use omnish_llm::backend::CacheHint;`):

```rust
def: ToolDef {
    name: te.name,
    description: te.description.into_string(),
    input_schema: te.input_schema,
    cache: CacheHint::None,
},
```

`crates/omnish-daemon/src/tools/command_query.rs` - 2 sites (around lines 285, 301). Add `cache: omnish_llm::backend::CacheHint::None,` to each.

`crates/omnish-daemon/src/tool_registry.rs` - 3 sites in `#[cfg(test)]` block (around lines 334, 343, 369). Add `cache: omnish_llm::backend::CacheHint::None,` to each.

- [ ] **Step 5: Run tests to verify**

Run: `cargo test -p omnish-llm --test cache_hint_test`
Expected: PASS - both new tests pass.

Run: `cargo build --release -p omnish-daemon`
Expected: builds clean (no field-missing errors).

Run: `cargo test -p omnish-daemon --lib tool_registry`
Expected: existing tool_registry tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-llm/src/backend.rs crates/omnish-llm/src/tool.rs \
        crates/omnish-daemon/src/plugin.rs \
        crates/omnish-daemon/src/tools/command_query.rs \
        crates/omnish-daemon/src/tool_registry.rs \
        crates/omnish-llm/tests/cache_hint_test.rs
git commit -m "feat(llm): add cache hint field to ToolDef"
```

---

### Task 4: Migrate `LlmRequest` field types and remove `conversation`

This is a coordinated change: the field types of `system_prompt` and `extra_messages` change, and `conversation` is deleted. All 7 call sites and both backends must be updated atomically for the workspace to compile.

**Files:**
- Modify: `crates/omnish-llm/src/backend.rs` (LlmRequest struct)
- Modify: `crates/omnish-llm/src/anthropic.rs` (build messages, system, ChatTurn loop)
- Modify: `crates/omnish-llm/src/openai_compat.rs` (build messages, system, ChatTurn loop)
- Modify: `crates/omnish-llm/tests/llm_test.rs` (update existing test)
- Modify: `crates/omnish-daemon/src/server.rs` (call sites at lines ~377, ~1266, ~1991, ~2802, ~2883; in-loop pushes)
- Modify: `crates/omnish-daemon/src/hourly_summary.rs` (line 191)
- Modify: `crates/omnish-daemon/src/daily_notes.rs` (line 137)
- Modify: `crates/omnish-daemon/src/thread_summary.rs` (line 111)

This task changes types but **preserves existing behavior**: in step 5, the Anthropic backend keeps its old hardcoded cache placement (system always cached, last tool cached, second-to-last message cached). Cache *strategy* moves to the caller in Task 7, after the type migration is in.

- [ ] **Step 1: Update `LlmRequest` struct in backend.rs**

In `crates/omnish-llm/src/backend.rs`, replace the `LlmRequest` struct with:

```rust
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
    /// Optional system prompt (e.g., chat mode system prompt).
    pub system_prompt: Option<CachedText>,
    /// Whether to enable extended thinking mode (e.g., Claude extended thinking, DeepSeek R1).
    /// None means use backend default. Set to false to disable, true to enable.
    pub enable_thinking: Option<bool>,
    /// Tool definitions to provide to the LLM. Empty means no tools.
    pub tools: Vec<ToolDef>,
    /// Messages for multi-turn / agent loop. Each carries an optional cache hint.
    /// Content is raw Anthropic-format JSON (canonical internal format).
    pub extra_messages: Vec<TaggedMessage>,
}
```

Notes:
- `conversation: Vec<ChatTurn>` field is removed.
- `extra_messages` type changes from `Vec<serde_json::Value>` to `Vec<TaggedMessage>`.
- `system_prompt` type changes from `Option<String>` to `Option<CachedText>`.
- Imports at top of file: `use crate::tool::{ToolCall, ToolDef};` is already present; no new imports needed (CachedText/TaggedMessage are defined in the same file).
- The `use omnish_protocol::message::ChatTurn;` import (if any in this file) becomes unused - remove it.

- [ ] **Step 2: Update `crates/omnish-llm/src/anthropic.rs` for the new types**

Replace the message-building block (lines ~60–95) to drop the `conversation` branch and read from the new `TaggedMessage` shape. The hardcoded "second-to-last cache_control" stays for now - we replace it in Task 7.

Replace this block:

```rust
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
    // Mark the second-to-last message with cache_control so the stable
    // prefix is cached.  The last message contains a system-reminder that
    // changes between requests, so it must NOT be the cache boundary.
    let len = msgs.len();
    if len >= 2 {
        inject_cache_control(&mut msgs[len - 2]);
    }
    msgs
};
```

with:

```rust
let messages: Vec<serde_json::Value> = if req.extra_messages.is_empty() {
    // Single-turn fallback: build a synthetic user message from context+query.
    let user_content = crate::template::build_user_content(
        &req.context,
        req.query.as_deref(),
    );
    vec![serde_json::json!({"role": "user", "content": user_content})]
} else {
    // Multi-turn / agent loop: extract raw JSON from TaggedMessage wrappers.
    let mut msgs: Vec<serde_json::Value> = req.extra_messages
        .iter()
        .map(|m| m.content.clone())
        .collect();
    // Preserve existing behavior for now: mark the second-to-last message.
    // Replaced in a later task with hint-driven placement.
    let len = msgs.len();
    if len >= 2 {
        inject_cache_control(&mut msgs[len - 2]);
    }
    msgs
};
```

Replace the system-prompt insertion block (lines ~104–108):

```rust
if let Some(ref system) = req.system_prompt {
    body_map.insert("system".to_string(), serde_json::json!([
        {"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}
    ]));
}
```

with:

```rust
if let Some(ref system) = req.system_prompt {
    body_map.insert("system".to_string(), serde_json::json!([
        {"type": "text", "text": system.text, "cache_control": {"type": "ephemeral"}}
    ]));
}
```

(Behavior preserved: still injects `cache_control` regardless of `system.cache`. Hint-driven placement comes in Task 7.)

- [ ] **Step 3: Update `crates/omnish-llm/src/openai_compat.rs` for the new types**

Replace the message-building block (lines ~150–178) to drop the `conversation` branch:

```rust
let mut messages: Vec<serde_json::Value> = if req.conversation.is_empty() && req.extra_messages.is_empty() {
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
    // Convert and append extra messages (tool_use/tool_result exchanges)
    msgs.extend(convert_extra_messages(&req.extra_messages));
    msgs
};
```

with:

```rust
let mut messages: Vec<serde_json::Value> = if req.extra_messages.is_empty() {
    // Single-turn fallback.
    let user_content = crate::template::build_user_content(
        &req.context,
        req.query.as_deref(),
    );
    vec![serde_json::json!({"role": "user", "content": user_content})]
} else {
    // Multi-turn / agent loop: extract content, then convert Anthropic→OpenAI format.
    let raw: Vec<serde_json::Value> = req.extra_messages
        .iter()
        .map(|m| m.content.clone())
        .collect();
    convert_extra_messages(&raw)
};
```

Update the system-prompt insertion (lines ~180–183):

```rust
if let Some(ref system) = req.system_prompt {
    messages.insert(0, serde_json::json!({"role": "system", "content": system}));
}
```

with:

```rust
if let Some(ref system) = req.system_prompt {
    messages.insert(0, serde_json::json!({"role": "system", "content": system.text}));
}
```

The `convert_extra_messages` signature stays the same (takes `&[serde_json::Value]`). No changes inside it.

- [ ] **Step 4: Update `crates/omnish-llm/tests/llm_test.rs`**

Replace the file contents with:

```rust
use omnish_llm::backend::{LlmRequest, TriggerType, UseCase};

#[test]
fn test_llm_request_build() {
    let req = LlmRequest {
        context: "$ ls\nfile.txt\n$ cat file.txt\nhello".to_string(),
        query: Some("what is in file.txt?".to_string()),
        trigger: TriggerType::Manual,
        session_ids: vec!["abc".to_string()],
        use_case: UseCase::Analysis,
        max_content_chars: None,
        system_prompt: None,
        enable_thinking: None,
        tools: vec![],
        extra_messages: vec![],
    };
    assert_eq!(req.session_ids.len(), 1);
    assert!(req.query.is_some());
}
```

(Removed `conversation: vec![]` field.)

- [ ] **Step 5: Update non-chat call sites (5 files)**

For each of the following sites, change `system_prompt` from `Some(String)` / `None` to `Some(CachedText { text: ..., cache: CacheHint::None })` / `None`, and change `extra_messages: vec![]` (already empty Vec) - type now infers as `Vec<TaggedMessage>`. Remove `conversation: vec![],`.

Add `use omnish_llm::backend::{CacheHint, CachedText, TaggedMessage};` at the top of each file if needed.

**`crates/omnish-daemon/src/server.rs` line ~377** (`summarize_tool_result`):

Find:
```rust
let req = omnish_llm::backend::LlmRequest {
    context: String::new(),
    query: Some(query),
    trigger: omnish_llm::backend::TriggerType::Manual,
    session_ids: vec![],
    use_case: omnish_llm::backend::UseCase::Summarize,
    max_content_chars: None,
    conversation: vec![],
    system_prompt: None,
    enable_thinking: Some(false),
    tools: vec![],
    extra_messages: vec![],
};
```

Replace with:
```rust
let req = omnish_llm::backend::LlmRequest {
    context: String::new(),
    query: Some(query),
    trigger: omnish_llm::backend::TriggerType::Manual,
    session_ids: vec![],
    use_case: omnish_llm::backend::UseCase::Summarize,
    max_content_chars: None,
    system_prompt: None,
    enable_thinking: Some(false),
    tools: vec![],
    extra_messages: vec![],
};
```

(Just removes the `conversation: vec![],` line. `vec![]` for `extra_messages` infers correctly to `Vec<TaggedMessage>`.)

**`crates/omnish-daemon/src/hourly_summary.rs` line ~191:** Same treatment - drop `conversation: vec![],`. The `system_prompt: None` line stays as-is.

**`crates/omnish-daemon/src/daily_notes.rs` line ~137:** Same.

**`crates/omnish-daemon/src/thread_summary.rs` line ~111:** Same.

- [ ] **Step 6: Update chat agent loop call sites in server.rs (lines ~1266, ~2802)**

At line ~1266 (chat agent loop entry point), the existing block:

```rust
let llm_req = LlmRequest {
    context: String::new(),
    query: None,
    trigger: TriggerType::Manual,
    session_ids: vec![cm.session_id.clone()],
    use_case,
    max_content_chars: max_context_chars,
    conversation: vec![],
    system_prompt: Some(full_system_prompt),
    enable_thinking: Some(true), // Enable thinking mode for chat
    tools,
    extra_messages,
};
```

The `extra_messages` variable above is currently `Vec<serde_json::Value>` from `conv_mgr.load_raw_messages`. We need to wrap each into `TaggedMessage`. Look at the surrounding code (lines ~1239–1264):

```rust
// Load prior conversation history as raw JSON
let mut extra_messages = conv_mgr.load_raw_messages(&cm.thread_id);
// ... sanitize / strip metadata / push user_msg ...
extra_messages.push(user_msg.clone());
```

Change `extra_messages` declaration to wrap into `TaggedMessage` after all the prep work. The cleanest way: keep the existing prep on `Vec<serde_json::Value>` (named `raw_messages`), then convert at the end.

Replace the entire block from line ~1239 to ~1278 with:

```rust
// Load prior conversation history as raw JSON
let mut raw_messages = conv_mgr.load_raw_messages(&cm.thread_id);

// Sanitize orphaned tool_use blocks that can appear when a ChatInterrupt
// races with a new ChatMessage (both are dispatched concurrently).
if omnish_daemon::conversation_mgr::sanitize_orphaned_tool_use(&mut raw_messages) {
    tracing::warn!("Sanitized orphaned tool_use blocks before chat (thread={})", cm.thread_id);
    conv_mgr.replace_messages(&cm.thread_id, &raw_messages);
}

// Strip internal metadata fields that must not be sent to the LLM API
for msg in &mut raw_messages {
    if let Some(obj) = msg.as_object_mut() {
        obj.remove("_usage");
        obj.remove("_model");
    }
}
let prior_len = raw_messages.len();

// User message (clean, without system-reminder)
let user_msg = serde_json::json!({"role": "user", "content": cm.query});
raw_messages.push(user_msg.clone());

// Persist user message immediately so /resume works even if the agent loop
// hasn't finished (each message is handled in its own spawned task, so
// ChatEnd can race with the agent loop).
conv_mgr.append_messages(&cm.thread_id, &[user_msg]);

// Wrap raw messages into TaggedMessage. Cache hints are applied per-iteration
// inside the agent loop (see mark_message_cache_hints in run_agent_loop).
let extra_messages: Vec<omnish_llm::backend::TaggedMessage> = raw_messages
    .into_iter()
    .map(|content| omnish_llm::backend::TaggedMessage {
        content,
        cache: omnish_llm::backend::CacheHint::None,
    })
    .collect();

let llm_req = LlmRequest {
    context: String::new(),
    query: None,
    trigger: TriggerType::Manual,
    session_ids: vec![cm.session_id.clone()],
    use_case,
    max_content_chars: max_context_chars,
    system_prompt: Some(omnish_llm::backend::CachedText {
        text: full_system_prompt,
        cache: omnish_llm::backend::CacheHint::None, // Task 7 sets to Long
    }),
    enable_thinking: Some(true),
    tools,
    extra_messages,
};
```

(Note: cache hints stay `None` here. Task 7 will flip these to `Long`. The behavior preservation comes from anthropic.rs still injecting cache_control unconditionally for now.)

At line ~2802 (the other chat code path - find by searching for `LlmRequest` near `enable_thinking: Some(true), // Enable thinking mode for chat` and `tools: vec![]`):

```rust
let llm_req = LlmRequest {
    context,
    query: Some(req.query.clone()),
    trigger: TriggerType::Manual,
    session_ids: vec![req.session_id.clone()],
    use_case,
    max_content_chars: max_context_chars,
    conversation: vec![],
    system_prompt: None,
    enable_thinking: Some(true), // Enable thinking mode for chat
    tools: vec![],
    extra_messages: vec![],
};
```

Replace with (drop `conversation`, leave rest):

```rust
let llm_req = LlmRequest {
    context,
    query: Some(req.query.clone()),
    trigger: TriggerType::Manual,
    session_ids: vec![req.session_id.clone()],
    use_case,
    max_content_chars: max_context_chars,
    system_prompt: None,
    enable_thinking: Some(true),
    tools: vec![],
    extra_messages: vec![],
};
```

- [ ] **Step 7: Update KV cache warmup site at server.rs line ~1991**

Find:
```rust
let req = LlmRequest {
    context: String::new(),
    query: Some(prompt),
    trigger: TriggerType::Manual,
    session_ids: vec![session_id.to_string()],
    use_case: UseCase::Completion,
    max_content_chars: max_chars,
    conversation: vec![],
    system_prompt: None,
    enable_thinking: Some(false),
    tools: vec![],
    extra_messages: vec![],
};
```

Replace with (drop `conversation`):
```rust
let req = LlmRequest {
    context: String::new(),
    query: Some(prompt),
    trigger: TriggerType::Manual,
    session_ids: vec![session_id.to_string()],
    use_case: UseCase::Completion,
    max_content_chars: max_chars,
    system_prompt: None,
    enable_thinking: Some(false),
    tools: vec![],
    extra_messages: vec![],
};
```

- [ ] **Step 8: Update completion main path at server.rs line ~2883**

Find:
```rust
let llm_req = LlmRequest {
    context: String::new(),
    query: Some(prompt),
    trigger: TriggerType::Manual,
    session_ids: vec![req.session_id.clone()],
    use_case,
    max_content_chars: max_context_chars,
    conversation: vec![],
    system_prompt: None,
    enable_thinking: Some(false),
    tools: vec![],
    extra_messages: vec![],
};
```

Replace with (drop `conversation`):
```rust
let llm_req = LlmRequest {
    context: String::new(),
    query: Some(prompt),
    trigger: TriggerType::Manual,
    session_ids: vec![req.session_id.clone()],
    use_case,
    max_content_chars: max_context_chars,
    system_prompt: None,
    enable_thinking: Some(false),
    tools: vec![],
    extra_messages: vec![],
};
```

- [ ] **Step 9: Update agent-loop in-place pushes (server.rs)**

The agent loop pushes to `state.llm_req.extra_messages` in five places. Currently each push is `serde_json::json!(...)` directly into a `Vec<serde_json::Value>`. After the type change, each must wrap in `TaggedMessage`.

Find each `state.llm_req.extra_messages.push(serde_json::json!({...}))` and replace with:

```rust
state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage {
    content: serde_json::json!({...}),  // unchanged inner expression
    cache: omnish_llm::backend::CacheHint::None,
});
```

Sites (use grep `state\.llm_req\.extra_messages\.push` to confirm):
- Line ~1403 (after tool execution result)
- Line ~1465 (interrupted tool_use sanitization in `persist_unsaved_sanitized`)
- Line ~1601 (assistant response from LLM)
- Line ~1825 (interrupted user)
- Line ~1862 (more results)

Also update read-side:
- Line ~1440: `state.llm_req.extra_messages.last().is_some_and(|msg| { ... msg.get("role") ... })` - `msg` is now `&TaggedMessage`, so change to `msg.content.get("role")`. The closure body that iterates `blocks.iter().any(|b| b.get(...))` reads from `msg.get("content")` - change to `msg.content.get("content")`. (Note the `msg` here is the TaggedMessage; `.content` is the JSON field; the inner `content` field of the JSON object is a separate thing.)

Specifically replace:

```rust
let last_is_tool_use = state.llm_req.extra_messages.last().is_some_and(|msg| {
    msg.get("role").and_then(|r| r.as_str()) == Some("assistant")
        && msg.get("content").and_then(|c| c.as_array()).is_some_and(|blocks| {
            blocks.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        })
});
```

with:

```rust
let last_is_tool_use = state.llm_req.extra_messages.last().is_some_and(|msg| {
    msg.content.get("role").and_then(|r| r.as_str()) == Some("assistant")
        && msg.content.get("content").and_then(|c| c.as_array()).is_some_and(|blocks| {
            blocks.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        })
});
```

And shortly after:
```rust
let tool_ids: Vec<String> = state.llm_req.extra_messages.last().unwrap()
    .get("content").unwrap().as_array().unwrap()
    ...
```

Replace with:
```rust
let tool_ids: Vec<String> = state.llm_req.extra_messages.last().unwrap()
    .content.get("content").unwrap().as_array().unwrap()
    ...
```

Also update the persist-slicing logic (line ~1485):

```rust
let mut to_store = state.llm_req.extra_messages[state.saved_up_to..].to_vec();
to_store.extend_from_slice(suffix);
```

`to_store` will now be `Vec<TaggedMessage>`, but `conv_mgr.append_messages` takes `&[serde_json::Value]`. Convert:

```rust
let mut to_store: Vec<serde_json::Value> = state.llm_req.extra_messages[state.saved_up_to..]
    .iter()
    .map(|m| m.content.clone())
    .collect();
to_store.extend_from_slice(suffix);
```

- [ ] **Step 10: Build & run tests**

Run: `cargo build --release`
Expected: workspace builds clean.

Run: `cargo test -p omnish-llm`
Expected: all tests pass (cache_hint_test + llm_test).

Run: `cargo test -p omnish-daemon`
Expected: existing tests pass.

If any compile error mentions `ChatTurn` being unused, remove the `use omnish_protocol::message::ChatTurn;` import from `backend.rs`.

- [ ] **Step 11: Commit**

```bash
git add -u
git commit -m "refactor(llm): migrate LlmRequest to typed cache hint carriers

- Wrap system_prompt: Option<String> -> Option<CachedText>
- Wrap extra_messages: Vec<Value> -> Vec<TaggedMessage>
- Add cache field to ToolDef
- Remove dead conversation: Vec<ChatTurn> field
- Behavior preserved: Anthropic backend still applies legacy cache placement"
```

---

### Task 5: Implement Anthropic backend hint-driven translation

Now we replace the legacy hardcoded cache placement with logic that reads `CacheHint` from each unit. Behavior up through Task 4 was preserved by leaving the old logic in; this task swaps it out.

**Files:**
- Modify: `crates/omnish-llm/src/anthropic.rs`
- Test: `crates/omnish-llm/tests/cache_hint_test.rs` (extend with backend translation tests)

To test the backend output without a network call, factor the body construction into a pure function `build_request_body(&LlmRequest, &str /* model */) -> serde_json::Value`, and call it from `complete()`. Tests assert against the body JSON.

- [ ] **Step 1: Write failing test for translation contract**

Append to `crates/omnish-llm/tests/cache_hint_test.rs`:

```rust
use omnish_llm::anthropic::build_request_body_for_test;
use omnish_llm::backend::{LlmRequest, TriggerType, UseCase};

fn empty_req() -> LlmRequest {
    LlmRequest {
        context: String::new(),
        query: None,
        trigger: TriggerType::Manual,
        session_ids: vec![],
        use_case: UseCase::Chat,
        max_content_chars: None,
        system_prompt: None,
        enable_thinking: None,
        tools: vec![],
        extra_messages: vec![],
    }
}

#[test]
fn anthropic_system_long_emits_1h_ttl() {
    let mut req = empty_req();
    req.system_prompt = Some(omnish_llm::backend::CachedText {
        text: "you are helpful".into(),
        cache: omnish_llm::backend::CacheHint::Long,
    });
    req.extra_messages = vec![omnish_llm::backend::TaggedMessage {
        content: serde_json::json!({"role":"user","content":"hi"}),
        cache: omnish_llm::backend::CacheHint::None,
    }];
    let body = build_request_body_for_test(&req, "test-model");
    let sys_block = &body["system"][0];
    assert_eq!(sys_block["text"], "you are helpful");
    assert_eq!(sys_block["cache_control"]["type"], "ephemeral");
    assert_eq!(sys_block["cache_control"]["ttl"], "1h");
}

#[test]
fn anthropic_system_short_omits_ttl() {
    let mut req = empty_req();
    req.system_prompt = Some(omnish_llm::backend::CachedText {
        text: "sys".into(),
        cache: omnish_llm::backend::CacheHint::Short,
    });
    req.extra_messages = vec![omnish_llm::backend::TaggedMessage {
        content: serde_json::json!({"role":"user","content":"hi"}),
        cache: omnish_llm::backend::CacheHint::None,
    }];
    let body = build_request_body_for_test(&req, "test-model");
    let cc = &body["system"][0]["cache_control"];
    assert_eq!(cc["type"], "ephemeral");
    assert!(cc.get("ttl").is_none(), "Short hint must not emit ttl field, got {:?}", cc);
}

#[test]
fn anthropic_system_none_emits_no_cache_control() {
    let mut req = empty_req();
    req.system_prompt = Some(omnish_llm::backend::CachedText {
        text: "sys".into(),
        cache: omnish_llm::backend::CacheHint::None,
    });
    req.extra_messages = vec![omnish_llm::backend::TaggedMessage {
        content: serde_json::json!({"role":"user","content":"hi"}),
        cache: omnish_llm::backend::CacheHint::None,
    }];
    let body = build_request_body_for_test(&req, "test-model");
    assert!(body["system"][0].get("cache_control").is_none());
}

#[test]
fn anthropic_message_cache_marks_last_block() {
    let mut req = empty_req();
    req.extra_messages = vec![
        omnish_llm::backend::TaggedMessage {
            content: serde_json::json!({"role":"user","content":"a"}),
            cache: omnish_llm::backend::CacheHint::None,
        },
        omnish_llm::backend::TaggedMessage {
            content: serde_json::json!({"role":"user","content":"b"}),
            cache: omnish_llm::backend::CacheHint::Long,
        },
    ];
    let body = build_request_body_for_test(&req, "test-model");
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    // first message: no cache_control anywhere
    assert!(messages[0]["content"].as_array().is_none() || messages[0]["content"][0].get("cache_control").is_none());
    // second message: content was a string, becomes array with cache_control on the (single) block
    let last_block = &messages[1]["content"][0];
    assert_eq!(last_block["cache_control"]["type"], "ephemeral");
    assert_eq!(last_block["cache_control"]["ttl"], "1h");
}

#[test]
fn anthropic_tool_cache_marks_that_tool() {
    let mut req = empty_req();
    req.tools = vec![
        omnish_llm::tool::ToolDef {
            name: "a".into(), description: "ad".into(),
            input_schema: serde_json::json!({"type":"object"}),
            cache: omnish_llm::backend::CacheHint::None,
        },
        omnish_llm::tool::ToolDef {
            name: "b".into(), description: "bd".into(),
            input_schema: serde_json::json!({"type":"object"}),
            cache: omnish_llm::backend::CacheHint::Long,
        },
    ];
    req.extra_messages = vec![omnish_llm::backend::TaggedMessage {
        content: serde_json::json!({"role":"user","content":"x"}),
        cache: omnish_llm::backend::CacheHint::None,
    }];
    let body = build_request_body_for_test(&req, "test-model");
    let tools = body["tools"].as_array().unwrap();
    assert!(tools[0].get("cache_control").is_none());
    assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
    assert_eq!(tools[1]["cache_control"]["ttl"], "1h");
}

#[test]
fn anthropic_budget_keeps_last_n_message_marks() {
    let mut req = empty_req();
    req.system_prompt = Some(omnish_llm::backend::CachedText {
        text: "s".into(), cache: omnish_llm::backend::CacheHint::Long,
    });
    req.tools = vec![omnish_llm::tool::ToolDef {
        name: "t".into(), description: "d".into(),
        input_schema: serde_json::json!({"type":"object"}),
        cache: omnish_llm::backend::CacheHint::Long,
    }];
    // 5 marked messages, budget = 4 - 2 = 2 remaining → keep last 2 (indices 3,4)
    req.extra_messages = (0..5).map(|i| omnish_llm::backend::TaggedMessage {
        content: serde_json::json!({"role":"user","content": format!("m{}", i)}),
        cache: omnish_llm::backend::CacheHint::Long,
    }).collect();

    let body = build_request_body_for_test(&req, "test-model");
    let messages = body["messages"].as_array().unwrap();
    let has_cache = |idx: usize| -> bool {
        let c = &messages[idx]["content"];
        if let Some(arr) = c.as_array() {
            arr.iter().any(|b| b.get("cache_control").is_some())
        } else {
            false
        }
    };
    assert!(!has_cache(0), "msg 0 should be dropped");
    assert!(!has_cache(1), "msg 1 should be dropped");
    assert!(!has_cache(2), "msg 2 should be dropped");
    assert!(has_cache(3), "msg 3 should be kept");
    assert!(has_cache(4), "msg 4 should be kept");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-llm --test cache_hint_test`
Expected: FAIL - `build_request_body_for_test` not found in `omnish_llm::anthropic`.

- [ ] **Step 3: Refactor anthropic.rs to expose a pure body-builder + implement hint translation**

In `crates/omnish-llm/src/anthropic.rs`:

1. Delete `inject_cache_control` function (no longer used).
2. Add the hint translation helpers and budget enforcement.
3. Extract the body-building logic into a pure function.

Replace the file's body-building section. The new structure:

```rust
use crate::backend::{CacheHint, ContentBlock, LlmBackend, LlmRequest, LlmResponse, StopReason, Usage};
use crate::tool::ToolCall;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;
use std::time::Duration;

/// Maximum number of retries for rate-limit (429) and overloaded (529) errors.
const MAX_RETRIES: u32 = 3;
const DEFAULT_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Maximum cache_control breakpoints in a single Anthropic request.
const MAX_CACHE_BREAKPOINTS: usize = 4;

pub struct AnthropicBackend {
    pub config_name: String,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub client: reqwest::Client,
    pub max_content_chars: Option<usize>,
}

fn strip_thinking(content: &str) -> String {
    content.replace("\n<think>", "").replace("</think>", "")
}

/// Render a `CacheHint` into Anthropic's `cache_control` JSON object.
/// Returns `None` for `CacheHint::None` (no field should be emitted).
fn cache_control_value(hint: CacheHint) -> Option<serde_json::Value> {
    match hint {
        CacheHint::None  => None,
        CacheHint::Short => Some(serde_json::json!({"type": "ephemeral"})),
        CacheHint::Long  => Some(serde_json::json!({"type": "ephemeral", "ttl": "1h"})),
    }
}

/// Apply a cache hint to the last content block of a message JSON value.
/// Handles both string content (converted to array form) and array content.
/// No-op if hint is `None` or content shape is empty.
fn apply_cache_hint_to_message(msg: &mut serde_json::Value, hint: CacheHint) {
    let Some(cc) = cache_control_value(hint) else { return; };
    match msg.get("content").cloned() {
        Some(serde_json::Value::String(s)) => {
            msg["content"] = serde_json::json!([
                {"type": "text", "text": s, "cache_control": cc}
            ]);
        }
        Some(serde_json::Value::Array(arr)) if !arr.is_empty() => {
            let mut new_arr = arr;
            if let Some(last_block) = new_arr.last_mut() {
                last_block["cache_control"] = cc;
            }
            msg["content"] = serde_json::Value::Array(new_arr);
        }
        _ => {}
    }
}

/// Compute effective per-message cache hints after applying the breakpoint budget.
/// Anthropic supports up to 4 cache_control breakpoints per request.
/// Strategy: count static breakpoints (system + tools), give the remainder to
/// messages, retain only the last N marked messages, downgrade the rest to None.
fn enforce_breakpoint_budget(req: &LlmRequest) -> Vec<CacheHint> {
    let used_static = req.tools.iter().filter(|t| t.cache != CacheHint::None).count()
        + req.system_prompt.as_ref().map_or(0, |s| (s.cache != CacheHint::None) as usize);
    let remaining = MAX_CACHE_BREAKPOINTS.saturating_sub(used_static);

    let marked: Vec<usize> = req.extra_messages.iter()
        .enumerate()
        .filter(|(_, m)| m.cache != CacheHint::None)
        .map(|(i, _)| i)
        .collect();

    if marked.len() > remaining {
        tracing::warn!(
            "cache breakpoint budget exceeded: {} static + {} message hints, \
             dropping {} earliest message hints (max breakpoints = {})",
            used_static, marked.len(), marked.len() - remaining, MAX_CACHE_BREAKPOINTS
        );
    }

    let kept: HashSet<usize> = marked.iter().rev().take(remaining).copied().collect();

    req.extra_messages.iter().enumerate()
        .map(|(i, m)| if kept.contains(&i) { m.cache } else { CacheHint::None })
        .collect()
}

/// Pure body builder: produces the full JSON payload sent to Anthropic.
/// Exposed for tests; `complete()` calls it.
pub fn build_request_body_for_test(req: &LlmRequest, model: &str) -> serde_json::Value {
    build_request_body(req, model)
}

fn build_request_body(req: &LlmRequest, model: &str) -> serde_json::Value {
    // Build messages array from extra_messages (or single-turn fallback).
    let mut messages: Vec<serde_json::Value> = if req.extra_messages.is_empty() {
        let user_content = crate::template::build_user_content(
            &req.context,
            req.query.as_deref(),
        );
        vec![serde_json::json!({"role": "user", "content": user_content})]
    } else {
        req.extra_messages.iter().map(|m| m.content.clone()).collect()
    };

    // Apply per-message cache hints (after budget enforcement).
    if !req.extra_messages.is_empty() {
        let effective_hints = enforce_breakpoint_budget(req);
        for (msg, hint) in messages.iter_mut().zip(effective_hints.iter().copied()) {
            apply_cache_hint_to_message(msg, hint);
        }
    }

    let mut body_map = serde_json::Map::new();
    body_map.insert("model".to_string(), serde_json::Value::String(model.to_string()));
    body_map.insert("max_tokens".to_string(), serde_json::Value::Number(8192.into()));
    body_map.insert("messages".to_string(), serde_json::Value::Array(messages));

    // System prompt: optional cache_control based on hint.
    if let Some(ref system) = req.system_prompt {
        let mut block = serde_json::json!({"type": "text", "text": system.text});
        if let Some(cc) = cache_control_value(system.cache) {
            block["cache_control"] = cc;
        }
        body_map.insert("system".to_string(), serde_json::Value::Array(vec![block]));
    }

    // Tools: per-tool cache_control based on each tool's hint.
    if !req.tools.is_empty() {
        let tools_json: Vec<serde_json::Value> = req.tools.iter().map(|t| {
            let mut entry = serde_json::json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            });
            if let Some(cc) = cache_control_value(t.cache) {
                entry["cache_control"] = cc;
            }
            entry
        }).collect();
        body_map.insert("tools".to_string(), serde_json::Value::Array(tools_json));
    }

    // Thinking parameter (unchanged behavior).
    if req.enable_thinking == Some(true) {
        body_map.insert("thinking".to_string(), serde_json::json!({
            "type": "enabled",
            "budget_tokens": 4096,
        }));
    }

    serde_json::Value::Object(body_map)
}
```

Then in `impl LlmBackend for AnthropicBackend::complete()`, replace the body-building block (everything from the `let messages: ...` statement up through the `let body = serde_json::Value::Object(body_map);` line) with a single call:

```rust
let body = build_request_body(req, &self.model);
crate::message_log::log_request(&body, req.use_case);
```

Keep the rest of `complete()` (the retry loop, response parsing) unchanged.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p omnish-llm --test cache_hint_test`
Expected: PASS - all translation tests pass.

Run: `cargo test -p omnish-llm`
Expected: PASS - full crate tests still green.

- [ ] **Step 5: Build daemon to confirm no consumer broke**

Run: `cargo build --release -p omnish-daemon`
Expected: builds clean.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "feat(llm): translate CacheHint to Anthropic cache_control with budget enforcement

- Pure build_request_body() function for testability
- Per-tool, per-system, per-message hint reading
- 4-breakpoint budget retains the latest N message marks
- Removes legacy 'second-to-last message' heuristic"
```

---

### Task 6: Apply cache hints in the chat agent loop

Now the upper layer's policy goes in: system_prompt → Long, last-2 messages → Long. This is added to the chat path's request build at line ~1266 and re-applied each iteration of `run_agent_loop` (because new messages are appended each round).

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

- [ ] **Step 1: Set system_prompt cache to Long at chat entry**

In `crates/omnish-daemon/src/server.rs`, find the block from Task 4 Step 6 where `system_prompt: Some(omnish_llm::backend::CachedText { text: full_system_prompt, cache: omnish_llm::backend::CacheHint::None })` is constructed. Change the cache field to `Long`:

```rust
system_prompt: Some(omnish_llm::backend::CachedText {
    text: full_system_prompt,
    cache: omnish_llm::backend::CacheHint::Long,
}),
```

- [ ] **Step 2: Add a helper to mark the last-2 messages with Long**

Add this private helper near the top of `crates/omnish-daemon/src/server.rs` (any module-level position works; put it near `run_agent_loop`):

```rust
/// Apply cache hints for the chat agent loop's message tail.
/// Resets all hints to None, then marks the last 2 messages as Long.
/// Called before each LLM call so newly-appended messages get fresh marks
/// (without accumulating beyond the budget).
fn mark_chat_message_hints(messages: &mut [omnish_llm::backend::TaggedMessage]) {
    for m in messages.iter_mut() {
        m.cache = omnish_llm::backend::CacheHint::None;
    }
    let len = messages.len();
    for i in 0..2.min(len) {
        messages[len - 1 - i].cache = omnish_llm::backend::CacheHint::Long;
    }
}
```

- [ ] **Step 3: Call the helper before each LLM call in `run_agent_loop`**

In `run_agent_loop`, find the loop body around line ~1570:

```rust
for iteration in state.iteration..max_iterations {
    // Check if user interrupted (Ctrl+C)
    if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
        ...
    }

    match backend.complete(&state.llm_req).await {
```

Insert the helper call right before `backend.complete()`:

```rust
for iteration in state.iteration..max_iterations {
    // Check if user interrupted (Ctrl+C)
    if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
        ...
    }

    // Apply cache hints fresh each iteration: agent loop appends new messages,
    // and last-N markers must roll forward without accumulating beyond budget.
    mark_chat_message_hints(&mut state.llm_req.extra_messages);

    match backend.complete(&state.llm_req).await {
```

- [ ] **Step 4: Build & run tests**

Run: `cargo build --release`
Expected: clean build.

Run: `cargo test -p omnish-daemon`
Expected: existing tests pass (no daemon test exercises the chat agent loop end-to-end with mocked LLM, so this is a smoke check).

- [ ] **Step 5: Add an integration-style unit test for `mark_chat_message_hints`**

Append to `crates/omnish-llm/tests/cache_hint_test.rs` (the helper lives in `omnish-daemon` but its semantics are simple and testable inline; add a test in the daemon crate instead):

Create `crates/omnish-daemon/tests/chat_cache_hints_test.rs`:

```rust
//! Verifies the chat agent loop marks last-2 messages with Long.
//!
//! Mirrors the helper's logic - if you change `mark_chat_message_hints`,
//! update this test (it's intentionally redundant for safety).

use omnish_llm::backend::{CacheHint, TaggedMessage};

fn mark_chat_message_hints(messages: &mut [TaggedMessage]) {
    for m in messages.iter_mut() {
        m.cache = CacheHint::None;
    }
    let len = messages.len();
    for i in 0..2.min(len) {
        messages[len - 1 - i].cache = CacheHint::Long;
    }
}

fn msg(text: &str) -> TaggedMessage {
    TaggedMessage {
        content: serde_json::json!({"role":"user","content":text}),
        cache: CacheHint::Long, // Pre-set to verify reset
    }
}

#[test]
fn marks_last_two_messages_long() {
    let mut msgs = vec![msg("a"), msg("b"), msg("c"), msg("d")];
    mark_chat_message_hints(&mut msgs);
    assert_eq!(msgs[0].cache, CacheHint::None);
    assert_eq!(msgs[1].cache, CacheHint::None);
    assert_eq!(msgs[2].cache, CacheHint::Long);
    assert_eq!(msgs[3].cache, CacheHint::Long);
}

#[test]
fn handles_single_message() {
    let mut msgs = vec![msg("only")];
    mark_chat_message_hints(&mut msgs);
    assert_eq!(msgs[0].cache, CacheHint::Long);
}

#[test]
fn handles_empty_list() {
    let mut msgs: Vec<TaggedMessage> = vec![];
    mark_chat_message_hints(&mut msgs);
    assert!(msgs.is_empty());
}

#[test]
fn resets_old_marks_before_setting_new() {
    let mut msgs = vec![msg("a"), msg("b"), msg("c"), msg("d"), msg("e")];
    // All start as Long (from msg() helper). After marking, only last 2 should be Long.
    mark_chat_message_hints(&mut msgs);
    assert_eq!(msgs[0].cache, CacheHint::None);
    assert_eq!(msgs[1].cache, CacheHint::None);
    assert_eq!(msgs[2].cache, CacheHint::None);
    assert_eq!(msgs[3].cache, CacheHint::Long);
    assert_eq!(msgs[4].cache, CacheHint::Long);
}
```

Run: `cargo test -p omnish-daemon --test chat_cache_hints_test`
Expected: PASS - all 4 tests.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "feat(daemon): apply Long cache hints in chat agent loop

- system_prompt -> Long
- last-2 messages -> Long, re-marked each iteration
- Stays within the 4-breakpoint budget (1 system + 2 messages)"
```

---

### Task 7: Apply Long cache hint for completion warmup

The warmup at `server.rs` line ~1991 sends a stable, pre-built completion prompt. Marking it Long gives 1h cache for subsequent completion requests built from the same template prefix.

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs` (line ~1991 warmup site)

The warmup currently passes `query: Some(prompt)` with empty `extra_messages`, which lands in the single-turn fallback in `anthropic.rs::build_request_body`. To get a per-message cache hint applied, route the prompt through `extra_messages` instead.

- [ ] **Step 1: Modify the warmup call site**

Find the block at line ~1991:

```rust
let req = LlmRequest {
    context: String::new(),
    query: Some(prompt),
    trigger: TriggerType::Manual,
    session_ids: vec![session_id.to_string()],
    use_case: UseCase::Completion,
    max_content_chars: max_chars,
    system_prompt: None,
    enable_thinking: Some(false),
    tools: vec![],
    extra_messages: vec![],
};
```

Replace with:

```rust
let req = LlmRequest {
    context: String::new(),
    query: None,  // moved to extra_messages so we can attach a cache hint
    trigger: TriggerType::Manual,
    session_ids: vec![session_id.to_string()],
    use_case: UseCase::Completion,
    max_content_chars: max_chars,
    system_prompt: None,
    enable_thinking: Some(false),
    tools: vec![],
    extra_messages: vec![omnish_llm::backend::TaggedMessage {
        content: serde_json::json!({"role": "user", "content": prompt}),
        cache: omnish_llm::backend::CacheHint::Long,
    }],
};
```

- [ ] **Step 2: Build & run tests**

Run: `cargo build --release -p omnish-daemon`
Expected: clean build.

Run: `cargo test -p omnish-daemon`
Expected: existing tests pass.

- [ ] **Step 3: Commit**

```bash
git add -u
git commit -m "feat(daemon): mark completion warmup prompt with Long cache hint"
```

---

### Task 8: Final integration check

**Files:**
- Read-only: ensure release build, full workspace tests, integration tests pass.

- [ ] **Step 1: Full release build**

Run: `cargo build --release`
Expected: clean build, no warnings about unused imports (specifically `ChatTurn`).

- [ ] **Step 2: Full workspace test**

Run: `cargo test --release`
Expected: all tests pass.

- [ ] **Step 3: Run integration test smoke (basic only)**

Run: `bash tools/integration_tests/test_basic.sh`
Expected: passes. (This validates the daemon binary still runs and basic shell flows work.)

If `test_basic.sh` requires a running daemon, ask the user to run `omnish-daemon` first (per CLAUDE.md project rule: "DO NOT run omnish-daemon via bash tool, ask me to do it.").

- [ ] **Step 4: Verify wire output by hand on a chat session**

This is a manual smoke check (no auto-test) - set `RUST_LOG=omnish_llm::message_log=info` or similar to capture the request body, run a chat turn, and confirm the JSON shows:
- `system[0].cache_control = {"type":"ephemeral","ttl":"1h"}`
- last 2 messages have `cache_control` on their final block
- No `cache_control` on any tool entry

If verifying against the message_log JSONL files (`omnish_dir() / "logs"` per project docs), grep for `"cache_control"` in the latest file.

- [ ] **Step 5: Optional - close issue with commit reference**

Per CLAUDE.md: "when closing issue, push (to get correct commit id) and append commits info."

Tell the user:
> "Implementation complete. To close issue #550, push the branch and run:
> `glab issue close 550 -m 'Closed by commits <SHA1>..<SHAN>'`"

---

## Self-Review Notes

**Coverage check** against the spec:

| Spec section | Plan task |
|---|---|
| `CacheHint` enum | Task 1 |
| `CachedText` / `TaggedMessage` types | Task 2 |
| `ToolDef.cache` field | Task 3 |
| `LlmRequest` field types + drop `conversation` | Task 4 |
| Anthropic translation: system + tools + messages | Task 5 |
| Anthropic budget enforcement (`enforce_breakpoint_budget`) | Task 5 |
| OpenAI-compat: ignore hints, drop `conversation` | Task 4 (steps 3, 5–8) |
| Chat agent loop policy: system Long + last-2 messages Long | Task 6 |
| Completion warmup policy: Long | Task 7 |
| Other 5 call sites: type-only adaptation | Task 4 (step 5) |
| Tests in `cache_hint_test.rs` | Tasks 1, 2, 3, 5 (incrementally) |
| Tests in `chat_cache_hints_test.rs` | Task 6 |
| Integration tests | Task 8 |

All spec sections have an implementing task. Wire example in spec is verified by Task 5 Step 1's translation tests. Migration note (no persistence change) is preserved by Task 4 Step 9 routing `to_store` back through `.content.clone()` when calling `conv_mgr.append_messages`.
