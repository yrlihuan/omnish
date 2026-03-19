# /debug Command Design

## Overview

Add a `/debug` command to omnish chat mode for inspecting what gets sent to the LLM. Only available in debug builds (`#[cfg(debug_assertions)]`). Designed as part of an extensible client-side command system for future `/` commands.

## Trigger

User types `:` to enter chat mode, then `/debug <subcommand> [> file.txt]` and presses Enter. The interceptor produces `InterceptAction::Chat("/debug context")` as usual.

## Client-Side Command System

Client parses `InterceptAction::Chat(msg)` and checks for `/` prefix:

1. Strip `> path` redirect suffix (pure client-side, never sent to daemon)
2. Dispatch to command handler based on first word after `/`
3. Handler returns `String` result
4. If redirect present: write to file + show confirmation; otherwise: display in terminal
5. Unknown `/` commands fall through as normal LLM chat queries

```
:/debug context > /tmp/ctx.txt
 ^chat   ^cmd   ^subcmd  ^redirect (client-only)
```

Extensible dispatch:

```rust
fn handle_command(cmd: &str, ...) -> Option<String> {
    match first_word {
        "debug" => handle_debug(rest, ...),
        // future: "status", "export", etc.
        _ => None  // unknown → treat as normal LLM chat
    }
}
```

## /debug Subcommands

- `/debug context` — sends Request to daemon with `__debug:context` query prefix. Daemon returns `get_session_context()` result via existing Response message instead of calling LLM.
- `/debug template` — pure client-side. Imports template function from `omnish-llm`, returns template with `{context}` and `{query}` placeholders.

## Daemon Handling

No new protocol messages. Reuse existing `Request`/`Response`. In `handle_llm_request`, detect `__debug:` query prefix:

```rust
if req.query.starts_with("__debug:") {
    // parse subcommand, return data directly
} else {
    // normal LLM flow
}
```

## Debug-Only Compilation

- Client: `/` command parsing gated by `#[cfg(debug_assertions)]`
- Daemon: `__debug:` prefix handling gated by `#[cfg(debug_assertions)]`
- Release builds: `/debug ...` sent as normal LLM chat query

## Template Function

Add to `omnish-llm`:

```rust
pub fn prompt_template(has_query: bool) -> String {
    if has_query {
        "Here is the terminal session context:\n\n```\n{context}\n```\n\nUser question: {query}"
    } else {
        "Analyze this terminal session output and explain any errors or issues:\n\n```\n{context}\n```"
    }
}
```

Anthropic/OpenAI backends use this function instead of hardcoding the template.
