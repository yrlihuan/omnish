# PromptManager Design

## Goal

Manage system prompt as composable named fragments instead of a monolithic const string. Add a ToolStatus prompt instructing the LLM to explain its actions before tool calls.

## Architecture

`PromptManager` in `crates/omnish-llm/src/prompt.rs` — ordered named fragments, joined with `\n\n`.

```rust
pub struct PromptManager {
    fragments: Vec<(String, String)>,
}
```

Methods: `new()`, `add(name, content)`, `build() -> String`.

Factory method `default_chat()` provides base fragments.

## Fragments

| Name | Content |
|------|---------|
| identity | Who the assistant is, what omnish does |
| chat_mode | Chat mode description, persistent conversations |
| commands | Available user commands (/help, /resume, etc.) |
| tool_status | "Before using any tool, explain what action you are about to take." |
| guidelines | Response style guidelines |
| tools | (dynamic, added at request time from plugin system_prompt) |

## ToolStatus Fallback

Plugin `status_text()` retained as fallback. Client shows LLM text block if present, otherwise falls back to `status_text()`.

## Files Changed

| File | Change |
|------|--------|
| `crates/omnish-llm/src/prompt.rs` | New: PromptManager |
| `crates/omnish-llm/src/lib.rs` | Export prompt module |
| `crates/omnish-llm/src/template.rs` | Split CHAT_SYSTEM_PROMPT into const fragments |
| `crates/omnish-daemon/src/server.rs` | Use PromptManager to assemble system prompt |
