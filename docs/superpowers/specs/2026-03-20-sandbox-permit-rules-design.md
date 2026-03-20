# Sandbox Permit Rules Design (Issue #379)

## Problem

Landlock LSM requires `PR_SET_NO_NEW_PRIVS`, which prevents setuid binaries (like snap's `snap-confine`) from gaining elevated capabilities. Any snap-installed tool (e.g., `glab`) called from the sandboxed bash tool fails with:

```
snap-confine is packaged without necessary permissions and cannot continue
required permitted capability cap_dac_override not found in current capabilities
```

This is a kernel-level constraint — Landlock and setuid are fundamentally incompatible.

## Solution

Add configurable permit rules to `daemon.toml` that allow specific tool invocations to bypass the Landlock sandbox. Rules are evaluated per-tool against the tool's raw input parameters.

## Config Format

```toml
[sandbox.plugins.bash]
permit_rules = [
  'command starts_with glab',
  'command starts_with docker',
  'command matches ^snap\s+run\s+',
  'cwd equals /home/huan/docker',
]
```

- Section key `sandbox.plugins.<tool_name>` scopes rules to a specific tool
- `permit_rules` is a list of rule strings
- Use single-quoted TOML strings for regex patterns to avoid double-escaping backslashes

## Rule Syntax

```
<param_field> <operator> <value>
```

- **param_field**: a key in the tool's input JSON (for bash: `command`, `cwd`, `shell`, `timeout`)
- **operator**: one of `starts_with`, `contains`, `equals`, `matches`
- **value**: the rest of the string after the operator (leading/trailing whitespace trimmed)

### Operators

| Operator | Meaning |
|---|---|
| `starts_with` | param value starts with the given string |
| `contains` | param value contains the given substring |
| `equals` | param value exactly equals the given string |
| `matches` | param value matches the given regex pattern |

### Evaluation Rules

- Multiple `permit_rules` use **OR** logic — any match → bypass sandbox
- All matching is **case-sensitive**
- If the referenced param field does not exist in the tool input, the rule evaluates to **false**
- Regex patterns use the `regex` crate syntax (linear-time matching, no catastrophic backtracking)
- Invalid regex patterns are logged as errors at config load time and skipped
- Rules are evaluated against the **raw LLM-generated input** (`tc.input`), not the merged input (which includes override/config params)

## Decision Flow

1. LLM generates a tool call (e.g., bash with `command: "glab issue view 379"`)
2. Daemon looks up pre-compiled rules for tool name `"bash"`
3. Evaluates each `PermitRule` against the tool's raw input JSON
4. If any rule matches:
   - Sets `sandboxed: false` on `ChatToolCall`
   - Logs `tracing::warn!("sandbox bypass: tool={}, rule={}, input=...", ...)`
5. If no rules match or no rules configured: `sandboxed: true` (default)
6. Client receives `ChatToolCall`:
   - If `sandboxed: false`: skips Landlock `pre_exec`, emits event to event_log
   - If `sandboxed: true`: applies Landlock as usual

## Components

### Config (omnish-common)

```rust
// config.rs
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SandboxConfig {
    /// Per-plugin permit rules: key is tool_name
    #[serde(default)]
    pub plugins: HashMap<String, SandboxPluginConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SandboxPluginConfig {
    #[serde(default)]
    pub permit_rules: Vec<String>,
}
```

Add `sandbox` field to `DaemonConfig`:
```rust
pub struct DaemonConfig {
    // ...existing fields...
    #[serde(default)]
    pub sandbox: SandboxConfig,
}
```

### Rule Engine (omnish-daemon)

New file `sandbox_rules.rs`:

```rust
pub struct PermitRule {
    pub field: String,
    pub operator: RuleOperator,
    pub value: String,
    pub compiled_regex: Option<Regex>,  // pre-compiled for `matches` operator
}

pub enum RuleOperator {
    StartsWith,
    Contains,
    Equals,
    Matches,
}

impl PermitRule {
    /// Parse a rule string like "command starts_with glab"
    pub fn parse(rule: &str) -> Result<Self, String>;

    /// Evaluate the rule against a tool input JSON
    pub fn evaluate(&self, input: &serde_json::Value) -> bool;
}

/// Returns true if any permit rule matches (OR logic)
pub fn should_bypass_sandbox(rules: &[PermitRule], input: &serde_json::Value) -> bool;
```

### Startup: Pre-compile Rules

At `DaemonServer::new()`, parse all rule strings into `PermitRule` structs and store as a field:

```rust
// DaemonServer
pub struct DaemonServer {
    // ...existing fields...
    sandbox_rules: HashMap<String, Vec<PermitRule>>,
}

// In DaemonServer::new():
let sandbox_rules = sandbox_rules::compile_config(&config.sandbox);
// Logs errors for invalid rules (bad regex, unknown operator, etc.)
```

This replaces `PluginManager::tool_sandboxed()`, which is removed. The `sandboxed` field in `ToolJsonEntry` remains as `#[allow(dead_code)]` for backwards compatibility with existing tool.json files.

### Decision Point (omnish-daemon/server.rs)

Replace the current hardcoded `sandboxed: true`:

```rust
// Before:
sandboxed: plugin_mgr.tool_sandboxed(&tc.name).unwrap_or(true),

// After:
sandboxed: !sandbox_rules::should_bypass_sandbox(
    self.sandbox_rules.get(&tc.name).map(|v| v.as_slice()).unwrap_or(&[]),
    &tc.input,  // raw LLM input, not merged
),
```

### Client Event (omnish-client/chat_session.rs)

When dispatching a tool with `sandboxed: false`, emit an event:

```rust
if !tool_call.sandboxed {
    event_log::push(&format!("tool '{}' running without sandbox", tool_call.tool_name));
}
```

## Files Changed

| File | Change |
|---|---|
| `crates/omnish-common/src/config.rs` | Add `SandboxConfig`, `SandboxPluginConfig` structs; add `sandbox` to `DaemonConfig` |
| `crates/omnish-daemon/src/sandbox_rules.rs` | New: `PermitRule`, `compile_config()`, `should_bypass_sandbox()` |
| `crates/omnish-daemon/src/server.rs` | Add `sandbox_rules` field to `DaemonServer`; use `should_bypass_sandbox` at decision point |
| `crates/omnish-daemon/src/plugin.rs` | Remove `tool_sandboxed()` method |
| `crates/omnish-client/src/chat_session.rs` | Emit event_log entry when tool runs unsandboxed |
| `crates/omnish-daemon/Cargo.toml` | Add `regex = "1"` dependency |

## Tests

Unit tests in `sandbox_rules.rs`:
- Parse valid rules (all 4 operators)
- Parse invalid rules (unknown operator, missing field, empty string)
- Evaluate each operator (match and no-match cases)
- Missing field in input → false
- OR logic: multiple rules, first fails, second matches → true
- Invalid regex logged and skipped (does not panic)
- Config deserialization: empty sandbox section, missing section, full config

## Security Notes

- Default is sandboxed; bypass requires explicit daemon.toml config
- LLM cannot modify daemon.toml — config is controlled by the system operator
- `contains` rule like `command contains glab` would match `echo glab` — this is acceptable as the user explicitly configured the rule
- The `regex` crate guarantees linear-time matching, preventing regex DoS

## Not In Scope

- Client-side sandbox override
- Deny rules or per-rule AND logic
- Network-level sandbox control
- Hot-reload of sandbox config (requires daemon restart)
