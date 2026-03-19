# Plugin Refactor Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace long-lived JSON-RPC plugin subprocesses with short-lived per-call processes and static JSON tool definitions.

**Architecture:** Tool definitions move from Rust code to `tool.json` files. `PluginManager` becomes metadata-only (loads JSON, no process management). Each tool call spawns a fresh process (stdin JSON → stdout JSON → exit). `ChatToolCall` carries `plugin_name` and `sandboxed` so the client knows how to spawn.

**Tech Stack:** Rust, serde_json, bincode (protocol), Landlock (sandbox)

**Spec:** `docs/plans/2026-03-12-plugin-refactor-design.md`

---

## Chunk 1: Foundation (tool.json + PluginManager rewrite)

### Task 1: Create builtin tool.json

**Files:**
- Create: `crates/omnish-plugin/plugins/builtin/tool.json`

The JSON file contains all 4 built-in tool definitions. Copy descriptions from current Rust code.

- [ ] **Step 1: Create directory and tool.json**

Create `crates/omnish-plugin/plugins/builtin/tool.json`:

```json
{
  "plugin_type": "client_tool",
  "tools": [
    {
      "name": "bash",
      "description": "Execute a shell command and return its output. Use this to run shell commands, inspect files, check system state, or perform any operation the user asks about. Commands run in the specified shell and working directory.\n\nGuidelines:\n- The tool runs in a sandboxed environment with restricted write access.\n- Always quote file paths that contain spaces with double quotes.\n- If a command fails with a permission error, do not retry. Explain the error to the user.",
      "input_schema": {
        "type": "object",
        "properties": {
          "command": {
            "type": "string",
            "description": "The shell command to execute"
          },
          "shell": {
            "type": "string",
            "description": "Shell to use (e.g., /bin/bash, /bin/zsh). Defaults to bash if not specified."
          },
          "cwd": {
            "type": "string",
            "description": "Working directory for the command. Defaults to the user's current directory."
          },
          "timeout": {
            "type": "number",
            "description": "Timeout in seconds (default: 30)"
          }
        },
        "required": ["command"]
      },
      "status_template": "执行: {command}",
      "sandboxed": true
    },
    {
      "name": "read",
      "description": "Read a file from the local filesystem and return its contents with line numbers. The file_path must be an absolute path. By default reads up to 500 lines from the beginning. Use offset and limit for long files.\n\nGuidelines:\n- It is okay to read a file that does not exist; an error will be returned.\n- Any lines longer than 200 characters will be truncated.\n- This tool can only read files, not directories. To read a directory, use ls via the bash tool.\n- You can call multiple tools in a single response. It is always better to speculatively read multiple potentially useful files in parallel.\n- Returned contents are in format of \"line number→line contents\"",
      "input_schema": {
        "type": "object",
        "properties": {
          "file_path": {
            "type": "string",
            "description": "The absolute path to the file to read"
          },
          "offset": {
            "type": "integer",
            "description": "Line number to start reading from (1-based, default: 1)"
          },
          "limit": {
            "type": "integer",
            "description": "Maximum number of lines to read (default: 500)"
          }
        },
        "required": ["file_path"]
      },
      "status_template": "读取: {file_path}",
      "sandboxed": true
    },
    {
      "name": "edit",
      "description": "Perform exact string replacements in files. The old_string must match exactly. If old_string appears more than once and replace_all is false, the edit will fail — provide more surrounding context to make it unique, or set replace_all to true.\n\nGuidelines:\n- When editing text from Read tool output, preserve the exact indentation (tabs/spaces) as it appears AFTER the line number prefix (\"line number→\"). Never include any part of the line number prefix in old_string or new_string.\n- ALWAYS prefer editing existing files. NEVER write new files unless explicitly required.",
      "input_schema": {
        "type": "object",
        "properties": {
          "file_path": {
            "type": "string",
            "description": "Absolute path to the file to edit"
          },
          "old_string": {
            "type": "string",
            "description": "The exact text to find and replace"
          },
          "new_string": {
            "type": "string",
            "description": "The replacement text"
          },
          "replace_all": {
            "type": "boolean",
            "description": "Replace all occurrences (default: false)"
          }
        },
        "required": ["file_path", "old_string", "new_string"]
      },
      "status_template": "编辑: {file_path}",
      "sandboxed": false
    },
    {
      "name": "write",
      "description": "Write content to a file, creating it if it doesn't exist or overwriting if it does. Parent directories are created automatically. Use this for creating new files or completely replacing file contents.\n\nGuidelines:\n- file_path must be an absolute path.\n- Overwrites existing files. Use with care.\n- Runs in a sandboxed environment with restricted write access.",
      "input_schema": {
        "type": "object",
        "properties": {
          "file_path": {
            "type": "string",
            "description": "Absolute path to the file to write"
          },
          "content": {
            "type": "string",
            "description": "The content to write to the file"
          }
        },
        "required": ["file_path", "content"]
      },
      "status_template": "写入: {file_path}",
      "sandboxed": false
    }
  ]
}
```

- [ ] **Step 2: Validate JSON**

Run: `python3 -c "import json; json.load(open('crates/omnish-plugin/plugins/builtin/tool.json'))"`
Expected: no output (valid JSON)

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-plugin/plugins/builtin/tool.json
git commit -m "feat: add builtin tool.json with all 4 tool definitions"
```

---

### Task 2: Rewrite PluginManager to load from JSON

**Files:**
- Rewrite: `crates/omnish-daemon/src/plugin.rs` (lines 1-385)

Replace the entire file. The old code has `ExternalPlugin`, `load_custom_prompts`, `MockPlugin` tests — all removed. New code is metadata-only, loads from JSON files.

- [ ] **Step 1: Write tests for new PluginManager**

Add at the bottom of `crates/omnish-daemon/src/plugin.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tool_json(dir: &std::path::Path, name: &str, content: &str) {
        let plugin_dir = dir.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let mut f = std::fs::File::create(plugin_dir.join("tool.json")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn test_load_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = PluginManager::load(tmp.path());
        assert!(mgr.all_tools().is_empty());
    }

    #[test]
    fn test_load_single_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Run commands",
                "input_schema": {"type": "object", "properties": {}, "required": []},
                "status_template": "执行: {command}",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.all_tools().len(), 1);
        assert_eq!(mgr.all_tools()[0].name, "bash");
    }

    #[test]
    fn test_tool_plugin_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Run",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.tool_plugin_name("bash"), Some("builtin"));
    }

    #[test]
    fn test_tool_plugin_type() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "myplugin", r#"{
            "plugin_type": "daemon_tool",
            "tools": [{
                "name": "query",
                "description": "Query stuff",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": false
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.tool_plugin_type("query"), Some(PluginType::DaemonTool));
    }

    #[test]
    fn test_status_text_interpolation() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Run",
                "input_schema": {"type": "object"},
                "status_template": "执行: {command}",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        let input = serde_json::json!({"command": "ls -la"});
        assert_eq!(mgr.tool_status_text("bash", &input), "执行: ls -la");
    }

    #[test]
    fn test_malformed_json_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "bad", "not json{{{");
        write_tool_json(tmp.path(), "good", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "read",
                "description": "Read",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.all_tools().len(), 1);
        assert_eq!(mgr.all_tools()[0].name, "read");
    }

    #[test]
    fn test_duplicate_tool_name_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "plugin_a", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "First",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        write_tool_json(tmp.path(), "plugin_b", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Duplicate",
                "input_schema": {"type": "object"},
                "status_template": "",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        // First one wins, second skipped
        assert_eq!(mgr.all_tools().len(), 1);
    }

    #[test]
    fn test_status_text_missing_field() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [{
                "name": "bash",
                "description": "Run",
                "input_schema": {"type": "object"},
                "status_template": "执行: {command}",
                "sandboxed": true
            }]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        let input = serde_json::json!({"timeout": 30});
        // Missing {command} key — placeholder stays literal
        assert_eq!(mgr.tool_status_text("bash", &input), "执行: {command}");
    }

    #[test]
    fn test_tool_sandboxed() {
        let tmp = tempfile::tempdir().unwrap();
        write_tool_json(tmp.path(), "builtin", r#"{
            "plugin_type": "client_tool",
            "tools": [
                {"name": "bash", "description": "", "input_schema": {"type": "object"}, "status_template": "", "sandboxed": true},
                {"name": "edit", "description": "", "input_schema": {"type": "object"}, "status_template": "", "sandboxed": false}
            ]
        }"#);
        let mgr = PluginManager::load(tmp.path());
        assert_eq!(mgr.tool_sandboxed("bash"), Some(true));
        assert_eq!(mgr.tool_sandboxed("edit"), Some(false));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p omnish-daemon --lib plugin 2>&1`
Expected: compilation errors (PluginManager::load doesn't exist yet)

- [ ] **Step 3: Write PluginManager implementation**

Replace the entire `crates/omnish-daemon/src/plugin.rs` with:

```rust
use omnish_llm::tool::ToolDef;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Classifies whether a plugin's tools run on the daemon or the client side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    DaemonTool,
    ClientTool,
}

/// A single tool entry parsed from tool.json.
#[derive(Debug, Clone)]
struct ToolEntry {
    def: ToolDef,
    status_template: String,
    sandboxed: bool,
}

/// A plugin loaded from a tool.json file.
#[derive(Debug)]
struct PluginInfo {
    dir_name: String,
    plugin_type: PluginType,
    tools: Vec<ToolEntry>,
}

/// Metadata-only plugin manager. Loads tool definitions from JSON files.
/// Does not spawn or manage any processes.
pub struct PluginManager {
    plugins: Vec<PluginInfo>,
    /// Maps tool_name → (plugin_index, tool_index) for fast lookup.
    tool_index: HashMap<String, (usize, usize)>,
}

#[derive(Deserialize)]
struct ToolJsonFile {
    plugin_type: String,
    tools: Vec<ToolJsonEntry>,
}

#[derive(Deserialize)]
struct ToolJsonEntry {
    name: String,
    description: String,
    input_schema: serde_json::Value,
    #[serde(default)]
    status_template: String,
    #[serde(default = "default_sandboxed")]
    sandboxed: bool,
}

fn default_sandboxed() -> bool {
    true
}

impl PluginManager {
    /// Load all plugins from the given directory.
    /// Each subdirectory containing a `tool.json` is treated as a plugin.
    pub fn load(plugins_dir: &Path) -> Self {
        let mut plugins = Vec::new();
        let mut tool_index = HashMap::new();

        let mut entries: Vec<_> = match std::fs::read_dir(plugins_dir) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
            Err(_) => Vec::new(),
        };
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let tool_json = path.join("tool.json");
            if !tool_json.is_file() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().to_string();
            let content = match std::fs::read_to_string(&tool_json) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to read {}: {}", tool_json.display(), e);
                    continue;
                }
            };
            let parsed: ToolJsonFile = match serde_json::from_str(&content) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Malformed {}: {}", tool_json.display(), e);
                    continue;
                }
            };

            let plugin_type = match parsed.plugin_type.as_str() {
                "client_tool" => PluginType::ClientTool,
                _ => PluginType::DaemonTool,
            };

            let plugin_idx = plugins.len();
            let mut tools = Vec::new();
            for te in parsed.tools {
                if tool_index.contains_key(&te.name) {
                    tracing::warn!(
                        "Duplicate tool name '{}' in {}, skipping",
                        te.name,
                        tool_json.display()
                    );
                    continue;
                }
                let tool_idx = tools.len();
                tool_index.insert(te.name.clone(), (plugin_idx, tool_idx));
                tools.push(ToolEntry {
                    def: ToolDef {
                        name: te.name,
                        description: te.description,
                        input_schema: te.input_schema,
                    },
                    status_template: te.status_template,
                    sandboxed: te.sandboxed,
                });
            }

            tracing::info!(
                "Loaded plugin '{}' with {} tools",
                dir_name,
                tools.len()
            );
            plugins.push(PluginInfo {
                dir_name,
                plugin_type,
                tools,
            });
        }

        Self {
            plugins,
            tool_index,
        }
    }

    /// Collect all tool definitions from all plugins.
    pub fn all_tools(&self) -> Vec<ToolDef> {
        self.plugins
            .iter()
            .flat_map(|p| p.tools.iter().map(|t| t.def.clone()))
            .collect()
    }

    /// Get the status text for a tool call, interpolating {field} from input.
    pub fn tool_status_text(&self, tool_name: &str, input: &serde_json::Value) -> String {
        if let Some(&(pi, ti)) = self.tool_index.get(tool_name) {
            let template = &self.plugins[pi].tools[ti].status_template;
            interpolate_template(template, input)
        } else {
            format!("执行 {}...", tool_name)
        }
    }

    /// Return the plugin type that owns the given tool.
    pub fn tool_plugin_type(&self, tool_name: &str) -> Option<PluginType> {
        self.tool_index
            .get(tool_name)
            .map(|&(pi, _)| self.plugins[pi].plugin_type)
    }

    /// Return the plugin directory name for the given tool.
    pub fn tool_plugin_name(&self, tool_name: &str) -> Option<&str> {
        self.tool_index
            .get(tool_name)
            .map(|&(pi, _)| self.plugins[pi].dir_name.as_str())
    }

    /// Return whether the tool should be sandboxed.
    pub fn tool_sandboxed(&self, tool_name: &str) -> Option<bool> {
        self.tool_index
            .get(tool_name)
            .map(|&(pi, ti)| self.plugins[pi].tools[ti].sandboxed)
    }
}

/// Replace `{field_name}` in template with values from the JSON input.
fn interpolate_template(template: &str, input: &serde_json::Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = input.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{}}}", key);
            let replacement = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }
    }
    result
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p omnish-daemon --lib plugin 2>&1`
Expected: all 7 tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs
git commit -m "refactor: rewrite PluginManager to load from tool.json files"
```

---

## Chunk 2: Protocol + Daemon wiring (single atomic commit)

Note: Tasks 3 and 4 must be applied together — the daemon binary won't
compile between them. They form one commit.

### Task 3: Protocol changes + Daemon wiring

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs:230-237` (ChatToolCall struct)
- Modify: `crates/omnish-protocol/src/message.rs:474` (PROTOCOL_VERSION bump)
- Modify: `crates/omnish-protocol/src/message.rs:588-594` (message_variant_guard test)
- Modify: `crates/omnish-daemon/src/main.rs:148-169` (plugin initialization)
- Modify: `crates/omnish-daemon/src/server.rs` (multiple locations)

- [ ] **Step 1: Add plugin_name and sandboxed fields to ChatToolCall**

In `crates/omnish-protocol/src/message.rs`, add two fields to `ChatToolCall`:

```rust
pub struct ChatToolCall {
    pub request_id: String,
    pub thread_id: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub input: String,
    pub plugin_name: String,
    pub sandboxed: bool,
}
```

- [ ] **Step 2: Bump PROTOCOL_VERSION**

In `crates/omnish-protocol/src/message.rs` line 9, change:
```rust
pub const PROTOCOL_VERSION: u32 = 4;
```

- [ ] **Step 3: Update message_variant_guard test**

In `crates/omnish-protocol/src/message.rs` lines 588-594, add the new fields:
```rust
Message::ChatToolCall(ChatToolCall {
    request_id: String::new(),
    thread_id: String::new(),
    tool_name: String::new(),
    tool_call_id: String::new(),
    input: String::new(),
    plugin_name: String::new(),
    sandboxed: true,
}),
```

- [ ] **Step 4: Update daemon main.rs plugin initialization**

Replace the plugin registration block (lines ~148-169) in
`crates/omnish-daemon/src/main.rs`:

```rust
    // Load plugins from ~/.omnish/plugins/
    let plugins_dir = omnish_common::config::omnish_dir().join("plugins");
    let plugin_mgr = Arc::new(omnish_daemon::plugin::PluginManager::load(&plugins_dir));
```

Remove imports of `ExternalPlugin`, old `PluginManager::new()`, `.register()`,
`.load_external_plugins()`, and `omnish_plugin::tools::*`.

- [ ] **Step 5: Update server.rs imports and ChatToolCall construction**

Remove `Plugin` from imports (it no longer exists in `omnish_daemon::plugin`).
Keep `PluginManager` and `PluginType`.

Update both ChatToolCall construction sites in `server.rs` (~lines 437-442
and 578-583) to populate `plugin_name` and `sandboxed`:

```rust
plugin_name: plugin_mgr.tool_plugin_name(&tc.name).unwrap_or("builtin").to_string(),
sandboxed: plugin_mgr.tool_sandboxed(&tc.name).unwrap_or(true),
```

- [ ] **Step 6: Remove plugin_mgr.call_tool() calls**

In `crates/omnish-daemon/src/server.rs`, the `plugin_mgr.call_tool()` calls
at ~lines 455 and 607 handle daemon-side plugin tools. Since all current
plugin tools are client-side, replace these with an error/unreachable for
non-command_query daemon tools:

```rust
// Was: plugin_mgr.call_tool(&tc.name, &tc.input)
// All plugin tools are now client-side; only command_query is daemon-side
omnish_llm::tool::ToolResult {
    tool_use_id: String::new(),
    content: format!("Error: daemon-side tool '{}' not found", tc.name),
    is_error: true,
}
```

- [ ] **Step 7: Build and test**

Run: `cargo build --workspace 2>&1 | tail -10`
Expected: compiles with no errors

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 8: Commit**

```bash
git add crates/omnish-protocol/src/message.rs crates/omnish-daemon/src/main.rs crates/omnish-daemon/src/server.rs
git commit -m "refactor: protocol v4 + wire daemon to JSON-based PluginManager"
```

---

## Chunk 3: Plugin binary + tool struct simplification

### Task 5: Simplify omnish-plugin binary to single-shot

**Files:**
- Rewrite: `crates/omnish-plugin/src/main.rs` (lines 1-141)

- [ ] **Step 1: Rewrite main.rs to single-shot execution**

Replace `crates/omnish-plugin/src/main.rs`:

```rust
use omnish_plugin::tools::bash::BashTool;
use omnish_plugin::tools::edit::EditTool;
use omnish_plugin::tools::read::ReadTool;
use omnish_plugin::tools::write::WriteTool;
use omnish_llm::tool::Tool;
use std::io::{BufRead, Write};

#[derive(serde::Deserialize)]
struct Request {
    name: String,
    input: serde_json::Value,
}

#[derive(serde::Serialize)]
struct Response {
    content: String,
    is_error: bool,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 {
        if args[1] == "--version" || args[1] == "-V" {
            println!("omnish-plugin {}", omnish_common::VERSION);
            return;
        }
    }

    let stdin = std::io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
        eprintln!("omnish-plugin: no input on stdin");
        std::process::exit(1);
    }

    let req: Request = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            let resp = Response {
                content: format!("Invalid input: {e}"),
                is_error: true,
            };
            println!("{}", serde_json::to_string(&resp).unwrap());
            return;
        }
    };

    let result = match req.name.as_str() {
        "bash" => BashTool::new().execute(&req.input),
        "read" => ReadTool::new().execute(&req.input),
        "edit" => EditTool::new().execute(&req.input),
        "write" => WriteTool::new().execute(&req.input),
        other => omnish_llm::tool::ToolResult {
            tool_use_id: String::new(),
            content: format!("Unknown tool: {other}"),
            is_error: true,
        },
    };

    let resp = Response {
        content: result.content,
        is_error: result.is_error,
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", serde_json::to_string(&resp).unwrap());
}
```

- [ ] **Step 2: Build and verify**

Run: `cargo build -p omnish-plugin 2>&1 | tail -5`
Expected: compiles

- [ ] **Step 3: Test manually with echo pipe**

Run: `echo '{"name":"bash","input":{"command":"echo hello"}}' | cargo run -p omnish-plugin 2>/dev/null`
Expected: `{"content":"hello\n","is_error":false}` (or similar)

Run: `echo '{"name":"read","input":{"file_path":"/etc/hostname"}}' | cargo run -p omnish-plugin 2>/dev/null`
Expected: JSON with file contents

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-plugin/src/main.rs
git commit -m "refactor: simplify omnish-plugin binary to single-shot execution"
```

---

### Task 6: Remove Plugin trait, simplify tool structs and command_query

**Files:**
- Modify: `crates/omnish-plugin/src/tools/bash.rs:134-214` (remove definition + Plugin impl)
- Modify: `crates/omnish-plugin/src/tools/read.rs:19-161` (remove definition + Plugin impl)
- Modify: `crates/omnish-plugin/src/tools/edit.rs:12-172` (remove definition + Plugin impl)
- Modify: `crates/omnish-plugin/src/tools/write.rs:55-125` (remove definition + Plugin impl)
- Modify: `crates/omnish-plugin/src/lib.rs` (remove Plugin trait, JSON-RPC types, PluginProcess)
- Modify: `crates/omnish-llm/src/tool.rs:29-34` (remove definition from Tool trait)
- Modify: `crates/omnish-daemon/src/tools/command_query.rs:147-170` (remove Plugin impl)
- Modify: `crates/omnish-daemon/src/server.rs:295` (command_query.definition() call)
- Modify: `crates/omnish-plugin/src/main.rs` (remove dead Tool import after changes)

For each built-in tool file:
- Remove `impl Tool for XxxTool` block (contains `definition()` with description strings)
- Remove `impl Plugin for XxxTool` block
- Keep `execute()` method — move it to `impl XxxTool` (inherent impl)

- [ ] **Step 1: Simplify bash.rs**

In `crates/omnish-plugin/src/tools/bash.rs`:
- Remove `impl Tool for BashTool` block (lines ~134-168, contains `definition()`)
- Remove `impl Plugin for BashTool` block (lines ~189-214)
- Move `execute()` to `impl BashTool` as a public method.

- [ ] **Step 2: Simplify read.rs**

Same pattern: remove `impl Tool for ReadTool` and `impl Plugin for ReadTool`.
Keep `execute()` as `impl ReadTool`.

- [ ] **Step 3: Simplify edit.rs**

Same pattern for EditTool.

- [ ] **Step 4: Simplify write.rs**

Same pattern for WriteTool.

- [ ] **Step 5: Remove Plugin trait and JSON-RPC types from lib.rs**

In `crates/omnish-plugin/src/lib.rs`:
- Remove `Plugin` trait (lines 32-47)
- Remove `PluginType` enum (lines 26-29) — daemon has its own now
- Remove `JsonRpcRequest`, `JsonRpcResponse`, `ExecuteResult` structs (lines 50-75)
- Remove `PluginProcess` struct and its impl (lines 113-270)
- Keep `apply_sandbox()` function (lines 82-108)
- Keep `pub mod tools;`

- [ ] **Step 6: Remove definition() from Tool trait in omnish-llm**

In `crates/omnish-llm/src/tool.rs`, simplify the `Tool` trait:

```rust
pub trait Tool {
    fn execute(&self, input: &serde_json::Value) -> ToolResult;
}
```

- [ ] **Step 7: Update command_query.rs**

In `crates/omnish-daemon/src/tools/command_query.rs`:
- Remove `use omnish_plugin::Plugin;` import (line 1)
- Remove `impl Plugin for CommandQueryTool` block (lines 147-170)
- Convert `definition()` to an inherent method on `CommandQueryTool`
  (move from `impl Tool` to `impl CommandQueryTool`). Keep the method body
  unchanged — it's called by `server.rs` at `build_chat_setup()`.
- Keep `status_text()` as an inherent method (move from `Plugin` impl to
  `impl CommandQueryTool`). Called by server.rs at ~line 564.

- [ ] **Step 8: Update omnish-plugin main.rs**

Remove the `use omnish_llm::tool::Tool;` import from
`crates/omnish-plugin/src/main.rs` (now dead — `execute()` is an inherent
method, not a trait method).

- [ ] **Step 9: Build workspace**

Run: `cargo build --workspace 2>&1 | tail -10`
Expected: compiles. Fix any remaining references to removed items.

- [ ] **Step 10: Run all tests**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: all tests pass. Tool tests call `.execute()` directly — since it's
now an inherent method, the calls still resolve. Remove any dead `use` imports
for `Tool` trait in test modules.

- [ ] **Step 11: Commit**

```bash
git add crates/omnish-plugin/ crates/omnish-llm/src/tool.rs crates/omnish-daemon/src/tools/command_query.rs crates/omnish-daemon/src/server.rs crates/omnish-plugin/src/main.rs
git commit -m "refactor: remove Plugin trait, JSON-RPC types, simplify tool structs"
```

---

## Chunk 4: Client-side changes

### Task 7: Rewrite ClientPluginManager for short-lived processes

**Files:**
- Rewrite: `crates/omnish-client/src/client_plugin.rs` (lines 1-75)

- [ ] **Step 1: Rewrite client_plugin.rs**

Replace `crates/omnish-client/src/client_plugin.rs`:

```rust
//! Client-side tool execution via short-lived plugin processes.
//! Spawns a fresh process per tool call: writes JSON to stdin, reads JSON from stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// Executes client-side tools by spawning short-lived plugin processes.
pub struct ClientPluginManager {
    plugin_bin: std::path::PathBuf,
}

#[derive(serde::Deserialize)]
struct PluginResponse {
    content: String,
    #[serde(default)]
    is_error: bool,
}

impl ClientPluginManager {
    pub fn new() -> Self {
        let plugin_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("omnish-plugin")))
            .unwrap_or_else(|| std::path::PathBuf::from("omnish-plugin"));
        Self { plugin_bin }
    }

    /// Execute a tool via a short-lived plugin process.
    ///
    /// - `plugin_name`: "builtin" or external plugin directory name
    /// - `tool_name`: the specific tool within the plugin
    /// - `input`: tool input JSON
    /// - `cwd`: optional working directory to inject into input
    /// - `sandboxed`: whether to apply Landlock sandbox
    pub fn execute_tool(
        &self,
        plugin_name: &str,
        tool_name: &str,
        input: &serde_json::Value,
        cwd: Option<&str>,
        sandboxed: bool,
    ) -> (String, bool) {
        let executable = if plugin_name == "builtin" {
            self.plugin_bin.clone()
        } else {
            omnish_common::config::omnish_dir()
                .join("plugins")
                .join(plugin_name)
                .join(plugin_name)
        };

        // Inject cwd into input if available
        let effective_input = if let Some(cwd) = cwd {
            let mut patched = input.clone();
            if let Some(obj) = patched.as_object_mut() {
                obj.insert("cwd".to_string(), serde_json::Value::String(cwd.to_string()));
            }
            patched
        } else {
            input.clone()
        };

        let request = serde_json::json!({
            "name": tool_name,
            "input": effective_input,
        });

        let data_dir = omnish_common::config::omnish_dir()
            .join("data")
            .join(plugin_name);
        let _ = std::fs::create_dir_all(&data_dir);

        let mut cmd = Command::new(&executable);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        // Apply sandbox via pre_exec if requested
        if sandboxed {
            let data_dir_clone = data_dir.clone();
            unsafe {
                cmd.pre_exec(move || {
                    omnish_plugin::apply_sandbox(&data_dir_clone).map_err(|e| {
                        std::io::Error::new(std::io::ErrorKind::PermissionDenied, e)
                    })
                });
            }
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return (format!("Failed to spawn plugin '{}': {}", plugin_name, e), true),
        };

        // Write request to stdin
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{}", serde_json::to_string(&request).unwrap());
            // stdin dropped here, closing it
        }

        // Read response from stdout
        let result = if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => ("Plugin produced no output".to_string(), true),
                Ok(_) => match serde_json::from_str::<PluginResponse>(&line) {
                    Ok(resp) => (resp.content, resp.is_error),
                    Err(e) => (format!("Invalid plugin response: {e}"), true),
                },
                Err(e) => (format!("Failed to read plugin output: {e}"), true),
            }
        } else {
            ("No stdout from plugin".to_string(), true)
        };

        let _ = child.wait();
        result
    }
}
```

- [ ] **Step 2: Build to verify**

Run: `cargo build -p omnish-client 2>&1 | tail -5`
Expected: compilation errors from main.rs (signature changed). That's expected.

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-client/src/client_plugin.rs
git commit -m "refactor: rewrite ClientPluginManager for short-lived process spawning"
```

---

### Task 8: Update client main.rs for new ChatToolCall fields

**Files:**
- Modify: `crates/omnish-client/src/main.rs` (~lines 2538-2546)

- [ ] **Step 1: Update ChatToolCall handler**

In `crates/omnish-client/src/main.rs`, find the `Message::ChatToolCall` match
arm (around line 2538). Update the `execute_tool()` call to pass the new
fields from ChatToolCall:

```rust
Message::ChatToolCall(tc) => {
    let tool_name = tc.tool_name.clone();
    let plugin_name = tc.plugin_name.clone();
    let sandboxed = tc.sandboxed;
    let tool_input: serde_json::Value = serde_json::from_str(&tc.input).unwrap_or_default();
    let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
    let plugins = Arc::clone(&client_plugins);
    let (content, is_error) = tokio::task::spawn_blocking(move || {
        plugins.execute_tool(&plugin_name, &tool_name, &tool_input, shell_cwd.as_deref(), sandboxed)
    }).await.unwrap_or_else(|_| ("Tool execution panicked".to_string(), true));
    // ... rest unchanged (send ChatToolResult back)
}
```

- [ ] **Step 2: Build full workspace**

Run: `cargo build --workspace 2>&1 | tail -10`
Expected: compiles with no errors

- [ ] **Step 3: Run all tests**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "refactor: update client to use new ChatToolCall fields for tool execution"
```

---

## Chunk 5: Cleanup + install mechanism

### Task 9: Install builtin tool.json to plugins directory

**Files:**
- Modify: `crates/omnish-daemon/src/main.rs`

The daemon needs the `builtin/tool.json` to exist in `~/.omnish/plugins/`.
On startup, if `~/.omnish/plugins/builtin/tool.json` doesn't exist, copy
the default from the source-embedded version.

- [ ] **Step 1: Embed and install builtin tool.json**

In `crates/omnish-daemon/src/main.rs`, before calling `PluginManager::load()`,
add code to install the default builtin tool.json if missing:

```rust
    // Ensure builtin tool.json exists
    let builtin_dir = plugins_dir.join("builtin");
    let builtin_tool_json = builtin_dir.join("tool.json");
    if !builtin_tool_json.exists() {
        let _ = std::fs::create_dir_all(&builtin_dir);
        let default_json = include_str!("../../omnish-plugin/plugins/builtin/tool.json");
        let _ = std::fs::write(&builtin_tool_json, default_json);
    }
```

- [ ] **Step 2: Build and verify**

Run: `cargo build -p omnish-daemon 2>&1 | tail -5`
Expected: compiles

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-daemon/src/main.rs
git commit -m "feat: auto-install builtin tool.json on first daemon startup"
```

---

### Task 10: Final cleanup

**Files:**
- Verify: all removed code is gone
- Verify: no dead imports or unused dependencies

- [ ] **Step 1: Check for dead code warnings**

Run: `cargo build --workspace 2>&1 | grep "warning:"`
Fix any warnings about unused imports, dead code, etc.

- [ ] **Step 2: Run full test suite**

Run: `cargo test --workspace 2>&1`
Expected: all tests pass

- [ ] **Step 3: Build release**

Run: `cargo build --release -p omnish-client -p omnish-daemon -p omnish-plugin 2>&1 | tail -5`
Expected: compiles

- [ ] **Step 4: Manual smoke test**

Run: `echo '{"name":"bash","input":{"command":"echo smoke"}}' | ./target/release/omnish-plugin`
Expected: `{"content":"smoke\n","is_error":false}`

- [ ] **Step 5: Commit any remaining cleanup**

```bash
git add -A
git commit -m "chore: clean up dead code from plugin refactor"
```
