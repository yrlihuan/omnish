# Tool Display Interface Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace flat `ChatToolStatus.status` string with 5 structured display elements driven by built-in formatters, making the client a pure renderer.

**Architecture:** Extend `ChatToolStatus` protocol message with structured fields (`status_icon`, `display_name`, `param_desc`, `result_compact`, `result_full`). Add a `ToolFormatter` trait with built-in implementations in the daemon. Replace `scroll_history: Vec<String>` with `Vec<ScrollEntry>` enum for typed, updatable entries. Daemon sends two `ChatToolStatus` per tool (before + after execution); client matches updates by `tool_call_id`.

**Tech Stack:** Rust, bincode serialization, serde, ANSI terminal rendering

**Spec:** `docs/plans/2026-03-16-tool-display-interface-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/omnish-protocol/src/message.rs` | Modify | Add `StatusIcon` enum, `tool_call_id` + structured fields to `ChatToolStatus`, bump `PROTOCOL_VERSION` |
| `crates/omnish-daemon/src/formatter.rs` | Create | `ToolFormatter` trait, `FormatInput`/`FormatOutput`, built-in formatters (`DefaultFormatter`, `ReadFormatter`, `EditFormatter`) |
| `crates/omnish-daemon/src/plugin.rs` | Modify | Add `display_name`/`formatter` to `ToolEntry`/`ToolJsonEntry`, add lookup methods |
| `crates/omnish-daemon/src/server.rs` | Modify | Use formatters to produce structured `ChatToolStatus`, send result update after tool execution |
| `crates/omnish-daemon/src/lib.rs` | Modify | Add `mod formatter;` |
| `crates/omnish-plugin/assets/tool.json` | Modify | Add `display_name` and `formatter` per tool |
| `crates/omnish-client/src/chat_session.rs` | Modify | `ScrollEntry` enum, structured `ChatToolStatus` rendering, update-by-`tool_call_id`, browse mode re-render |
| `crates/omnish-client/src/display.rs` | Modify | Add `render_tool_header()`, `render_tool_output()`, `render_scroll_entry()` functions |
| `crates/omnish-client/src/widgets/scroll_view.rs` | Modify | Accept `Vec<String>` from pre-rendered `ScrollEntry` list (no structural change needed) |

---

## Chunk 1: Protocol + Formatter

### Task 1: Extend ChatToolStatus in protocol

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs:9` (PROTOCOL_VERSION)
- Modify: `crates/omnish-protocol/src/message.rs:222-228` (ChatToolStatus struct)
- Modify: `crates/omnish-protocol/src/message.rs:480-656` (serialization test)

- [ ] **Step 1: Add StatusIcon enum**

In `crates/omnish-protocol/src/message.rs`, add before the `ChatToolStatus` struct (around line 222):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StatusIcon {
    Running,
    Success,
    Error,
}
```

- [ ] **Step 2: Extend ChatToolStatus struct**

Replace the existing `ChatToolStatus` struct (lines 222-228):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolStatus {
    pub request_id: String,
    pub thread_id: String,
    pub tool_name: String,
    pub tool_call_id: Option<String>,
    pub status: String,
    pub status_icon: Option<StatusIcon>,
    pub display_name: Option<String>,
    pub param_desc: Option<String>,
    pub result_compact: Option<Vec<String>>,
    pub result_full: Option<Vec<String>>,
}

```

- [ ] **Step 3: Bump PROTOCOL_VERSION**

Change line 9 from `pub const PROTOCOL_VERSION: u32 = 4;` to:

```rust
pub const PROTOCOL_VERSION: u32 = 5;
```

- [ ] **Step 4: Update serialization test**

Update the `message_variant_guard` test to use the new `ChatToolStatus` fields. Find the existing `ChatToolStatus` test case and update it to include the new fields:

```rust
Message::ChatToolStatus(ChatToolStatus {
    request_id: "r".into(),
    thread_id: "t".into(),
    tool_name: "bash".into(),
    tool_call_id: Some("tc1".into()),
    status: String::new(),
    status_icon: Some(StatusIcon::Running),
    display_name: Some("Bash".into()),
    param_desc: Some("ls -la".into()),
    result_compact: None,
    result_full: None,
}),
```

- [ ] **Step 5: Fix all compilation errors**

Search for all places that construct `ChatToolStatus` and add the new fields with default values. Use `tool_call_id: None`, `status_icon: None`, `display_name: None`, `param_desc: None`, `result_compact: None`, `result_full: None` as defaults for now - they'll be populated in later tasks.

Key locations:
- `crates/omnish-daemon/src/server.rs:578` (LLM text)
- `crates/omnish-daemon/src/server.rs:598` (tool status)
- Any test files constructing `ChatToolStatus`

Run: `cargo check -p omnish-protocol -p omnish-daemon -p omnish-client 2>&1`
Expected: compiles with no errors (warnings OK)

- [ ] **Step 6: Run tests**

Run: `cargo test -p omnish-protocol 2>&1`
Expected: all tests pass

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-protocol/src/message.rs crates/omnish-daemon/src/server.rs
git commit -m "feat(protocol): extend ChatToolStatus with structured display fields (#292)"
```

---

### Task 2: Add tool.json fields (display_name, formatter)

**Files:**
- Modify: `crates/omnish-daemon/src/plugin.rs:15-20` (ToolEntry)
- Modify: `crates/omnish-daemon/src/plugin.rs:53-63` (ToolJsonEntry)
- Modify: `crates/omnish-plugin/assets/tool.json`

- [ ] **Step 1: Extend ToolJsonEntry with new fields**

In `crates/omnish-daemon/src/plugin.rs`, add to `ToolJsonEntry` struct (around line 53):

```rust
#[serde(default)]
pub display_name: Option<String>,
#[serde(default)]
pub formatter: Option<String>,
```

- [ ] **Step 2: Extend ToolEntry with new fields**

In `crates/omnish-daemon/src/plugin.rs`, add to `ToolEntry` struct (around line 15):

```rust
pub display_name: String,
pub formatter: String,
```

- [ ] **Step 3: Populate new fields during plugin loading**

In the code that converts `ToolJsonEntry` → `ToolEntry`, populate:

```rust
display_name: entry.display_name.clone().unwrap_or_else(|| entry.name.clone()),
formatter: entry.formatter.clone().unwrap_or_else(|| "default".to_string()),
```

- [ ] **Step 4: Add accessor methods to PluginManager**

Add methods to `PluginManager`:

```rust
pub fn tool_display_name(&self, tool_name: &str) -> Option<&str> {
    self.tool_entries.get(tool_name).map(|e| e.display_name.as_str())
}

pub fn tool_formatter(&self, tool_name: &str) -> Option<&str> {
    self.tool_entries.get(tool_name).map(|e| e.formatter.as_str())
}

pub fn tool_status_template(&self, tool_name: &str) -> Option<&str> {
    self.tool_entries.get(tool_name).map(|e| e.status_template.as_str())
}
```

- [ ] **Step 5: Update tool.json with display_name and formatter**

In `crates/omnish-plugin/assets/tool.json`, add to each tool entry:

```json
{"name": "bash",    "display_name": "Bash",  "formatter": "default", ...}
{"name": "glob",    "display_name": "Glob",  "formatter": "default", ...}
{"name": "grep",    "display_name": "Grep",  "formatter": "default", ...}
{"name": "read",    "display_name": "Read",  "formatter": "read",    ...}
{"name": "edit",    "display_name": "Edit",  "formatter": "edit",    ...}
{"name": "write",   "display_name": "Write", "formatter": "write",   ...}
```

- [ ] **Step 6: Verify compilation**

Run: `cargo check -p omnish-daemon 2>&1`
Expected: compiles

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs crates/omnish-plugin/assets/tool.json
git commit -m "feat(plugin): add display_name and formatter fields to tool.json (#292)"
```

---

### Task 3: Create formatter module with built-in formatters

**Files:**
- Create: `crates/omnish-daemon/src/formatter.rs`
- Modify: `crates/omnish-daemon/src/lib.rs` (add `mod formatter;`)

- [ ] **Step 1: Write tests for DefaultFormatter**

Create `crates/omnish-daemon/src/formatter.rs` with tests first:

```rust
use omnish_protocol::message::StatusIcon;

pub struct FormatInput {
    pub tool_name: String,
    pub display_name: String,
    pub status_template: String,
    pub params: serde_json::Value,
    pub output: Option<String>,
    pub is_error: Option<bool>,
}

pub struct FormatOutput {
    pub status_icon: StatusIcon,
    pub param_desc: String,
    pub result_compact: Vec<String>,
    pub result_full: Vec<String>,
}

pub trait ToolFormatter: Send + Sync {
    fn format(&self, input: &FormatInput) -> FormatOutput;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_formatter_before_execution() {
        let f = DefaultFormatter;
        let out = f.format(&FormatInput {
            tool_name: "bash".into(),
            display_name: "Bash".into(),
            status_template: "{command}".into(),
            params: serde_json::json!({"command": "ls -la"}),
            output: None,
            is_error: None,
        });
        assert!(matches!(out.status_icon, StatusIcon::Running));
        assert_eq!(out.param_desc, "ls -la");
        assert!(out.result_compact.is_empty());
        assert!(out.result_full.is_empty());
    }

    #[test]
    fn default_formatter_after_success() {
        let f = DefaultFormatter;
        let output = "file1.rs\nfile2.rs\nfile3.rs";
        let out = f.format(&FormatInput {
            tool_name: "bash".into(),
            display_name: "Bash".into(),
            status_template: "{command}".into(),
            params: serde_json::json!({"command": "ls"}),
            output: Some(output.into()),
            is_error: Some(false),
        });
        assert!(matches!(out.status_icon, StatusIcon::Success));
        assert_eq!(out.result_compact.len(), 3);
        assert_eq!(out.result_full.len(), 3);
    }

    #[test]
    fn default_formatter_after_error() {
        let f = DefaultFormatter;
        let out = f.format(&FormatInput {
            tool_name: "bash".into(),
            display_name: "Bash".into(),
            status_template: "{command}".into(),
            params: serde_json::json!({"command": "bad_cmd"}),
            output: Some("command not found".into()),
            is_error: Some(true),
        });
        assert!(matches!(out.status_icon, StatusIcon::Error));
    }

    #[test]
    fn default_formatter_truncates_compact_to_5_lines() {
        let f = DefaultFormatter;
        let output = (0..20).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let out = f.format(&FormatInput {
            tool_name: "bash".into(),
            display_name: "Bash".into(),
            status_template: "{command}".into(),
            params: serde_json::json!({"command": "ls"}),
            output: Some(output),
            is_error: Some(false),
        });
        assert_eq!(out.result_compact.len(), 5);
        assert_eq!(out.result_full.len(), 20);
    }

    #[test]
    fn default_formatter_escapes_newlines_in_param_desc() {
        let f = DefaultFormatter;
        let out = f.format(&FormatInput {
            tool_name: "bash".into(),
            display_name: "Bash".into(),
            status_template: "{command}".into(),
            params: serde_json::json!({"command": "echo 'hello\nworld'"}),
            output: None,
            is_error: None,
        });
        assert!(!out.param_desc.contains('\n'));
    }

    #[test]
    fn read_formatter_compact_shows_line_count() {
        let f = ReadFormatter;
        let output = "line1\nline2\nline3";
        let out = f.format(&FormatInput {
            tool_name: "read".into(),
            display_name: "Read".into(),
            status_template: "{file_path}".into(),
            params: serde_json::json!({"file_path": "/tmp/foo.rs"}),
            output: Some(output.into()),
            is_error: Some(false),
        });
        assert_eq!(out.param_desc, "/tmp/foo.rs");
        assert_eq!(out.result_compact, vec!["3 lines"]);
        assert_eq!(out.result_full.len(), 3);
    }

    #[test]
    fn edit_formatter_compact_shows_done() {
        let f = EditFormatter;
        let out = f.format(&FormatInput {
            tool_name: "edit".into(),
            display_name: "Edit".into(),
            status_template: "{file_path}".into(),
            params: serde_json::json!({"file_path": "/tmp/foo.rs"}),
            output: Some("ok".into()),
            is_error: Some(false),
        });
        assert_eq!(out.param_desc, "/tmp/foo.rs");
        assert_eq!(out.result_compact, vec!["done"]);
    }

    #[test]
    fn edit_formatter_error_shows_message() {
        let f = EditFormatter;
        let out = f.format(&FormatInput {
            tool_name: "edit".into(),
            display_name: "Edit".into(),
            status_template: "{file_path}".into(),
            params: serde_json::json!({"file_path": "/tmp/foo.rs"}),
            output: Some("file not found".into()),
            is_error: Some(true),
        });
        assert_eq!(out.result_compact, vec!["file not found"]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p omnish-daemon -- formatter 2>&1`
Expected: FAIL - `DefaultFormatter`, `ReadFormatter`, `EditFormatter` not defined

- [ ] **Step 3: Implement DefaultFormatter**

Add to `crates/omnish-daemon/src/formatter.rs`:

```rust
pub struct DefaultFormatter;

impl ToolFormatter for DefaultFormatter {
    fn format(&self, input: &FormatInput) -> FormatOutput {
        let param_desc = interpolate_template(&input.status_template, &input.params);
        let status_icon = match input.is_error {
            None => StatusIcon::Running,
            Some(false) => StatusIcon::Success,
            Some(true) => StatusIcon::Error,
        };
        let (result_compact, result_full) = match &input.output {
            None => (vec![], vec![]),
            Some(output) => {
                let lines: Vec<String> = output.lines().map(String::from).collect();
                let compact = lines.iter().take(5).cloned().collect();
                (compact, lines)
            }
        };
        FormatOutput { status_icon, param_desc, result_compact, result_full }
    }
}

fn interpolate_template(template: &str, params: &serde_json::Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = params.as_object() {
        for (key, value) in obj {
            let replacement = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&format!("{{{}}}", key), &replacement);
        }
    }
    result.replace('\n', "\\n").replace('\r', "\\r")
}
```

- [ ] **Step 4: Implement ReadFormatter**

```rust
pub struct ReadFormatter;

impl ToolFormatter for ReadFormatter {
    fn format(&self, input: &FormatInput) -> FormatOutput {
        let param_desc = input.params.get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or(&input.tool_name)
            .to_string();
        let status_icon = match input.is_error {
            None => StatusIcon::Running,
            Some(false) => StatusIcon::Success,
            Some(true) => StatusIcon::Error,
        };
        let (result_compact, result_full) = match &input.output {
            None => (vec![], vec![]),
            Some(output) => {
                let lines: Vec<String> = output.lines().map(String::from).collect();
                let compact = if input.is_error == Some(true) {
                    lines.iter().take(5).cloned().collect()
                } else {
                    vec![format!("{} lines", lines.len())]
                };
                (compact, lines)
            }
        };
        FormatOutput { status_icon, param_desc, result_compact, result_full }
    }
}
```

- [ ] **Step 5: Implement EditFormatter (shared by edit and write)**

```rust
pub struct EditFormatter;

impl ToolFormatter for EditFormatter {
    fn format(&self, input: &FormatInput) -> FormatOutput {
        let param_desc = input.params.get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or(&input.tool_name)
            .to_string();
        let status_icon = match input.is_error {
            None => StatusIcon::Running,
            Some(false) => StatusIcon::Success,
            Some(true) => StatusIcon::Error,
        };
        let (result_compact, result_full) = match &input.output {
            None => (vec![], vec![]),
            Some(output) => {
                let lines: Vec<String> = output.lines().map(String::from).collect();
                let compact = if input.is_error == Some(true) {
                    lines.iter().take(5).cloned().collect()
                } else {
                    vec!["done".to_string()]
                };
                (compact, lines)
            }
        };
        FormatOutput { status_icon, param_desc, result_compact, result_full }
    }
}
```

- [ ] **Step 6: Add get_formatter lookup function**

```rust
pub fn get_formatter(name: &str) -> &'static dyn ToolFormatter {
    match name {
        "read" => &ReadFormatter,
        "edit" | "write" => &EditFormatter,
        _ => &DefaultFormatter,
    }
}
```

- [ ] **Step 7: Add mod formatter to lib.rs**

In `crates/omnish-daemon/src/lib.rs`, add:

```rust
pub mod formatter;
```

- [ ] **Step 8: Run tests**

Run: `cargo test -p omnish-daemon -- formatter 2>&1`
Expected: all 8 tests pass

- [ ] **Step 9: Commit**

```bash
git add crates/omnish-daemon/src/formatter.rs crates/omnish-daemon/src/lib.rs
git commit -m "feat(daemon): add ToolFormatter trait with default/read/edit built-in formatters (#292)"
```

---

## Chunk 2: Daemon Integration

### Task 4: Wire formatters into daemon agent loop (before execution)

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs:574-603` (ChatToolStatus creation)

- [ ] **Step 1: Import formatter module**

At the top of `server.rs`, add:

```rust
use crate::formatter::{self, FormatInput};
```

- [ ] **Step 2: Replace tool_status_text with formatter call (pre-execution)**

In the agent loop (around line 591-603), replace the `tool_status_text` call and `ChatToolStatus` construction. For each tool call:

```rust
let display_name = plugin_mgr.tool_display_name(&tc.name)
    .unwrap_or(&tc.name).to_string();
let formatter_name = plugin_mgr.tool_formatter(&tc.name)
    .unwrap_or("default");
let status_template = plugin_mgr.tool_status_template(&tc.name)
    .unwrap_or("").to_string();
let fmt = formatter::get_formatter(formatter_name);
let fmt_out = fmt.format(&FormatInput {
    tool_name: tc.name.clone(),
    display_name: display_name.clone(),
    status_template,
    params: tc.input.clone(),
    output: None,
    is_error: None,
});

messages.push(Message::ChatToolStatus(ChatToolStatus {
    request_id: state.cm.request_id.clone(),
    thread_id: state.cm.thread_id.clone(),
    tool_name: tc.name.clone(),
    tool_call_id: Some(tc.id.clone()),
    status: String::new(),
    status_icon: Some(fmt_out.status_icon),
    display_name: Some(display_name),
    param_desc: Some(fmt_out.param_desc),
    result_compact: None,
    result_full: None,
}));
```

- [ ] **Step 3: Keep LLM text ChatToolStatus unchanged**

The LLM text path (around line 574-586) should remain unchanged - it creates `ChatToolStatus` with empty `tool_name` and all new fields as `None`.

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p omnish-daemon 2>&1`
Expected: compiles

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "feat(daemon): use formatters for pre-execution ChatToolStatus (#292)"
```

---

### Task 5: Send post-execution ChatToolStatus from daemon

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs:442-521` (handle_tool_result for client-side tools)
- Modify: `crates/omnish-daemon/src/server.rs:618-631` (daemon-side tool execution)

- [ ] **Step 1: Add post-execution update for daemon-side tools**

In the daemon-side tool execution path (around line 618-631), after executing the tool and getting `result`, call the formatter and send an update `ChatToolStatus`:

```rust
// After: result.tool_use_id = tc.id.clone();
let fmt = formatter::get_formatter(
    plugin_mgr.tool_formatter(&tc.name).unwrap_or("default")
);
let display_name = plugin_mgr.tool_display_name(&tc.name)
    .unwrap_or(&tc.name).to_string();
let status_template = plugin_mgr.tool_status_template(&tc.name)
    .unwrap_or("").to_string();
let fmt_out = fmt.format(&FormatInput {
    tool_name: tc.name.clone(),
    display_name: display_name.clone(),
    status_template,
    params: tc.input.clone(),
    output: Some(result.content.clone()),
    is_error: Some(result.is_error),
});
messages.push(Message::ChatToolStatus(ChatToolStatus {
    request_id: state.cm.request_id.clone(),
    thread_id: state.cm.thread_id.clone(),
    tool_name: tc.name.clone(),
    tool_call_id: Some(tc.id.clone()),
    status: String::new(),
    status_icon: Some(fmt_out.status_icon),
    display_name: Some(display_name),
    param_desc: Some(fmt_out.param_desc),
    result_compact: Some(fmt_out.result_compact),
    result_full: Some(fmt_out.result_full),
}));
```

- [ ] **Step 2: Add post-execution update for client-side tools**

In `handle_tool_result` (around line 442-521), after receiving `ChatToolResult` from the client and before continuing the agent loop, call the formatter and send an update. The `ChatToolResult` contains `content` and `is_error`. Need to retrieve the original tool call params from `AgentLoopState.pending_tool_calls`.

Find the matching pending tool call by `tool_call_id`, then:

```rust
let tc = state.pending_tool_calls.iter()
    .find(|tc| tc.id == tool_result.tool_call_id)
    .unwrap();
let fmt = formatter::get_formatter(
    plugin_mgr.tool_formatter(&tc.name).unwrap_or("default")
);
let display_name = plugin_mgr.tool_display_name(&tc.name)
    .unwrap_or(&tc.name).to_string();
let status_template = plugin_mgr.tool_status_template(&tc.name)
    .unwrap_or("").to_string();
let fmt_out = fmt.format(&FormatInput {
    tool_name: tc.name.clone(),
    display_name: display_name.clone(),
    status_template,
    params: tc.input.clone(),
    output: Some(tool_result.content.clone()),
    is_error: Some(tool_result.is_error),
});
// Send update ChatToolStatus on the stream
// (exact mechanism depends on how handle_tool_result returns messages)
```

The update `ChatToolStatus` must be sent on the response stream before continuing the LLM loop.

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p omnish-daemon 2>&1`
Expected: compiles

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "feat(daemon): send post-execution ChatToolStatus with formatted results (#292)"
```

---

## Chunk 3: Client Rendering

### Task 6: Add ScrollEntry enum and rendering functions

**Files:**
- Modify: `crates/omnish-client/src/chat_session.rs:20` (scroll_history type)
- Modify: `crates/omnish-client/src/display.rs` (add render functions)

- [ ] **Step 1: Define ScrollEntry enum**

In `crates/omnish-client/src/chat_session.rs`, add near the top (after imports):

```rust
use omnish_protocol::message::{ChatToolStatus, StatusIcon};

#[derive(Debug, Clone)]
pub enum ScrollEntry {
    UserInput(String),
    ToolStatus(ChatToolStatus),
    LlmText(String),
    Response(String),
    Separator,
    SystemMessage(String),
}
```

Change `scroll_history: Vec<String>` to `scroll_history: Vec<ScrollEntry>` (line 20).

- [ ] **Step 2: Add render functions to display.rs**

In `crates/omnish-client/src/display.rs`, add:

```rust
use omnish_protocol::message::StatusIcon;

/// Render the ● header line for a tool status
pub fn render_tool_header(
    icon: &StatusIcon,
    display_name: &str,
    param_desc: &str,
    max_cols: usize,
) -> String {
    let icon_str = match icon {
        StatusIcon::Running => "\x1b[97m●\x1b[0m",   // white
        StatusIcon::Success => "\x1b[38;5;114m●\x1b[0m", // green
        StatusIcon::Error => "\x1b[38;5;211m●\x1b[0m",   // pink
    };
    let name_cols = display_name.len() + 2; // name + "()"
    let available = max_cols.saturating_sub(4 + name_cols); // "● " + name + "()"
    let truncated = truncate_cols(param_desc, available);
    format!(
        "{} \x1b[1m{}\x1b[0m\x1b[2m({})\x1b[0m",
        icon_str, display_name, truncated
    )
}

/// Render the full (non-truncated) header for scroll_history / browse mode
pub fn render_tool_header_full(
    icon: &StatusIcon,
    display_name: &str,
    param_desc: &str,
) -> String {
    let icon_str = match icon {
        StatusIcon::Running => "\x1b[97m●\x1b[0m",
        StatusIcon::Success => "\x1b[38;5;114m●\x1b[0m",
        StatusIcon::Error => "\x1b[38;5;211m●\x1b[0m",
    };
    format!(
        "{} \x1b[1m{}\x1b[0m\x1b[2m({})\x1b[0m",
        icon_str, display_name, param_desc
    )
}

/// Render tool output lines with ⎿ gutter (compact view)
pub fn render_tool_output(lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            out.push(format!("  \x1b[2m⎿  {}\x1b[0m", line));
        } else {
            out.push(format!("  \x1b[2m   {}\x1b[0m", line));
        }
    }
    out
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p omnish-client 2>&1`
Expected: many errors due to `scroll_history` type change - that's expected, will be fixed in next task.

- [ ] **Step 4: Commit display.rs changes only**

```bash
git add crates/omnish-client/src/display.rs
git commit -m "feat(client): add render_tool_header and render_tool_output display functions (#292)"
```

---

### Task 7: Rewrite ChatToolStatus handling in chat_session.rs

**Files:**
- Modify: `crates/omnish-client/src/chat_session.rs:317-341` (Phase 1 rendering)
- Modify: `crates/omnish-client/src/chat_session.rs:413-446` (Phase 3 rendering)
- Modify: `crates/omnish-client/src/chat_session.rs:65-69` (print_line)

- [ ] **Step 1: Update print_line to accept ScrollEntry**

Replace `print_line` (lines 65-69) - it currently pushes a String. Split into two methods:

```rust
fn print_line(&mut self, line: &str) {
    write_stdout(line);
    write_stdout("\r\n");
}

fn push_entry(&mut self, entry: ScrollEntry) {
    self.scroll_history.push(entry);
}
```

This separates terminal output from history tracking.

- [ ] **Step 2: Rewrite Phase 1 ChatToolStatus rendering**

Replace the ChatToolStatus handler (lines 317-341). The new logic:

```rust
Some(Message::ChatToolStatus(cts)) => {
    self.erase_thinking();
    if cts.tool_name.is_empty() {
        // LLM intermediate text
        self.print_line(&cts.status);
        self.push_entry(ScrollEntry::LlmText(cts.status.clone()));
    } else if cts.result_compact.is_none() {
        // First status - tool is running
        let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
        let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
        let param_desc = cts.param_desc.as_deref().unwrap_or("");
        let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Running);
        let header = display::render_tool_header(icon, display_name, param_desc, cols as usize);
        self.print_line(&header);
        self.push_entry(ScrollEntry::ToolStatus(cts));
    } else {
        // Second status - tool completed, update existing entry
        let tool_call_id = cts.tool_call_id.as_deref();
        // Find and update the matching ToolStatus entry
        if let Some(entry) = self.scroll_history.iter_mut().rev().find(|e| {
            matches!(e, ScrollEntry::ToolStatus(prev)
                if prev.tool_call_id.as_deref() == tool_call_id)
        }) {
            *entry = ScrollEntry::ToolStatus(cts.clone());
        }
        // Re-render: move up 1 line, clear, rewrite header + output
        write_stdout("\x1b[1A\r\x1b[K");
        let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
        let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
        let param_desc = cts.param_desc.as_deref().unwrap_or("");
        let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Success);
        let header = display::render_tool_header(icon, display_name, param_desc, cols as usize);
        self.print_line(&header);
        // Render result_compact with ⎿ gutter
        if let Some(ref lines) = cts.result_compact {
            let rendered = display::render_tool_output(lines);
            for line in &rendered {
                self.print_line(line);
            }
        }
    }
}
```

- [ ] **Step 3: Remove Phase 3 output rendering**

In Phase 3 (lines 413-446), remove the hardcoded `⎿` gutter output rendering. The output is now rendered when the second `ChatToolStatus` arrives (step 2 above). Keep only the `ChatToolResult` sending logic.

```rust
// Phase 3: Send results only (display handled by second ChatToolStatus)
for (i, (tc, result)) in tool_calls.iter().zip(results).enumerate() {
    let (content, is_error) = result
        .unwrap_or_else(|_| ("Tool execution panicked".to_string(), true));

    let result_msg = Message::ChatToolResult(ChatToolResult {
        request_id: tc.request_id.clone(),
        thread_id: tc.thread_id.clone(),
        tool_call_id: tc.tool_call_id.clone(),
        content,
        is_error,
    });
    // ... send logic unchanged ...
}
```

- [ ] **Step 4: Update all other scroll_history push sites**

Search for remaining `self.scroll_history.push(...)` calls and convert them:

- User input (around line 120-128): `self.push_entry(ScrollEntry::UserInput(...))`
- ChatResponse (around line 348-355): `self.push_entry(ScrollEntry::Response(resp.content.clone()))` then `self.push_entry(ScrollEntry::Separator)`
- Interrupted (around line 498): `self.push_entry(ScrollEntry::SystemMessage(...))`

- [ ] **Step 5: Verify compilation**

Run: `cargo check -p omnish-client 2>&1`
Expected: errors from browse_history - fixed in next task

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-client/src/chat_session.rs
git commit -m "feat(client): structured ChatToolStatus rendering with ScrollEntry (#292)"
```

---

### Task 8: Update browse_history to render from ScrollEntry

**Files:**
- Modify: `crates/omnish-client/src/chat_session.rs:71-83` (browse_history)

- [ ] **Step 1: Rewrite browse_history to render ScrollEntry**

Replace the `browse_history` function (lines 71-83). Currently it passes `&self.scroll_history` (Vec<String>) to ScrollView. Now it must render each `ScrollEntry` to strings first:

```rust
fn browse_history(&self) {
    if self.scroll_history.is_empty() {
        return;
    }
    let (rows, cols) = super::get_terminal_size().unwrap_or((24, 80));
    let lines: Vec<String> = self.scroll_history.iter().flat_map(|entry| {
        match entry {
            ScrollEntry::UserInput(text) => {
                text.lines().enumerate().map(|(i, line)| {
                    if i == 0 {
                        format!("\x1b[36m> \x1b[0m{}", line)
                    } else {
                        format!("  {}", line)
                    }
                }).collect::<Vec<_>>()
            }
            ScrollEntry::ToolStatus(cts) => {
                let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                let param_desc = cts.param_desc.as_deref().unwrap_or("");
                let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Success);
                let mut lines = vec![display::render_tool_header_full(icon, display_name, param_desc)];
                // Use result_full in browse mode
                if let Some(ref full) = cts.result_full {
                    lines.extend(display::render_tool_output(full));
                }
                lines
            }
            ScrollEntry::LlmText(text) => vec![text.clone()],
            ScrollEntry::Response(content) => {
                let rendered = super::markdown::render(content);
                let rendered = format!("\x1b[97m●\x1b[0m {}", rendered);
                rendered.split("\r\n").map(String::from).collect()
            }
            ScrollEntry::Separator => {
                vec![display::render_separator(cols)]
            }
            ScrollEntry::SystemMessage(msg) => {
                vec![format!("\x1b[2;37m{}\x1b[0m", msg)]
            }
        }
    }).collect();

    if lines.is_empty() {
        return;
    }

    let mut view = super::widgets::scroll_view::ScrollView::new(
        lines, rows as usize, cols as usize,
    );
    view.run_browse();
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p omnish-client 2>&1`
Expected: compiles (warnings OK)

- [ ] **Step 3: Run full test suite**

Run: `cargo test 2>&1`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/chat_session.rs
git commit -m "feat(client): browse mode renders from structured ScrollEntry (#292)"
```

---

### Task 9: Final integration test and cleanup

**Files:**
- Modify: any remaining compilation issues

- [ ] **Step 1: Full build**

Run: `cargo build 2>&1`
Expected: compiles

- [ ] **Step 2: Run all tests**

Run: `cargo test 2>&1`
Expected: all pass

- [ ] **Step 3: Clean up old tool_status_text if unused**

Check if `plugin.rs:tool_status_text()` is still called anywhere. If not, remove it.

Run: `rg "tool_status_text" --type rust`

If only the definition remains, remove the function.

- [ ] **Step 4: Commit cleanup**

```bash
git add -A
git commit -m "chore: remove unused tool_status_text after formatter migration (#292)"
```

- [ ] **Step 5: Push and close issue**

```bash
git push
glab issue note 292 -m "Implemented structured tool display interface with built-in formatters. Commits: ..."
glab issue close 292
```
