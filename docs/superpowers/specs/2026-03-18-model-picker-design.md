# Per-Thread Model Selection via `/model` Picker (#154)

## Goal

Allow users to switch LLM backends per conversation thread using a `/model` picker command in chat mode.

## Context

The daemon already supports multiple LLM backends (e.g., `claude`, `deepseek`, `claude-haiku`) configured in `daemon.toml`. Currently the chat use case is fixed to one backend. This feature lets users choose which backend to use per thread, persisted across sessions.

## Design

### `/model` Command Flow

1. User types `/model` in chat mode.
2. Client sends `__cmd:models` (with `thread_id` if available) to daemon via existing builtin command pattern.
3. Daemon returns JSON array of backends:
   ```json
   [
     {"name": "claude", "model": "claude-sonnet-4-5-20250929", "selected": true},
     {"name": "deepseek", "model": "deepseek-chat", "selected": false},
     {"name": "claude-haiku", "model": "claude-haiku-3-5-20241022", "selected": false}
   ]
   ```
   - `selected` is determined by: `ThreadMeta.model` if thread exists and has one set, otherwise the default chat backend.
4. Client renders picker using existing `widgets::picker::pick_one`, pre-selecting the flagged item.
   - Display format: `claude (claude-sonnet-4-5)` — date suffix stripped for readability.
5. On selection:
   - **Existing thread**: Client sends a `ChatMessage` with `model=Some("backend_name")` and `query=""`. Daemon updates `ThreadMeta.model`, returns `Ack`. Client renders confirmation locally: "Switched to claude (claude-sonnet-4-5)".
   - **New thread (no thread_id yet)**: Client stores the selection. When the user sends the first message, it's attached as `model` on that `ChatMessage`. Thread creation and model setting happen together.

### Protocol Changes

**ChatMessage** — add optional field:
```rust
pub struct ChatMessage {
    pub request_id: String,
    pub session_id: String,
    pub thread_id: String,
    pub query: String,
    pub model: Option<String>,  // NEW: backend name override
}
```

Bump `PROTOCOL_VERSION`.

### Daemon Handling of ChatMessage.model

When `model` is present in a ChatMessage:

1. Validate the backend name exists in config. If not, ignore (use default).
2. Update `ThreadMeta.model` for this thread.
3. If `query` is empty — return `Ack` (model-only update, no LLM call, nothing written to thread history).
4. If `query` is non-empty — use the newly set backend to process the message normally.

When `model` is absent:

- Use `ThreadMeta.model` if set and the backend still exists in config.
- Otherwise fall back to the default chat backend.

### Thread Metadata

**ThreadMeta** — add optional field:
```rust
pub struct ThreadMeta {
    pub host: Option<String>,
    pub cwd: Option<String>,
    pub summary: Option<String>,
    pub summary_rounds: Option<u32>,
    pub model: Option<String>,  // NEW: backend name
}
```

Persisted in `.meta.json` alongside the thread.

### `__cmd:models` Builtin Command

- Input: `__cmd:models` or `__cmd:models <thread_id>`
- Output: JSON array as shown above.
- Source: iterates `MultiBackend`'s backend configs. Marks `selected` based on thread's stored model or default chat backend.
- Requires `MultiBackend` to expose a method listing available backends with their model names.

### Scope

- Model override applies only to **chat** use case. Completion and analysis are unaffected.
- Model override is **per-thread**. Other threads retain their own settings.
- The model-only `ChatMessage` (query="") is **not** written to thread conversation history.

### Error Handling

- Backend name in `ThreadMeta.model` no longer in config: silently fall back to default. No error shown.
- `/model` with no backends configured: show error "No LLM backends configured".
- User cancels picker (ESC): no change, return to chat prompt.

## Files to Modify

| File | Change |
|------|--------|
| `omnish-protocol/src/message.rs` | Add `model: Option<String>` to `ChatMessage`, bump `PROTOCOL_VERSION` |
| `omnish-daemon/src/conversation_mgr.rs` | Add `model: Option<String>` to `ThreadMeta` |
| `omnish-daemon/src/server.rs` | Handle `__cmd:models`; read `ChatMessage.model`, update ThreadMeta, select backend |
| `omnish-llm/src/factory.rs` | Add method to list backends (name + model) on `MultiBackend` |
| `omnish-client/src/command.rs` | Add `/model` command entry (chat-only, daemon command) |
| `omnish-client/src/chat_session.rs` | Handle `/model`: request models, render picker, send selection; store pending model for new threads |
