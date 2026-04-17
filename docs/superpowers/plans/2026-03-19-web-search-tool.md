# Web Search Tool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Brave Search API web search tool as a daemon-side external plugin, with a generic parameter injection mechanism for tool configuration.

**Architecture:** Three layers: (1) generic param injection in daemon config + plugin system, (2) daemon-side external plugin subprocess execution, (3) web_search shell script plugin. Each layer is independently testable.

**Tech Stack:** Rust (config parsing, plugin manager, daemon dispatch), Bash/curl/jq (plugin script)

**Spec:** `docs/superpowers/specs/2026-03-19-web-search-tool-design.md`

---

### Task 1: Add `tools` config to DaemonConfig

**Files:**
- Modify: `crates/omnish-common/src/config.rs:218-230` (DaemonConfig struct)
- Modify: `crates/omnish-common/Cargo.toml` (add serde_json dependency)

- [ ] **Step 1: Add serde_json to omnish-common dependencies**

In `crates/omnish-common/Cargo.toml`, add under `[dependencies]`:
```toml
serde_json = "1"
```

- [ ] **Step 2: Add `tools` field to DaemonConfig**

In `crates/omnish-common/src/config.rs`, add to the `DaemonConfig` struct after the `plugins` field:

```rust
/// Per-tool parameter overrides.
/// Example: [tools.web_search] api_key = "..."
#[serde(default)]
pub tools: HashMap<String, HashMap<String, serde_json::Value>>,
```

Add `use serde_json;` if not already imported (it's not - `serde_json` isn't currently used in config.rs).

Update the `Default` impl to include `tools: HashMap::new()`.

- [ ] **Step 3: Add `tools` to example daemon.toml**

In `config/daemon.toml`, add at the bottom:

```toml
# Per-tool parameter injection (values merged into tool call inputs)
# [tools.web_search]
# api_key = "BSAxxxxxxxx"
# base_url = "https://api.search.brave.com/res/v1/web/search"
```

- [ ] **Step 4: Test that config parses correctly**

Run: `cargo test -p omnish-common`

Also verify manually that existing daemon.toml still parses:
```bash
cargo run -p omnish-daemon -- --check-config 2>&1 || echo "No --check-config flag; just build"
cargo build -p omnish-common
```

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-common/src/config.rs crates/omnish-common/Cargo.toml config/daemon.toml
git commit -m "feat: add [tools] config section for per-tool parameter injection"
```

---

### Task 2: Add `params` to ToolOverrideEntry and PluginManager

**Files:**
- Modify: `crates/omnish-daemon/src/plugin.rs:96-104` (ToolOverrideEntry)
- Modify: `crates/omnish-daemon/src/plugin.rs:31-35` (PromptCache)
- Modify: `crates/omnish-daemon/src/plugin.rs:246-290` (reload_overrides)

- [ ] **Step 1: Add `params` field to ToolOverrideEntry**

In `crates/omnish-daemon/src/plugin.rs`, update `ToolOverrideEntry`:

```rust
#[derive(Deserialize)]
struct ToolOverrideEntry {
    /// Replaces the tool description entirely.
    #[serde(default)]
    description: Option<DescriptionValue>,
    /// Appended to the tool description (ignored if `description` is set).
    #[serde(default)]
    append: Option<DescriptionValue>,
    /// Extra parameters merged into tool call input at execution time.
    #[serde(default)]
    params: Option<HashMap<String, serde_json::Value>>,
}
```

- [ ] **Step 2: Add `params` to PromptCache**

Update the `PromptCache` struct:

```rust
struct PromptCache {
    /// tool_name → effective description (base with override/append applied)
    descriptions: HashMap<String, String>,
    /// tool_name → override params from tool.override.json
    override_params: HashMap<String, HashMap<String, serde_json::Value>>,
}
```

Update the initialization in `PluginManager::load` to include `override_params: HashMap::new()`.

- [ ] **Step 3: Populate `override_params` in `reload_overrides`**

In the `reload_overrides` method, after processing descriptions, add params collection:

```rust
// Inside the loop over plugin tools, after description handling:
if let Some(ref of_) = overrides {
    if let Some(ovr) = of_.tools.get(&te.def.name) {
        // ... existing description logic ...
        if let Some(ref p) = ovr.params {
            override_params.insert(te.def.name.clone(), p.clone());
        }
    }
}
```

Add `let mut override_params = HashMap::new();` at the top of `reload_overrides`, and store it in the cache:
```rust
cache.override_params = override_params;
```

- [ ] **Step 4: Add `tool_override_params` accessor**

Add a new public method to `PluginManager`:

```rust
/// Return override params for the given tool (from tool.override.json).
pub fn tool_override_params(&self, tool_name: &str) -> Option<HashMap<String, serde_json::Value>> {
    let cache = self.prompt_cache.read().unwrap();
    cache.override_params.get(tool_name).cloned()
}
```

- [ ] **Step 5: Add `plugin_executable` method**

Add to `PluginManager`:

```rust
/// Return the executable path for the plugin that owns the given tool.
pub fn plugin_executable(&self, tool_name: &str) -> Option<std::path::PathBuf> {
    self.tool_index.get(tool_name).map(|&(pi, _)| {
        let dir_name = &self.plugins[pi].dir_name;
        self.plugins_dir.join(dir_name).join(dir_name)
    })
}
```

- [ ] **Step 6: Write tests for params and plugin_executable**

Add to the existing `#[cfg(test)] mod tests` in `plugin.rs`:

```rust
#[test]
fn test_override_params() {
    let tmp = tempfile::tempdir().unwrap();
    write_tool_json(tmp.path(), "myplugin", r#"{
        "plugin_type": "daemon_tool",
        "tools": [{
            "name": "my_tool",
            "description": "My tool",
            "input_schema": {"type": "object"},
            "status_template": ""
        }]
    }"#);
    write_tool_override(tmp.path(), "myplugin", r#"{
        "tools": {
            "my_tool": {
                "params": {
                    "api_key": "test123",
                    "count": 10
                }
            }
        }
    }"#);
    let mgr = PluginManager::load(tmp.path());
    let params = mgr.tool_override_params("my_tool").unwrap();
    assert_eq!(params["api_key"], serde_json::json!("test123"));
    assert_eq!(params["count"], serde_json::json!(10));
}

#[test]
fn test_no_override_params() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = PluginManager::load(tmp.path());
    assert!(mgr.tool_override_params("bash").is_none());
}

#[test]
fn test_plugin_executable() {
    let tmp = tempfile::tempdir().unwrap();
    write_tool_json(tmp.path(), "web_search", r#"{
        "plugin_type": "daemon_tool",
        "tools": [{
            "name": "web_search",
            "description": "Search",
            "input_schema": {"type": "object"},
            "status_template": ""
        }]
    }"#);
    let mgr = PluginManager::load(tmp.path());
    let exe = mgr.plugin_executable("web_search").unwrap();
    assert_eq!(exe, tmp.path().join("web_search").join("web_search"));
}
```

- [ ] **Step 7: Write merge_tool_params test**

Add a test for the merge helper (this tests the logic that will be used in server.rs, but we test the core merge behavior here):

```rust
#[test]
fn test_merge_precedence() {
    // Simulate: LLM input has query+count, override has count+api_key, config has api_key
    let mut input = serde_json::json!({"query": "test", "count": 3});
    let override_params: HashMap<String, serde_json::Value> = [
        ("count".to_string(), serde_json::json!(5)),
        ("api_key".to_string(), serde_json::json!("override_key")),
    ].into();
    let config_params: HashMap<String, serde_json::Value> = [
        ("api_key".to_string(), serde_json::json!("config_key")),
    ].into();

    // Apply override params (layer 2)
    if let Some(obj) = input.as_object_mut() {
        for (k, v) in &override_params { obj.insert(k.clone(), v.clone()); }
    }
    // Apply config params (layer 3, highest precedence)
    if let Some(obj) = input.as_object_mut() {
        for (k, v) in &config_params { obj.insert(k.clone(), v.clone()); }
    }

    assert_eq!(input["query"], "test");           // LLM input preserved
    assert_eq!(input["count"], 5);                // override wins over LLM
    assert_eq!(input["api_key"], "config_key");   // config wins over override
}
```

- [ ] **Step 8: Run tests**

Run: `cargo test -p omnish-daemon -- plugin`
Expected: all tests pass

- [ ] **Step 9: Commit**

```bash
git add crates/omnish-daemon/src/plugin.rs
git commit -m "feat: add params support to tool.override.json and plugin_executable method"
```

---

### Task 3: Implement param merge + daemon-side plugin execution in server.rs

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs:50-58` (DaemonServer struct)
- Modify: `crates/omnish-daemon/src/server.rs:60-77` (DaemonServer::new)
- Modify: `crates/omnish-daemon/src/server.rs:729-739` (daemon tool dispatch)
- Modify: `crates/omnish-daemon/src/main.rs` (pass tool_params to DaemonServer)

- [ ] **Step 1: Add `tool_params` to DaemonServer**

In `server.rs`, add a field to `DaemonServer`:

```rust
pub struct DaemonServer {
    // ... existing fields ...
    /// Per-tool params from daemon.toml [tools.X] sections
    tool_params: HashMap<String, HashMap<String, serde_json::Value>>,
}
```

Update `new()` to accept and store it:
```rust
pub fn new(
    // ... existing params ...
    tool_params: HashMap<String, HashMap<String, serde_json::Value>>,
) -> Self {
    Self {
        // ... existing fields ...
        tool_params,
    }
}
```

- [ ] **Step 2: Add `merge_tool_params` helper function**

Add a free function in `server.rs`:

```rust
/// Shallow-merge params into a JSON object. Source keys overwrite target keys.
fn merge_tool_params(target: &mut serde_json::Value, params: &HashMap<String, serde_json::Value>) {
    if let Some(obj) = target.as_object_mut() {
        for (k, v) in params {
            obj.insert(k.clone(), v.clone());
        }
    }
}
```

- [ ] **Step 3: Add `execute_daemon_plugin` helper function**

Add a function in `server.rs` that spawns a daemon-side external plugin subprocess:

```rust
/// Execute a daemon-side external plugin by spawning a subprocess.
/// Uses the same stdin/stdout JSON protocol as client-side plugins.
async fn execute_daemon_plugin(
    executable: &std::path::Path,
    tool_name: &str,
    input: &serde_json::Value,
) -> omnish_llm::tool::ToolResult {
    use tokio::process::Command;
    use tokio::io::AsyncWriteExt;

    let request = serde_json::json!({
        "name": tool_name,
        "input": input,
    });

    let mut child = match Command::new(executable)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return omnish_llm::tool::ToolResult {
                tool_use_id: String::new(),
                content: format!("Failed to spawn plugin '{}': {}", executable.display(), e),
                is_error: true,
            };
        }
    };

    // Write request to stdin
    if let Some(mut stdin) = child.stdin.take() {
        let data = serde_json::to_string(&request).unwrap();
        let _ = stdin.write_all(data.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        drop(stdin);
    }

    // Wait with timeout
    let timeout = std::time::Duration::from_secs(30);
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return omnish_llm::tool::ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Plugin exited with {}: {}", output.status, stderr.trim()),
                    is_error: true,
                };
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            #[derive(serde::Deserialize)]
            struct PluginResponse {
                content: String,
                #[serde(default)]
                is_error: bool,
            }
            match serde_json::from_str::<PluginResponse>(stdout.trim()) {
                Ok(resp) => omnish_llm::tool::ToolResult {
                    tool_use_id: String::new(),
                    content: resp.content,
                    is_error: resp.is_error,
                },
                Err(e) => omnish_llm::tool::ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Invalid plugin response: {e}"),
                    is_error: true,
                },
            }
        }
        Ok(Err(e)) => omnish_llm::tool::ToolResult {
            tool_use_id: String::new(),
            content: format!("Plugin I/O error: {e}"),
            is_error: true,
        },
        Err(_) => {
            // Timeout: the future was dropped, which drops the Child,
            // killing the process. Return error.
            omnish_llm::tool::ToolResult {
                tool_use_id: String::new(),
                content: "Plugin timed out (30s)".to_string(),
                is_error: true,
            }
        }
    }
}
```

- [ ] **Step 4: Wire up param merge and plugin execution in tool dispatch**

In `server.rs`, at the tool dispatch section (around line 716-739), update the daemon-side branch. The current code:

```rust
} else {
    // Daemon-side tool: execute directly
    let mut result = if tc.name == "omnish_list_history" || tc.name == "omnish_get_output" {
        state.command_query_tool.execute(&tc.name, &tc.input)
    } else {
        omnish_llm::tool::ToolResult {
            tool_use_id: String::new(),
            content: format!("Unknown daemon tool: {}", tc.name),
            is_error: true,
        }
    };
```

Replace with:

```rust
} else {
    // Daemon-side tool: execute directly
    // Merge params: override.json defaults, then daemon.toml overrides
    let mut merged_input = tc.input.clone();
    if let Some(override_params) = plugin_mgr.tool_override_params(&tc.name) {
        merge_tool_params(&mut merged_input, &override_params);
    }
    if let Some(config_params) = self.tool_params.get(&tc.name) {
        merge_tool_params(&mut merged_input, config_params);
    }

    let mut result = if tc.name == "omnish_list_history" || tc.name == "omnish_get_output" {
        state.command_query_tool.execute(&tc.name, &merged_input)
    } else if let Some(exe) = plugin_mgr.plugin_executable(&tc.name) {
        execute_daemon_plugin(&exe, &tc.name, &merged_input).await
    } else {
        omnish_llm::tool::ToolResult {
            tool_use_id: String::new(),
            content: format!("Unknown daemon tool: {}", tc.name),
            is_error: true,
        }
    };
```

Note: Also apply param merge to the client-side branch (line 724). Update the `input` field in `ChatToolCall`:

```rust
// Before constructing ChatToolCall, merge params:
let mut merged_input = tc.input.clone();
if let Some(override_params) = plugin_mgr.tool_override_params(&tc.name) {
    merge_tool_params(&mut merged_input, &override_params);
}
if let Some(config_params) = self.tool_params.get(&tc.name) {
    merge_tool_params(&mut merged_input, config_params);
}

messages.push(Message::ChatToolCall(ChatToolCall {
    // ...
    input: serde_json::to_string(&merged_input).unwrap_or_default(),
    // ...
}));
```

- [ ] **Step 5: Pass tool_params from main.rs**

In `crates/omnish-daemon/src/main.rs`, find where `DaemonServer::new()` is called. Pass `config.tools`:

```rust
let server = DaemonServer::new(
    // ... existing args ...
    config.tools,
);
```

- [ ] **Step 6: Build and verify**

Run: `cargo build -p omnish-daemon`
Expected: compiles without errors

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/main.rs
git commit -m "feat: add param injection and daemon-side external plugin execution"
```

---

### Task 4: Create the web_search plugin

**Files:**
- Create: `plugins/web_search/web_search` (shell script)
- Create: `plugins/web_search/tool.json`

- [ ] **Step 1: Create plugin directory**

```bash
mkdir -p plugins/web_search
```

- [ ] **Step 2: Create tool.json**

Create `plugins/web_search/tool.json`:

```json
{
  "plugin_type": "daemon_tool",
  "tools": [
    {
      "name": "web_search",
      "description": [
        "Search the web using Brave Search API and return results.",
        "Returns search result titles, URLs, and snippets formatted as markdown.",
        "Use this tool to find up-to-date information beyond your knowledge cutoff.",
        "",
        "After answering the user's question based on search results, include a Sources section:",
        "Sources:",
        "- [Title](URL)",
        "",
        "Usage notes:",
        "- Domain filtering supports include and exclude: \"stackoverflow.com,-reddit.com\"",
        "- Prefix a domain with - to exclude it from results"
      ],
      "input_schema": {
        "type": "object",
        "properties": {
          "query": {
            "type": "string",
            "description": "The search query"
          },
          "count": {
            "type": "integer",
            "description": "Number of results to return (default: 5, max: 20)"
          },
          "domain_filter": {
            "type": "string",
            "description": "Comma-separated domains to include or exclude. Prefix with - to exclude. Example: \"stackoverflow.com,-reddit.com\""
          }
        },
        "required": ["query"]
      },
      "status_template": "搜索: {query}",
      "sandboxed": false,
      "display_name": "Web Search",
      "formatter": "default"
    }
  ]
}
```

- [ ] **Step 3: Create shell script**

Create `plugins/web_search/web_search`:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Read JSON request from stdin
INPUT=$(cat)
QUERY=$(echo "$INPUT" | jq -r '.input.query // empty')
COUNT=$(echo "$INPUT" | jq -r '.input.count // 5')
API_KEY=$(echo "$INPUT" | jq -r '.input.api_key // empty')
BASE_URL=$(echo "$INPUT" | jq -r '.input.base_url // "https://api.search.brave.com/res/v1/web/search"')
DOMAIN_FILTER=$(echo "$INPUT" | jq -r '.input.domain_filter // empty')

# Validate required params
if [ -z "$QUERY" ]; then
  jq -n '{"content":"Error: query parameter is required","is_error":true}'
  exit 0
fi
if [ -z "$API_KEY" ]; then
  jq -n '{"content":"Error: api_key not configured. Add [tools.web_search] api_key = \"...\" to daemon.toml","is_error":true}'
  exit 0
fi

# Apply domain filter: prepend site: / -site: to query
if [ -n "$DOMAIN_FILTER" ]; then
  IFS=',' read -ra DOMAINS <<< "$DOMAIN_FILTER"
  for d in "${DOMAINS[@]}"; do
    d=$(echo "$d" | xargs)  # trim whitespace
    if [[ "$d" == -* ]]; then
      QUERY="$QUERY -site:${d#-}"
    else
      QUERY="$QUERY site:$d"
    fi
  done
fi

# Call Brave Search API
RESPONSE=$(curl -s -w "\n%{http_code}" \
  -H "X-Subscription-Token: $API_KEY" \
  -H "Accept: application/json" \
  --get --data-urlencode "q=$QUERY" --data-urlencode "count=$COUNT" \
  "$BASE_URL")

HTTP_CODE=$(echo "$RESPONSE" | tail -1)
BODY=$(echo "$RESPONSE" | sed '$d')

if [ "$HTTP_CODE" != "200" ]; then
  ERROR_MSG=$(echo "$BODY" | jq -r '.message // .error // "Unknown error"' 2>/dev/null || echo "HTTP $HTTP_CODE")
  jq -n --arg msg "$ERROR_MSG" '{"content": ("Search failed: " + $msg), "is_error": true}'
  exit 0
fi

# Format results as markdown
RESULT=$(echo "$BODY" | jq -r '
  .web.results // [] |
  to_entries |
  map("\(.key + 1). [\(.value.title)](\(.value.url))\n   \(.value.description // "No description")") |
  join("\n\n")
')

if [ -z "$RESULT" ]; then
  RESULT="No results found for: $QUERY"
fi

# Output JSON response (use jq to properly escape the content string)
jq -n --arg content "$RESULT" '{"content": $content, "is_error": false}'
```

- [ ] **Step 4: Make script executable**

```bash
chmod +x plugins/web_search/web_search
```

- [ ] **Step 5: Test script standalone (requires API key)**

```bash
echo '{"name":"web_search","input":{"query":"rust programming language","api_key":"YOUR_KEY","count":3}}' | ./plugins/web_search/web_search
```

Verify output is valid JSON with `content` and `is_error` fields.

Test error cases:
```bash
# Missing query
echo '{"name":"web_search","input":{}}' | ./plugins/web_search/web_search
# Missing API key
echo '{"name":"web_search","input":{"query":"test"}}' | ./plugins/web_search/web_search
```

- [ ] **Step 6: Commit**

```bash
git add plugins/web_search/
git commit -m "feat: add web_search plugin (Brave Search API, shell script)"
```

---

### Task 5: Update install.sh to copy plugins

**Files:**
- Modify: `install.sh` (add plugins/ copy step)

- [ ] **Step 1: Read current install.sh**

Read the file to understand the installation flow.

- [ ] **Step 2: Add plugin installation step**

After the binary copy section, add:

```bash
# Install plugins
if [ -d "$SOURCE_DIR/plugins" ]; then
  mkdir -p "$OMNISH_DIR/plugins"
  for plugin_dir in "$SOURCE_DIR/plugins"/*/; do
    plugin_name=$(basename "$plugin_dir")
    mkdir -p "$OMNISH_DIR/plugins/$plugin_name"
    cp -f "$plugin_dir"* "$OMNISH_DIR/plugins/$plugin_name/"
    chmod +x "$OMNISH_DIR/plugins/$plugin_name/$plugin_name" 2>/dev/null || true
  done
fi
```

- [ ] **Step 3: Commit**

```bash
git add install.sh
git commit -m "feat: install plugins directory during installation"
```

---

### Task 6: Final build and integration test

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: no errors

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace`
Expected: all tests pass

- [ ] **Step 3: Manual integration test**

1. Copy `plugins/web_search/` to `~/.omnish/plugins/web_search/`
2. Add to `~/.omnish/daemon.toml`:
   ```toml
   [plugins]
   enabled = ["web_search"]

   [tools.web_search]
   api_key = "YOUR_BRAVE_API_KEY"
   ```
3. Restart daemon: `systemctl --user restart omnish-daemon`
4. In omnish session, ask: `:search for latest rust release`
5. Verify search results appear formatted as markdown with Sources section

- [ ] **Step 4: Final commit if any fixes needed**

```bash
git add -A
git commit -m "fix: address integration test findings"
```
