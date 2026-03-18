# Chat Mode Ghost Text Hint — Design Spec

**Issue**: #334
**Goal**: Show a dim ghost text hint on the first chat prompt to indicate the current model and guide the user.

## Behavior

When entering chat mode, the first empty prompt line shows a ghost hint after `> `:

- **New thread**: `current model is claude-sonnet-4-5. type /resume to continue last conversation.`
- **Resumed thread**: `current model is claude-sonnet-4-5. type to continue`

The hint:
- Renders in dim gray (`\x1b[2;90m`) using save/restore cursor so the cursor stays at `> ` position
- Disappears on the first keystroke (clear to end of line)
- Never reappears after being dismissed — one-shot only

## Protocol Change

Add `model_name: Option<String>` to `ChatReady` in `omnish-protocol/src/message.rs`. Bump `PROTOCOL_VERSION`.

The daemon populates this from the LLM backend configured for the `chat` use case. If no backend is available, `model_name` is `None` and the ghost hint omits the model prefix.

## Model Name Formatting

Strip date suffixes for readability: `claude-sonnet-4-5-20250929` → `claude-sonnet-4-5`. Logic: if the model name ends with `-YYYYMMDD` (8 digits), strip it.

## Files

| File | Change |
|------|--------|
| `omnish-protocol/src/message.rs` | Add `model_name: Option<String>` to `ChatReady`, bump `PROTOCOL_VERSION` |
| `omnish-daemon/src/server.rs` | Populate `model_name` from LLM backend config in ChatStart handler |
| `omnish-client/src/chat_session.rs` | Render ghost hint on first prompt, clear on first keystroke |
