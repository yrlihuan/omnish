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

    /// Evaluate the rule against a tool input JSON object.
    /// Returns false if the field doesn't exist or the value doesn't match.
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
}

/// Pre-compile all permit rules from config at startup.
/// Returns a map of tool_name → compiled rules.
/// Logs errors for invalid rules (bad regex, unknown operator) and skips them.
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

/// Check if any permit rule matches the given input (OR logic).
/// Returns the matched rule's raw string for logging, or None if no match.
pub fn check_bypass<'a>(rules: &'a [PermitRule], input: &serde_json::Value) -> Option<&'a str> {
    rules.iter().find(|rule| rule.evaluate(input)).map(|r| r.raw.as_str())
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
