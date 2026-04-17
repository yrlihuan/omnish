# Web Search Tool Design

Issue: #351 - Create web search tool using Brave Search API

## Overview

Add a web search tool as a daemon-side external plugin. The daemon spawns the plugin subprocess directly (not forwarded to client). The plugin is a shell script using `curl` and `jq` to call the Brave Search API. A generic parameter injection mechanism allows the daemon to merge configuration values into tool call inputs before execution.

## Components

### 1. Generic Parameter Injection

The daemon merges extra parameters into tool call inputs from two sources, with this precedence (later wins):

1. LLM's tool call input (what the model provides)
2. `tool.override.json` `params` field (plugin-level defaults)
3. `daemon.toml` `[tools.<name>]` section (deployment-level overrides)

Injected params are invisible to the LLM - they do not appear in tool definitions sent to the model, only in the execution input passed to the plugin subprocess.

#### daemon.toml

New top-level `[tools]` section with per-tool subsections:

```toml
[tools.web_search]
api_key = "BSAxxxxxxxx"
# base_url = "https://api.search.brave.com/res/v1/web/search"
```

Parsed as `HashMap<String, HashMap<String, serde_json::Value>>` in `DaemonConfig`. Uses `serde_json::Value` (not `toml::Value`) to avoid adding a `toml` dependency to `omnish-common`. The TOML deserializer handles the conversion automatically via serde.

#### tool.override.json

Existing `ToolOverrideEntry` gains an optional `params` field:

```json
{
  "tools": {
    "web_search": {
      "params": {
        "count": 5
      }
    }
  }
}
```

#### Merge Logic

In `server.rs` tool dispatch, before spawning the plugin subprocess or executing a daemon tool:

```
let mut merged_input = tc.input.clone();           // LLM's input
merge_json(&mut merged_input, override_params);     // tool.override.json params
merge_json(&mut merged_input, daemon_toml_params);  // daemon.toml [tools.X]
```

`merge_json` does a shallow merge: for each key in the source, set it on the target (overwriting if exists). This is intentionally shallow - no deep merge of nested objects.

### 2. Daemon-Side External Plugin Execution

Currently all external plugins are client-side. The `plugin_type` field in `tool.json` already supports `"daemon_tool"` vs `"client_tool"`. The PluginManager already parses this correctly. What's missing is the daemon-side execution path for external plugins.

Currently in `server.rs`, daemon-side tools are hardcoded:

```rust
if tc.name == "omnish_list_history" || tc.name == "omnish_get_output" {
    state.command_query_tool.execute(&tc.name, &tc.input)
} else {
    // Unknown daemon tool error
}
```

This needs to be extended: if a daemon-side tool is not a known built-in, spawn the plugin subprocess (same stdin/stdout JSON protocol as client-side plugins) and collect the result.

#### Subprocess Protocol

Same as existing client-side plugins:

- **stdin**: `{"name": "<tool_name>", "input": {<merged_params>}}`
- **stdout**: `{"content": "<result_text>", "is_error": false}`

The daemon resolves the executable path from the plugin directory: `~/.omnish/plugins/<plugin_name>/<plugin_name>`.

`PluginManager` exposes a new method `plugin_executable(&self, tool_name: &str) -> Option<PathBuf>` that returns `plugins_dir.join(dir_name).join(dir_name)` for the plugin that owns the given tool. The daemon calls this to locate the binary before spawning.

#### Sandboxing

Same sandbox rules apply. On Linux, Landlock. On macOS, sandbox-exec. The daemon applies sandboxing when spawning the subprocess, using the same `apply_sandbox` / `sandbox_profile` functions from `omnish-plugin`.

Note: the daemon currently links `omnish-plugin` for built-in tool definitions only. Sandbox functions are in `omnish-plugin::lib`. The daemon may need to call these, or the sandbox logic can be extracted. For simplicity, since the web_search plugin only needs network access (no filesystem writes), sandboxing can be deferred - the shell script runs with the daemon's permissions, which is acceptable for a user-installed plugin.

### 3. Web Search Plugin

#### Files

Source: `plugins/web_search/` in the project root.

Installed to: `~/.omnish/plugins/web_search/`

Contents:
- `web_search` - executable shell script
- `tool.json` - tool definition

#### tool.json

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

Note: `api_key` and `base_url` are NOT in `input_schema` - they are injected by the daemon from config.

#### Shell Script (`web_search`)

```bash
#!/usr/bin/env bash
set -euo pipefail

# Read JSON request from stdin
INPUT=$(cat)
NAME=$(echo "$INPUT" | jq -r '.name')
QUERY=$(echo "$INPUT" | jq -r '.input.query // empty')
COUNT=$(echo "$INPUT" | jq -r '.input.count // 5')
API_KEY=$(echo "$INPUT" | jq -r '.input.api_key // empty')
BASE_URL=$(echo "$INPUT" | jq -r '.input.base_url // "https://api.search.brave.com/res/v1/web/search"')
DOMAIN_FILTER=$(echo "$INPUT" | jq -r '.input.domain_filter // empty')

# Validate required params
if [ -z "$QUERY" ]; then
  echo '{"content":"Error: query parameter is required","is_error":true}'
  exit 0
fi
if [ -z "$API_KEY" ]; then
  echo '{"content":"Error: api_key not configured. Add [tools.web_search] api_key = \"...\" to daemon.toml","is_error":true}'
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

### 4. Configuration

#### daemon.toml additions

```toml
[plugins]
enabled = ["web_search"]

[tools.web_search]
api_key = "BSAxxxxxxxx"
# base_url = "https://api.search.brave.com/res/v1/web/search"  # default
```

#### DaemonConfig changes

Add to `DaemonConfig`:

```rust
#[serde(default)]
pub tools: HashMap<String, HashMap<String, serde_json::Value>>,
```

Each key is a tool name, each value is a flat map of param name to value. Serde deserializes TOML tables directly into `serde_json::Value` since both implement the serde data model.

#### ToolOverrideEntry changes

Add optional `params` field:

```rust
struct ToolOverrideEntry {
    description: Option<DescriptionValue>,
    append: Option<DescriptionValue>,
    params: Option<HashMap<String, serde_json::Value>>,  // NEW
}
```

The `PluginManager` stores these params in the prompt cache alongside descriptions, and exposes a method `tool_override_params(&self, tool_name: &str) -> Option<HashMap<String, serde_json::Value>>`.

## Data Flow

```
LLM generates tool_call: {name: "web_search", input: {query: "rust async"}}
  │
  ▼
Daemon tool dispatch (server.rs)
  │
  ├─ Merge params from tool.override.json (e.g., count: 5)
  ├─ Merge params from daemon.toml [tools.web_search] (e.g., api_key, base_url)
  │
  ├─ plugin_type == DaemonTool && not a built-in daemon tool
  │
  ▼
Spawn subprocess: ~/.omnish/plugins/web_search/web_search
  │
  ├─ stdin: {"name": "web_search", "input": {query, count, api_key, base_url}}
  ├─ stdout: {"content": "1. [Title](url)\n   snippet\n...", "is_error": false}
  │
  ▼
Daemon collects ToolResult, formats, feeds back to LLM
```

## Error Handling

- Missing `api_key`: plugin returns `is_error: true` with config instructions
- HTTP error: plugin returns `is_error: true` with status code and message
- No results: plugin returns `is_error: false` with "No results found" message
- `jq` or `curl` not found: `set -e` causes immediate exit with non-zero status. No structured JSON reaches the daemon. The daemon must handle non-zero exit by capturing stderr and producing a ToolResult with `is_error: true` and the stderr content.
- Plugin subprocess timeout: daemon wraps subprocess I/O with `tokio::time::timeout(Duration::from_secs(30), ...)`. On timeout, kill the process and return `is_error: true`.

## Installation

The `install.sh` script copies `plugins/web_search/` to `~/.omnish/plugins/web_search/`. The plugin is inactive until `[plugins] enabled = ["web_search"]` is set in `daemon.toml` with a valid `api_key`.

## Testing

- Unit test: `tool.override.json` `params` field parsing and merge logic
- Unit test: `daemon.toml` `[tools.X]` parsing
- Unit test: merge precedence (LLM input < override params < daemon.toml params)
- Integration: manually test with Brave Search API key
- The shell script can be tested standalone: `echo '{"name":"web_search","input":{"query":"test","api_key":"..."}}' | ./web_search`
