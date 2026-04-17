# Tool Display Interface Design

Issue: #292

## Overview

Define a structured tool display interface so each tool controls its own rendering. Replace the current flat `ChatToolStatus.status` string with 5 structured display elements, driven by a set of built-in formatters.

## Display Elements

Each tool use renders as:

```
● Bash(glab issue view 291)        ← status_icon + display_name + param_desc
  ⎿  title:     tool执行失败...    ← result_compact (terminal ⎿ gutter)
     state:     open
     author:    huan.li
```

| Element | Description | Mutability |
|---------|-------------|------------|
| `status_icon` | Colored circle: Running (white), Success (green), Error (pink) | Updatable |
| `display_name` | Tool alias for display (e.g. `Bash` for `bash`) | Immutable |
| `param_desc` | Formatted parameters (e.g. `glab issue view 291`) | Immutable |
| `result_compact` | Compact output for terminal `⎿` gutter display | Updatable (replaced) |
| `result_full` | Full output for browse mode (Ctrl+O) | Appendable |

## Protocol Changes

### Extended ChatToolStatus

```rust
pub struct ChatToolStatus {
    pub request_id: String,
    pub thread_id: String,
    pub tool_name: String,           // empty = LLM text (unchanged)
    pub tool_call_id: Option<String>, // unique per-invocation ID, None for LLM text
    pub status: String,              // kept for LLM text case only

    // Structured fields (None when tool_name is empty)
    pub status_icon: Option<StatusIcon>,
    pub display_name: Option<String>,
    pub param_desc: Option<String>,
    pub result_compact: Option<Vec<String>>,
    pub result_full: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StatusIcon {
    Running,   // white ●
    Success,   // green ●
    Error,     // pink ●
}
```

When `tool_name` is empty (LLM text), only `status` is used; structured fields and `tool_call_id` are `None`.

When `tool_name` is non-empty, `status` is ignored; rendering is driven by structured fields. `tool_call_id` uniquely identifies the invocation for update matching (the LLM can call the same tool multiple times in one turn).

**Semantics of `Option<Vec<String>>`**: `None` = not yet available (before execution). `Some(vec![])` = result available but empty output. Client uses `result_compact.is_none()` to distinguish first render from update.

Requires `PROTOCOL_VERSION` bump (currently 4 → 5) since struct fields change serialization.

## tool.json Changes

Two new optional fields:

```json
{
  "tools": [
    {
      "name": "bash",
      "display_name": "Bash",
      "formatter": "default",
      "status_template": "{command}",
      ...
    }
  ]
}
```

- `display_name`: Short alias for display. Defaults to `name`.
- `formatter`: Built-in formatter name. Defaults to `"default"`.
- `status_template`: Consumed by the `default` formatter for `param_desc` interpolation. Ignored by non-default formatters which hardcode their `param_desc` field extraction.

## Built-in Formatters

Daemon-side Rust implementations. Each tool specifies which formatter to use in tool.json.

### Formatter Trait

```rust
// crates/omnish-daemon/src/formatter.rs

pub struct FormatInput {
    pub tool_name: String,
    pub display_name: String,
    pub status_template: String,      // from tool.json, used by default formatter
    pub params: serde_json::Value,
    pub output: Option<String>,       // None = before execution
    pub is_error: Option<bool>,       // None = before execution
}

pub struct FormatOutput {
    pub status_icon: StatusIcon,
    pub param_desc: String,
    pub result_compact: Vec<String>,  // empty = before execution
    pub result_full: Vec<String>,     // empty = before execution
}

pub trait ToolFormatter: Send + Sync {
    fn format(&self, input: &FormatInput) -> FormatOutput;
}
```

Note: `display_name` is in `FormatInput` only (passed through to `ChatToolStatus` by the caller). `FormatOutput` does not include it - the caller copies `display_name` from input to the message directly.

### Built-in Implementations

| Formatter | param_desc | result_compact | result_full |
|-----------|-----------|----------------|-------------|
| `default` | Interpolate `status_template` with `{field}` from params, escape `\n\r` | Head 5 lines of output | All output lines verbatim |
| `read` | `file_path` from params | `"N lines"` | All output lines verbatim |
| `edit` | `file_path` from params | `"done"` / error msg | All output lines verbatim |
| `write` | `file_path` from params | `"done"` / error msg | All output lines verbatim |

All formatters: `status_icon` = `Running` before execution (output is `None`), `Success`/`Error` after (based on `is_error`).

## Daemon Data Flow

### Before tool execution (agent loop)

```
for tc in tool_calls:
    formatter = get_formatter(tc.name)   // lookup tool.json "formatter" field
    out = formatter.format(FormatInput { ..., output: None, is_error: None })
    send ChatToolStatus {
        tool_call_id: Some(tc.id),
        status_icon: Some(Running),
        display_name: Some(display_name),
        param_desc: Some(out.param_desc),
        result_compact: None,            // not yet available
        result_full: None,
    }
    if client_tool → send ChatToolCall
    if daemon_tool → execute directly
```

### After tool execution

**Client-side tool:**
```
Client sends ChatToolResult { content, is_error }
Daemon receives → calls formatter.format(FormatInput { ..., output: Some(content), is_error: Some(is_error) })
Daemon sends ChatToolStatus {
    tool_call_id: Some(tc.id),
    status_icon: Some(Success or Error),
    result_compact: Some(out.result_compact),
    result_full: Some(out.result_full),
    ...
}
Then continues LLM loop → eventually sends ChatResponse
```

**Daemon-side tool:**
```
Daemon executes tool → gets ToolResult { content, is_error }
Daemon calls formatter.format(...) → sends ChatToolStatus { updated fields }
Then continues LLM loop
```

Both paths send a second `ChatToolStatus` with updated display elements to the client.

## Structured Scroll History

Replace `scroll_history: Vec<String>` with typed entries:

```rust
pub enum ScrollEntry {
    UserInput(String),               // user's chat input
    ToolStatus(ChatToolStatus),      // structured, updatable in-place
    LlmText(String),                 // intermediate LLM text (ChatToolStatus with empty tool_name)
    Response(String),                // final LLM response (markdown source, rendered on display)
    Separator,
    SystemMessage(String),           // "(interrupted)", errors
}
```

### Benefits

- **Single rendering logic**: main flow and browse mode (Ctrl+O) share the same render functions
- **Easy updates**: result arriving → update ToolStatus entry's `status_icon`, fill `result_compact`/`result_full`
- **Resize-aware**: browse mode re-renders at current terminal width
- **Solves #291**: icon color update is a field change, no cursor hacking needed

### Matching update to entry

When a second `ChatToolStatus` arrives (with `result_compact` as `Some(...)`), find the corresponding `ToolStatus` entry in scroll_history by matching `tool_call_id`. This is unique per invocation, so it handles multiple calls to the same tool in one turn.

## Client Rendering

Client becomes a pure renderer - no formatting logic.

### On first ChatToolStatus (result_compact is None)

```
○ Bash(glab issue view 291)    // white circle = Running
```

Push `ScrollEntry::ToolStatus(...)` to scroll_history.

### On second ChatToolStatus (result_compact is Some)

1. Find matching `ToolStatus` in scroll_history by `tool_call_id`, update in-place
2. Terminal: move cursor up to the header line, clear, re-render header with new icon color
3. Render `result_compact` with `⎿` gutter below header

```
● Bash(glab issue view 291)    // green = Success (or pink = Error)
  ⎿  title:     tool执行失败...
     state:     open
```

**Multi-tool cursor handling**: When multiple tools are called in parallel, each gets a Running header line. When results arrive, the client tracks how many lines have been printed since each tool's header. For tool N, it moves cursor up by `lines_since_tool_N_header`, rewrites, then moves back down. The structured scroll_history provides the source of truth; terminal cursor movement is best-effort for live display.

### Browse mode (Ctrl+O)

Iterate `Vec<ScrollEntry>`, render each entry with shared render functions:
- `UserInput` → `> text` (cyan `>`)
- `ToolStatus` → header + `result_full` (full output, natural line wrap)
- `LlmText` → plain text
- `Response` → markdown rendered
- `Separator` → `─────`
- `SystemMessage` → dim text

### On ChatToolStatus (tool_name empty)

Unchanged - render as plain text, push `ScrollEntry::LlmText(...)`.

## Scope

This design:
- Replaces #291 (icon color change) - solved structurally via `StatusIcon` updates
- Replaces current hardcoded rendering in `chat_session.rs`
- Does not add formatter plugin support (built-in only for now)
- Future: formatter can be extracted to plugin interface when needed
