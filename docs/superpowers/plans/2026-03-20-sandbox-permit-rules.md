# Sandbox Permit Rules Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow specific tool invocations to bypass the Landlock sandbox via configurable permit rules in daemon.toml, fixing snap binary incompatibility (Issue #379).

**Architecture:** New `SandboxConfig` in omnish-common for TOML deserialization. New `sandbox_rules` module in omnish-daemon for rule parsing/evaluation. Rules are pre-compiled at daemon startup and stored on `DaemonServer`. The existing `tool_sandboxed()` method is removed. Client emits event_log when a tool runs unsandboxed.

**Tech Stack:** Rust, `regex` crate, `serde`/`toml` for config, `serde_json` for input matching.

**Spec:** `docs/superpowers/specs/2026-03-20-sandbox-permit-rules-design.md`

---

### Task 1: Add SandboxConfig to omnish-common

**Files:**
- Modify: `crates/omnish-common/src/config.rs:206-247`

- [ ] **Step 1: Add SandboxConfig and SandboxPluginConfig structs**

Add after the `PluginsConfig` struct (line 212) and before the `DaemonConfig` struct (line 218):

```rust
// ---------------------------------------------------------------------------
// Sandbox config
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Clone, Default)]
pub struct SandboxConfig {
    /// Per-tool permit rules. Key is tool_name (e.g. "bash").
    /// When any rule matches, the tool runs without Landlock sandbox.
    #[serde(default)]
    pub plugins: HashMap<String, SandboxPluginConfig>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct SandboxPluginConfig {
    /// Rules in format: "<param_field> <operator> <value>"
    /// Operators: starts_with, contains, equals, matches (regex)
    #[serde(default)]
    pub permit_rules: Vec<String>,
}
```

- [ ] **Step 2: Add `sandbox` field to DaemonConfig**

Add to the `DaemonConfig` struct:

```rust
#[serde(default)]
pub sandbox: SandboxConfig,
```

And add to `Default for DaemonConfig`:

```rust
sandbox: SandboxConfig::default(),
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p omnish-common`
Expected: success (all new fields have `#[serde(default)]`)

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-common/src/config.rs
git commit -m "feat(config): add SandboxConfig for permit rules (#379)"
```

---

### Task 2: Implement sandbox_rules module with TDD

**Files:**
- Create: `crates/omnish-daemon/src/sandbox_rules.rs`
- Modify: `crates/omnish-daemon/src/main.rs:1` (add `mod sandbox_rules;` in server.rs scope)
- Modify: `crates/omnish-daemon/Cargo.toml` (add `regex` dependency)

- [ ] **Step 1: Add `regex` dependency**

Add to `[dependencies]` in `crates/omnish-daemon/Cargo.toml`:

```toml
regex = "1"
```

- [ ] **Step 2: Create sandbox_rules.rs with struct definitions and failing tests**

Create `crates/omnish-daemon/src/sandbox_rules.rs`:

```rust
use omnish_common::config::SandboxConfig;
use regex::Regex;
use std::collections::HashMap;

#[derive(Debug)]
pub enum RuleOperator {
    StartsWith,
    Contains,
    Equals,
    Matches,
}

#[derive(Debug)]
pub struct PermitRule {
    pub field: String,
    pub operator: RuleOperator,
    pub value: String,
    compiled_regex: Option<Regex>,
    /// Original rule string for logging
    pub raw: String,
}

impl PermitRule {
    /// Parse a rule string like "command starts_with glab".
    /// Format: <param_field> <operator> <value>
    pub fn parse(rule: &str) -> Result<Self, String> {
        todo!()
    }

    /// Evaluate the rule against a tool input JSON object.
    /// Returns false if the field doesn't exist or the value doesn't match.
    pub fn evaluate(&self, input: &serde_json::Value) -> bool {
        todo!()
    }
}

/// Pre-compile all permit rules from config at startup.
/// Returns a map of tool_name → compiled rules.
/// Logs errors for invalid rules (bad regex, unknown operator) and skips them.
pub fn compile_config(config: &SandboxConfig) -> HashMap<String, Vec<PermitRule>> {
    todo!()
}

/// Check if any permit rule matches the given input (OR logic).
/// Returns the matched rule's raw string for logging, or None if no match.
pub fn check_bypass(rules: &[PermitRule], input: &serde_json::Value) -> Option<&str> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse tests ---

    #[test]
    fn test_parse_starts_with() {
        let rule = PermitRule::parse("command starts_with glab").unwrap();
        assert_eq!(rule.field, "command");
        assert!(matches!(rule.operator, RuleOperator::StartsWith));
        assert_eq!(rule.value, "glab");
    }

    #[test]
    fn test_parse_contains() {
        let rule = PermitRule::parse("command contains docker").unwrap();
        assert_eq!(rule.field, "command");
        assert!(matches!(rule.operator, RuleOperator::Contains));
        assert_eq!(rule.value, "docker");
    }

    #[test]
    fn test_parse_equals() {
        let rule = PermitRule::parse("cwd equals /home/user/docker").unwrap();
        assert_eq!(rule.field, "cwd");
        assert!(matches!(rule.operator, RuleOperator::Equals));
        assert_eq!(rule.value, "/home/user/docker");
    }

    #[test]
    fn test_parse_matches() {
        let rule = PermitRule::parse(r"command matches ^snap\s+run").unwrap();
        assert_eq!(rule.field, "command");
        assert!(matches!(rule.operator, RuleOperator::Matches));
        assert!(rule.compiled_regex.is_some());
    }

    #[test]
    fn test_parse_value_with_spaces() {
        let rule = PermitRule::parse("command starts_with snap run").unwrap();
        assert_eq!(rule.value, "snap run");
    }

    #[test]
    fn test_parse_unknown_operator() {
        assert!(PermitRule::parse("command foobar glab").is_err());
    }

    #[test]
    fn test_parse_missing_value() {
        assert!(PermitRule::parse("command starts_with").is_err());
    }

    #[test]
    fn test_parse_empty_string() {
        assert!(PermitRule::parse("").is_err());
    }

    #[test]
    fn test_parse_single_token() {
        assert!(PermitRule::parse("command").is_err());
    }

    #[test]
    fn test_parse_invalid_regex() {
        assert!(PermitRule::parse("command matches [invalid").is_err());
    }

    // --- evaluate tests ---

    #[test]
    fn test_eval_starts_with_match() {
        let rule = PermitRule::parse("command starts_with glab").unwrap();
        let input = serde_json::json!({"command": "glab issue view 379"});
        assert!(rule.evaluate(&input));
    }

    #[test]
    fn test_eval_starts_with_no_match() {
        let rule = PermitRule::parse("command starts_with glab").unwrap();
        let input = serde_json::json!({"command": "git status"});
        assert!(!rule.evaluate(&input));
    }

    #[test]
    fn test_eval_contains_match() {
        let rule = PermitRule::parse("command contains docker").unwrap();
        let input = serde_json::json!({"command": "sudo docker ps"});
        assert!(rule.evaluate(&input));
    }

    #[test]
    fn test_eval_contains_no_match() {
        let rule = PermitRule::parse("command contains docker").unwrap();
        let input = serde_json::json!({"command": "ls -la"});
        assert!(!rule.evaluate(&input));
    }

    #[test]
    fn test_eval_equals_match() {
        let rule = PermitRule::parse("cwd equals /home/user").unwrap();
        let input = serde_json::json!({"cwd": "/home/user"});
        assert!(rule.evaluate(&input));
    }

    #[test]
    fn test_eval_equals_no_match() {
        let rule = PermitRule::parse("cwd equals /home/user").unwrap();
        let input = serde_json::json!({"cwd": "/home/user/project"});
        assert!(!rule.evaluate(&input));
    }

    #[test]
    fn test_eval_matches_match() {
        let rule = PermitRule::parse(r"command matches ^glab\s+").unwrap();
        let input = serde_json::json!({"command": "glab issue view"});
        assert!(rule.evaluate(&input));
    }

    #[test]
    fn test_eval_matches_no_match() {
        let rule = PermitRule::parse(r"command matches ^glab\s+").unwrap();
        let input = serde_json::json!({"command": "git status"});
        assert!(!rule.evaluate(&input));
    }

    #[test]
    fn test_eval_missing_field() {
        let rule = PermitRule::parse("cwd equals /tmp").unwrap();
        let input = serde_json::json!({"command": "ls"});
        assert!(!rule.evaluate(&input));
    }

    #[test]
    fn test_eval_non_string_field() {
        let rule = PermitRule::parse("timeout equals 120").unwrap();
        let input = serde_json::json!({"timeout": 120});
        assert!(!rule.evaluate(&input)); // only string values are matched
    }

    #[test]
    fn test_eval_case_sensitive() {
        let rule = PermitRule::parse("command starts_with Glab").unwrap();
        let input = serde_json::json!({"command": "glab issue view"});
        assert!(!rule.evaluate(&input));
    }

    // --- check_bypass tests ---

    #[test]
    fn test_bypass_empty_rules() {
        let input = serde_json::json!({"command": "glab issue view"});
        assert!(check_bypass(&[], &input).is_none());
    }

    #[test]
    fn test_bypass_or_logic_first_matches() {
        let rules = vec![
            PermitRule::parse("command starts_with glab").unwrap(),
            PermitRule::parse("command starts_with docker").unwrap(),
        ];
        let input = serde_json::json!({"command": "glab issue view"});
        assert_eq!(check_bypass(&rules, &input), Some("command starts_with glab"));
    }

    #[test]
    fn test_bypass_or_logic_second_matches() {
        let rules = vec![
            PermitRule::parse("command starts_with glab").unwrap(),
            PermitRule::parse("command starts_with docker").unwrap(),
        ];
        let input = serde_json::json!({"command": "docker ps"});
        assert_eq!(check_bypass(&rules, &input), Some("command starts_with docker"));
    }

    #[test]
    fn test_bypass_or_logic_none_match() {
        let rules = vec![
            PermitRule::parse("command starts_with glab").unwrap(),
            PermitRule::parse("command starts_with docker").unwrap(),
        ];
        let input = serde_json::json!({"command": "ls -la"});
        assert!(check_bypass(&rules, &input).is_none());
    }

    // --- compile_config tests ---

    #[test]
    fn test_compile_empty_config() {
        let config = SandboxConfig::default();
        let rules = compile_config(&config);
        assert!(rules.is_empty());
    }

    #[test]
    fn test_compile_valid_rules() {
        let mut config = SandboxConfig::default();
        config.plugins.insert("bash".to_string(), omnish_common::config::SandboxPluginConfig {
            permit_rules: vec![
                "command starts_with glab".to_string(),
                "command contains docker".to_string(),
            ],
        });
        let rules = compile_config(&config);
        assert_eq!(rules.get("bash").unwrap().len(), 2);
    }

    #[test]
    fn test_toml_deserialization() {
        let toml_str = r#"
[sandbox.plugins.bash]
permit_rules = [
  'command starts_with glab',
  'command contains docker',
]
"#;
        let config: omnish_common::config::DaemonConfig = toml::from_str(toml_str).unwrap();
        let bash_rules = &config.sandbox.plugins["bash"];
        assert_eq!(bash_rules.permit_rules.len(), 2);
    }

    #[test]
    fn test_toml_empty_sandbox() {
        let toml_str = "";
        let config: omnish_common::config::DaemonConfig = toml::from_str(toml_str).unwrap();
        assert!(config.sandbox.plugins.is_empty());
    }

    #[test]
    fn test_compile_skips_invalid_rules() {
        let mut config = SandboxConfig::default();
        config.plugins.insert("bash".to_string(), omnish_common::config::SandboxPluginConfig {
            permit_rules: vec![
                "command starts_with glab".to_string(),
                "command foobar invalid".to_string(), // invalid operator
                "command matches [bad".to_string(),   // invalid regex
            ],
        });
        let rules = compile_config(&config);
        // Only the valid rule should survive
        assert_eq!(rules.get("bash").unwrap().len(), 1);
    }
}
```

- [ ] **Step 3: Add mod declaration in main.rs**

In `crates/omnish-daemon/src/main.rs`, add `mod sandbox_rules;` before the existing `mod server;` (line 1):

```rust
mod sandbox_rules;
mod server;
```

`server.rs` will reference it as `crate::sandbox_rules`.

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test -p omnish-daemon sandbox_rules`
Expected: FAIL (all functions are `todo!()`)

- [ ] **Step 5: Implement PermitRule::parse**

Replace the `todo!()` in `parse`:

```rust
pub fn parse(rule: &str) -> Result<Self, String> {
    let rule = rule.trim();
    if rule.is_empty() {
        return Err("empty rule".into());
    }

    // Split into: field, operator, value (value is everything after operator)
    let mut parts = rule.splitn(3, ' ');
    let field = parts.next().ok_or("missing field")?.to_string();
    let op_str = parts.next().ok_or("missing operator")?;
    let value = parts.next().ok_or("missing value")?.to_string();

    if value.is_empty() {
        return Err("empty value".into());
    }

    let (operator, compiled_regex) = match op_str {
        "starts_with" => (RuleOperator::StartsWith, None),
        "contains" => (RuleOperator::Contains, None),
        "equals" => (RuleOperator::Equals, None),
        "matches" => {
            let re = Regex::new(&value).map_err(|e| format!("invalid regex: {e}"))?;
            (RuleOperator::Matches, Some(re))
        }
        other => return Err(format!("unknown operator: {other}")),
    };

    Ok(Self {
        field,
        operator,
        value,
        compiled_regex,
        raw: rule.to_string(),
    })
}
```

- [ ] **Step 6: Implement PermitRule::evaluate**

Replace the `todo!()` in `evaluate`:

```rust
pub fn evaluate(&self, input: &serde_json::Value) -> bool {
    let field_value = match input.get(&self.field).and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return false, // field missing or not a string
    };

    match self.operator {
        RuleOperator::StartsWith => field_value.starts_with(&self.value),
        RuleOperator::Contains => field_value.contains(&self.value),
        RuleOperator::Equals => field_value == self.value,
        RuleOperator::Matches => self
            .compiled_regex
            .as_ref()
            .map(|re| re.is_match(field_value))
            .unwrap_or(false),
    }
}
```

- [ ] **Step 7: Implement compile_config and check_bypass**

Replace the `todo!()` stubs:

```rust
pub fn compile_config(config: &SandboxConfig) -> HashMap<String, Vec<PermitRule>> {
    let mut result = HashMap::new();
    for (tool_name, plugin_config) in &config.plugins {
        let mut rules = Vec::new();
        for rule_str in &plugin_config.permit_rules {
            match PermitRule::parse(rule_str) {
                Ok(rule) => rules.push(rule),
                Err(e) => {
                    tracing::error!(
                        "sandbox permit rule for '{}' is invalid: '{}' - {}",
                        tool_name, rule_str, e
                    );
                }
            }
        }
        if !rules.is_empty() {
            result.insert(tool_name.clone(), rules);
        }
    }
    result
}

pub fn check_bypass<'a>(rules: &'a [PermitRule], input: &serde_json::Value) -> Option<&'a str> {
    rules.iter().find(|rule| rule.evaluate(input)).map(|r| r.raw.as_str())
}
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test -p omnish-daemon sandbox_rules`
Expected: all tests PASS

- [ ] **Step 9: Commit**

```bash
git add crates/omnish-daemon/src/sandbox_rules.rs crates/omnish-daemon/src/main.rs crates/omnish-daemon/Cargo.toml
git commit -m "feat(daemon): add sandbox_rules module with permit rule engine (#379)"
```

---

### Task 3: Wire sandbox_rules into DaemonServer

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs:111-121,220-241,1121`
- Modify: `crates/omnish-daemon/src/main.rs:267`
- Modify: `crates/omnish-daemon/src/plugin.rs:342-347` (remove `tool_sandboxed`)

- [ ] **Step 1: Add type alias and `sandbox_rules` field to DaemonServer**

In `server.rs`, add a type alias near the top and the field to the struct:

```rust
type SandboxRules = Arc<HashMap<String, Vec<crate::sandbox_rules::PermitRule>>>;

pub struct DaemonServer {
    // ...existing fields...
    sandbox_rules: SandboxRules,
}
```

- [ ] **Step 2: Update DaemonServer::new() to accept and wrap sandbox_rules**

Add parameter to `new()` - accepts the raw HashMap, wraps in Arc:

```rust
pub fn new(
    // ...existing params...
    tool_params: HashMap<String, HashMap<String, serde_json::Value>>,
    sandbox_rules: HashMap<String, Vec<crate::sandbox_rules::PermitRule>>,
) -> Self {
    Self {
        // ...existing fields...
        tool_params,
        sandbox_rules: Arc::new(sandbox_rules),
    }
}
```

- [ ] **Step 3: Thread sandbox_rules through run() → handle_message() → run_agent_loop()**

Follow the exact same pattern as `tool_params` (which is already threaded through all these functions). The type alias used throughout:

```rust
type SandboxRules = Arc<HashMap<String, Vec<crate::sandbox_rules::PermitRule>>>;
```

Store as `Arc` on `DaemonServer` (field type: `SandboxRules`), wrap in `Arc::new()` in `new()`.

Add a `sandbox_rules: &SandboxRules` parameter to each function in the call chain, mirroring `tool_params`:

1. **`DaemonServer::run()`** (line 260): `let sandbox_rules = self.sandbox_rules.clone();`
2. **Closure in `run()`** (line 321): `let sandbox_rules = sandbox_rules.clone();`
3. **`Box::pin(async move { handle_message(..., &sandbox_rules, ...) })`** (line 324)
4. **`handle_message()`** signature (line 334): add `sandbox_rules: &SandboxRules`
5. **`handle_message()` body**: pass `&sandbox_rules` to `handle_chat_message()` (line 702) and `handle_tool_result()` (line 706)
6. **`handle_chat_message()`** signature (line 813): add `sandbox_rules: &SandboxRules`, pass to `run_agent_loop()` (line 894)
7. **`handle_tool_result()`** signature (line 898): add `sandbox_rules: &SandboxRules`, pass to `run_agent_loop()` (line 1010)
8. **`run_agent_loop()`** signature (line 1017): add `sandbox_rules: &SandboxRules` - this is where the decision point lives (line 1121)

- [ ] **Step 4: Replace tool_sandboxed() call with check_bypass()**

At line 1121 in `server.rs`, change:

```rust
// Before:
sandboxed: plugin_mgr.tool_sandboxed(&tc.name).unwrap_or(true),

// After: use check_bypass which returns the matched rule for logging
let matched_rule = crate::sandbox_rules::check_bypass(
    sandbox_rules.get(&tc.name).map(|v| v.as_slice()).unwrap_or(&[]),
    &tc.input,
);
if let Some(rule) = matched_rule {
    tracing::warn!(
        "sandbox bypass: tool={}, rule='{}', input={}",
        tc.name, rule,
        serde_json::to_string(&tc.input).unwrap_or_default(),
    );
}
// ... then in the ChatToolCall construction:
sandboxed: matched_rule.is_none(),
```

- [ ] **Step 5: Update main.rs to compile config and pass to DaemonServer**

In `main.rs` at line 267, change:

```rust
// Before:
let server = DaemonServer::new(session_mgr, llm_backend, task_mgr, conv_mgr, plugin_mgr, chat_model_name, config.tools);

// After:
let sandbox_rules = crate::sandbox_rules::compile_config(&config.sandbox);
let server = DaemonServer::new(session_mgr, llm_backend, task_mgr, conv_mgr, plugin_mgr, chat_model_name, config.tools, sandbox_rules);
```

- [ ] **Step 6: Remove tool_sandboxed() from PluginManager**

In `crates/omnish-daemon/src/plugin.rs`, remove lines 342-347:

```rust
// DELETE:
/// Return whether the tool should be sandboxed. Always true - plugins cannot opt out.
pub fn tool_sandboxed(&self, tool_name: &str) -> Option<bool> {
    self.tool_index
        .get(tool_name)
        .map(|_| true)
}
```

Also remove the `test_tool_sandboxed` test (lines 768-776).

- [ ] **Step 7: Verify it compiles and tests pass**

Run: `cargo build -p omnish-daemon && cargo test -p omnish-daemon`
Expected: success

- [ ] **Step 8: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/main.rs crates/omnish-daemon/src/plugin.rs
git commit -m "feat(daemon): wire sandbox permit rules into tool dispatch (#379)"
```

---

### Task 4: Add client-side event_log entry

**Files:**
- Modify: `crates/omnish-client/src/chat_session.rs:641-656`

- [ ] **Step 1: Add event_log entry when tool runs unsandboxed**

In `chat_session.rs`, around line 644 where `sandboxed` is extracted from `tc`, add:

```rust
if !sandboxed {
    crate::event_log::push(format!(
        "tool '{}' running without sandbox (permit rule match)",
        tc.tool_name,
    ));
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p omnish-client`
Expected: success

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-client/src/chat_session.rs
git commit -m "feat(client): emit event_log when tool runs without sandbox (#379)"
```

---

### Task 5: End-to-end verification

- [ ] **Step 1: Full build**

Run: `cargo build`
Expected: success

- [ ] **Step 2: Full test suite**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 3: Verify config deserialization**

Ensure an empty `daemon.toml` (no `[sandbox]` section) still works - `SandboxConfig::default()` produces empty rules, all tools remain sandboxed. This is covered by existing daemon startup tests, but verify manually:

Run: `cargo test -p omnish-daemon` - should pass without any sandbox config.

- [ ] **Step 4: Commit if any fixes were needed**

Only if step 1-3 required changes.
