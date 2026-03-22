# Custom Plugin Formatter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow plugins to define custom formatters via long-running subprocess binaries, and move built-in formatters (edit, read) to omnish-plugin alongside their tools.

**Architecture:** Formatters are separated from tools by design. The `ToolFormatter` trait and built-in implementations move to `omnish-plugin`. A new `FormatterManager` in `omnish-daemon` manages both built-in and external (subprocess) formatters, using an mpsc queue per process for sequential request handling. External formatter processes are long-running, communicate via newline-delimited JSON on stdin/stdout, and are started lazily on first use.

**Tech Stack:** Rust, tokio (mpsc + oneshot channels), serde_json, omnish-plugin, omnish-daemon

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/omnish-plugin/src/formatter.rs` | **Create** | `ToolFormatter` trait, `FormatInput`, `FormatOutput`, helper functions, `DefaultFormatter`, `ReadFormatter`, `EditFormatter` |
| `crates/omnish-plugin/src/lib.rs` | **Modify** | Add `pub mod formatter;` |
| `crates/omnish-daemon/src/formatter.rs` | **Delete** | Replaced by `omnish-plugin/src/formatter.rs` + `formatter_mgr.rs` |
| `crates/omnish-daemon/src/formatter_mgr.rs` | **Create** | `FormatterManager`: registry of built-in + external formatters, subprocess lifecycle, mpsc queue |
| `crates/omnish-daemon/src/lib.rs` | **Modify** | Add `pub mod formatter_mgr;`, remove `pub mod formatter;` |
| `crates/omnish-daemon/src/server.rs` | **Modify** | Replace `formatter::get_formatter()` calls with `formatter_mgr.format()` (4 call sites) |
| `crates/omnish-daemon/src/main.rs` | **Modify** | Create `FormatterManager`, pass to `DaemonServer` |
| `crates/omnish-daemon/src/plugin.rs` | **Modify** | Parse `formatter_binary` from tool.json, pass to `FormatterManager` |
| `crates/omnish-protocol/src/message.rs` | **No change** | `StatusIcon` stays here; `FormatOutput` no longer references it |

---

### Task 1: Move ToolFormatter trait and built-in formatters to omnish-plugin

**Files:**
- Create: `crates/omnish-plugin/src/formatter.rs`
- Modify: `crates/omnish-plugin/src/lib.rs`
- Modify: `crates/omnish-plugin/Cargo.toml` (no new deps needed)

- [ ] **Step 1: Create `crates/omnish-plugin/src/formatter.rs`**

Move from `crates/omnish-daemon/src/formatter.rs`:
- `FormatInput` struct (remove `display_name` and `status_template` — those are handled by ToolRegistry, not formatter)
- `FormatOutput` struct (remove `status_icon` and `param_desc` — `status_icon` is derived from output/is_error by caller, `param_desc` from ToolRegistry)
- `ToolFormatter` trait
- Helper functions: `head_lines`, `all_lines`, `truncate_lines`
- `DefaultFormatter`, `ReadFormatter`, `EditFormatter` (and their helpers: `edit_summary`, `parse_replace_count`, `format_numbered_diff`)

Simplified types:

```rust
pub struct FormatInput {
    pub tool_name: String,
    pub params: serde_json::Value,
    pub output: String,
    pub is_error: bool,
}

pub struct FormatOutput {
    pub result_compact: Vec<String>,
    pub result_full: Vec<String>,
}

pub trait ToolFormatter: Send + Sync {
    fn format(&self, input: &FormatInput) -> FormatOutput;
}
```

Note: `FormatInput.output` is now a non-optional `String` (formatter is only called when output exists). `is_error` is a plain `bool`. The pre-execution status (Running icon, no output) is handled by the caller without involving the formatter.

Include all existing formatter implementations and their tests.

- [ ] **Step 2: Add `pub mod formatter;` to `crates/omnish-plugin/src/lib.rs`**

```rust
pub mod formatter;
```

- [ ] **Step 3: Run tests to verify formatters work in new location**

Run: `cargo test -p omnish-plugin --release --lib -- formatter`
Expected: All formatter tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-plugin/src/formatter.rs crates/omnish-plugin/src/lib.rs
git commit -m "refactor: move ToolFormatter trait and built-in formatters to omnish-plugin"
```

---

### Task 2: Create FormatterManager in omnish-daemon

**Files:**
- Create: `crates/omnish-daemon/src/formatter_mgr.rs`
- Modify: `crates/omnish-daemon/src/lib.rs`

- [ ] **Step 1: Write tests for FormatterManager built-in registration and lookup**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_builtin_formatter_default() {
        let mgr = FormatterManager::new();
        let input = FormatInput {
            tool_name: "unknown_tool".into(),
            params: serde_json::json!({}),
            output: "hello\nworld".into(),
            is_error: false,
        };
        let out = mgr.format("default", &input).await;
        assert!(!out.result_compact.is_empty());
    }

    #[tokio::test]
    async fn test_builtin_formatter_edit() {
        let mgr = FormatterManager::new();
        let input = FormatInput {
            tool_name: "edit".into(),
            params: serde_json::json!({"file_path": "/tmp/test.txt", "old_string": "hello", "new_string": "goodbye"}),
            output: "Edited /tmp/test.txt\n---\n1:  before\n2:-hello\n2:+goodbye\n3:  after".into(),
            is_error: false,
        };
        let out = mgr.format("edit", &input).await;
        assert!(out.result_compact[0].contains("Edited 1 line"));
    }

    #[tokio::test]
    async fn test_unknown_formatter_falls_back_to_default() {
        let mgr = FormatterManager::new();
        let input = FormatInput {
            tool_name: "test".into(),
            params: serde_json::json!({}),
            output: "some output".into(),
            is_error: false,
        };
        let out = mgr.format("nonexistent", &input).await;
        assert!(!out.result_compact.is_empty());
    }
}
```

- [ ] **Step 2: Implement FormatterManager with built-in formatters only**

```rust
use omnish_plugin::formatter::{
    FormatInput, FormatOutput, ToolFormatter,
    DefaultFormatter, ReadFormatter, EditFormatter,
};
use std::collections::HashMap;

pub struct FormatterManager {
    builtins: HashMap<String, Box<dyn ToolFormatter>>,
}

impl FormatterManager {
    pub fn new() -> Self {
        let mut builtins: HashMap<String, Box<dyn ToolFormatter>> = HashMap::new();
        builtins.insert("default".into(), Box::new(DefaultFormatter));
        builtins.insert("read".into(), Box::new(ReadFormatter));
        builtins.insert("edit".into(), Box::new(EditFormatter));
        builtins.insert("write".into(), Box::new(EditFormatter));
        Self { builtins }
    }

    pub async fn format(&self, formatter_name: &str, input: &FormatInput) -> FormatOutput {
        let fmt = self.builtins.get(formatter_name)
            .or_else(|| self.builtins.get("default"))
            .unwrap();
        fmt.format(input)
    }
}
```

- [ ] **Step 3: Add `pub mod formatter_mgr;` to `crates/omnish-daemon/src/lib.rs`**

- [ ] **Step 4: Run tests**

Run: `cargo test -p omnish-daemon --release --lib -- formatter_mgr`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/formatter_mgr.rs crates/omnish-daemon/src/lib.rs
git commit -m "feat: add FormatterManager with built-in formatter registry"
```

---

### Task 3: Wire FormatterManager into server, remove old formatter.rs

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs` (4 call sites)
- Modify: `crates/omnish-daemon/src/main.rs`
- Delete: `crates/omnish-daemon/src/formatter.rs`
- Modify: `crates/omnish-daemon/src/lib.rs`

- [ ] **Step 1: Add `FormatterManager` to `DaemonServer`**

Add `formatter_mgr: Arc<FormatterManager>` field to `DaemonServer` struct. Update the constructor and `main.rs` to create and pass it.

- [ ] **Step 2: Update server.rs call sites**

There are 4 call sites that use `formatter::get_formatter()`. Each follows this pattern:

Before:
```rust
let fmt = formatter::get_formatter(tool_registry.formatter_name(&tc.name));
let fmt_out = fmt.format(&FormatInput {
    tool_name: tc.name.clone(),
    display_name: display_name.clone(),
    status_template,
    params: tc.input.clone(),
    output: Some(result.content.clone()),
    is_error: Some(result.is_error),
});
// Then use fmt_out.status_icon, fmt_out.param_desc, fmt_out.result_compact, fmt_out.result_full
```

After:
```rust
let formatter_name = tool_registry.formatter_name(&tc.name);
let fmt_out = formatter_mgr.format(formatter_name, &FormatInput {
    tool_name: tc.name.clone(),
    params: tc.input.clone(),
    output: result.content.clone(),
    is_error: result.is_error,
}).await;
// status_icon: compute directly from is_error
// param_desc: already computed via tool_registry.status_text()
```

The `status_icon` is now computed at the call site:
```rust
let status_icon = if result.is_error { StatusIcon::Error } else { StatusIcon::Success };
```

The `param_desc` is already computed separately via `tool_registry.status_text()` at some call sites; at others, use `interpolate_template` (which already exists in tool_registry.rs).

For the pre-execution case (output=None), no formatter call is needed — just send Running icon with param_desc.

- [ ] **Step 3: Remove old `formatter.rs` and its `pub mod` declaration**

Delete `crates/omnish-daemon/src/formatter.rs`. Remove `pub mod formatter;` from `crates/omnish-daemon/src/lib.rs`. Update `use` statements in `server.rs` to import from `omnish_plugin::formatter` (for `FormatInput`) and `omnish_daemon::formatter_mgr` (for `FormatterManager`).

- [ ] **Step 4: Build and test**

Run: `cargo build --release`
Run: `cargo test -p omnish-daemon --release`
Expected: Clean build, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: replace formatter.rs with FormatterManager, use omnish-plugin formatters"
```

---

### Task 4: Add external formatter subprocess support

**Files:**
- Modify: `crates/omnish-daemon/src/formatter_mgr.rs`

- [ ] **Step 1: Write tests for external formatter**

```rust
#[tokio::test]
async fn test_external_formatter_echo() {
    // Create a simple formatter script that echoes back a fixed response
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("test_fmt");
    std::fs::write(&script, r#"#!/bin/bash
while IFS= read -r line; do
    echo '{"summary":"test summary","compact":["compact line"],"full":["full line 1","full line 2"]}'
done
"#).unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let mut mgr = FormatterManager::new();
    mgr.register_external("test_fmt", script.to_str().unwrap()).await;

    let input = FormatInput {
        tool_name: "test_tool".into(),
        params: serde_json::json!({}),
        output: "raw output".into(),
        is_error: false,
    };
    let out = mgr.format("test_fmt", &input).await;
    assert_eq!(out.result_compact, vec!["test summary", "compact line"]);
    assert_eq!(out.result_full, vec!["test summary", "full line 1", "full line 2"]);
}

#[tokio::test]
async fn test_external_formatter_sequential() {
    // Verify requests are processed sequentially (second request sees incremented state)
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("counter_fmt");
    std::fs::write(&script, r#"#!/bin/bash
n=0
while IFS= read -r line; do
    n=$((n + 1))
    echo "{\"summary\":\"call $n\",\"compact\":[\"call $n\"],\"full\":[\"call $n\"]}"
done
"#).unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let mut mgr = FormatterManager::new();
    mgr.register_external("counter", script.to_str().unwrap()).await;

    let input = FormatInput {
        tool_name: "t".into(),
        params: serde_json::json!({}),
        output: "x".into(),
        is_error: false,
    };
    let out1 = mgr.format("counter", &input).await;
    let out2 = mgr.format("counter", &input).await;
    assert_eq!(out1.result_compact, vec!["call 1", "call 1"]);
    assert_eq!(out2.result_compact, vec!["call 2", "call 2"]);
}
```

- [ ] **Step 2: Implement ExternalFormatter**

Protocol: one JSON line per request on stdin, one JSON line per response on stdout.

Request JSON (sent to stdin):
```json
{"formatter":"name","tool_name":"web_search","params":{"query":"rust"},"output":"...","is_error":false}
```

Response JSON (read from stdout):
```json
{"summary":"Found 5 results","compact":["line1","line2"],"full":["line1","line2","line3"]}
```

The `summary` field is prepended to both `compact` and `full` arrays by the manager (same pattern as built-in formatters).

```rust
use tokio::sync::{mpsc, oneshot};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use std::process::Stdio;

struct ExternalFormatter {
    tx: mpsc::Sender<(serde_json::Value, oneshot::Sender<ExternalResponse>)>,
}

#[derive(serde::Deserialize)]
struct ExternalResponse {
    summary: Option<String>,
    compact: Vec<String>,
    full: Vec<String>,
}

impl ExternalFormatter {
    async fn start(binary: &str) -> Self {
        let (tx, mut rx) = mpsc::channel::<(serde_json::Value, oneshot::Sender<ExternalResponse>)>(32);

        let mut child = tokio::process::Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start formatter process");

        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut reader = BufReader::new(stdout).lines();

        tokio::spawn(async move {
            while let Some((req, reply)) = rx.recv().await {
                // Write request as single JSON line
                let mut line = serde_json::to_string(&req).unwrap();
                line.push('\n');
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }

                // Read response line
                match reader.next_line().await {
                    Ok(Some(resp_line)) => {
                        match serde_json::from_str::<ExternalResponse>(&resp_line) {
                            Ok(resp) => { let _ = reply.send(resp); }
                            Err(e) => {
                                tracing::warn!("formatter response parse error: {}", e);
                                let _ = reply.send(ExternalResponse {
                                    summary: Some(format!("Formatter error: {}", e)),
                                    compact: vec![],
                                    full: vec![],
                                });
                            }
                        }
                    }
                    _ => break,
                }
            }
            // Channel closed or process died — kill child
            let _ = child.kill().await;
        });

        Self { tx }
    }

    async fn format(&self, formatter_name: &str, input: &FormatInput) -> FormatOutput {
        let req = serde_json::json!({
            "formatter": formatter_name,
            "tool_name": input.tool_name,
            "params": input.params,
            "output": input.output,
            "is_error": input.is_error,
        });
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.tx.send((req, reply_tx)).await.is_err() {
            return FormatOutput { result_compact: vec!["Formatter unavailable".into()], result_full: vec!["Formatter unavailable".into()] };
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await {
            Ok(Ok(resp)) => {
                let mut compact = Vec::new();
                let mut full = Vec::new();
                if let Some(ref s) = resp.summary {
                    compact.push(s.clone());
                    full.push(s.clone());
                }
                compact.extend(resp.compact);
                full.extend(resp.full);
                FormatOutput { result_compact: compact, result_full: full }
            }
            _ => FormatOutput { result_compact: vec!["Formatter timeout".into()], result_full: vec!["Formatter timeout".into()] },
        }
    }
}
```

- [ ] **Step 3: Extend FormatterManager to hold external formatters**

```rust
pub struct FormatterManager {
    builtins: HashMap<String, Box<dyn ToolFormatter>>,
    externals: HashMap<String, ExternalFormatter>,  // formatter_name -> process
}

impl FormatterManager {
    pub async fn register_external(&mut self, name: &str, binary: &str) {
        let ext = ExternalFormatter::start(binary).await;
        self.externals.insert(name.to_string(), ext);
    }

    pub async fn format(&self, formatter_name: &str, input: &FormatInput) -> FormatOutput {
        // Check external first
        if let Some(ext) = self.externals.get(formatter_name) {
            return ext.format(formatter_name, input).await;
        }
        // Fall back to built-in
        let fmt = self.builtins.get(formatter_name)
            .or_else(|| self.builtins.get("default"))
            .unwrap();
        fmt.format(input)
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p omnish-daemon --release --lib -- formatter_mgr`
Expected: All tests pass (built-in and external).

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/formatter_mgr.rs
git commit -m "feat: add external formatter subprocess support with mpsc queue"
```

---

### Task 5: Parse formatter_binary from tool.json and register external formatters

**Files:**
- Modify: `crates/omnish-daemon/src/plugin.rs`
- Modify: `crates/omnish-daemon/src/main.rs`
- Modify: `crates/omnish-daemon/src/server.rs` (if needed)

- [ ] **Step 1: Extend tool.json parsing to support `formatter_binary`**

In `plugin.rs`, the `ToolJsonRoot` (or equivalent top-level struct for tool.json) should accept an optional `formatter_binary` field:

```json
{
  "plugin_type": "daemon_tool",
  "formatter_binary": "./my_formatter",
  "tools": [
    {
      "name": "web_search",
      "formatter": "search_result",
      ...
    }
  ]
}
```

When `formatter_binary` is present AND a tool has a non-default `formatter` name, register that (formatter_name, binary_path) with the `FormatterManager`.

- [ ] **Step 2: Wire registration into daemon startup**

In `main.rs`, after `PluginManager::load()`, iterate plugins that have `formatter_binary` set. For each tool with a custom formatter name, call `formatter_mgr.register_external(formatter_name, binary_path)`.

The `FormatterManager` is created before plugin loading and passed (as `Arc<FormatterManager>` or `Arc<tokio::sync::Mutex<FormatterManager>>` for registration) to `DaemonServer`.

- [ ] **Step 3: Build and test end-to-end**

Run: `cargo build --release`
Run: `cargo test --release`
Expected: Clean build, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs crates/omnish-daemon/src/main.rs
git commit -m "feat: parse formatter_binary from tool.json and register external formatters (#404)"
```

---

### Task 6: Update web_search plugin as example

**Files:**
- Modify: `plugins/web_search/tool.json`

- [ ] **Step 1: Add formatter_binary and custom formatter name to web_search tool.json**

This serves as documentation and a reference for plugin authors. The actual formatter binary can be added later; for now the field shows the supported format:

```json
{
  "plugin_type": "daemon_tool",
  "tools": [
    {
      "name": "web_search",
      "formatter": "default",
      ...
    }
  ]
}
```

(Keep as "default" until a real formatter binary is written. The point is the infrastructure is ready.)

- [ ] **Step 2: Final build and full test**

Run: `cargo build --release`
Run: `cargo test --release`
Expected: Clean build, all tests pass.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "docs: update web_search plugin with formatter_binary support example"
```
