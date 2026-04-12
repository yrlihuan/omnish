//! Shared sandbox permit-rule utilities used by both daemon and client.

pub const OPERATORS: &[&str] = &["starts_with", "contains", "equals", "matches"];

/// Parse a rule string like `"command starts_with glab"` into `(field, operator, value)`.
/// `operator` defaults to `"starts_with"` when the rule string has fewer than 3 parts.
pub fn parse_rule_parts(rule: &str) -> (String, String, String) {
    let mut parts = rule.splitn(3, ' ');
    let field    = parts.next().unwrap_or("").to_string();
    let operator = parts.next().unwrap_or("starts_with").to_string();
    let value    = parts.next().unwrap_or("").to_string();
    (field, operator, value)
}

/// Evaluate a single raw rule string against a tool input JSON object.
fn evaluate_raw(rule: &str, input: &serde_json::Value) -> bool {
    let (field, operator, value) = parse_rule_parts(rule);
    if field.is_empty() || value.is_empty() {
        return false;
    }
    let field_value = match input.get(&field).and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return false,
    };
    match operator.as_str() {
        "starts_with" => field_value.starts_with(&value),
        "contains" => field_value.contains(&value),
        "equals" => field_value == value,
        "matches" => regex_lite::Regex::new(&value)
            .map(|re| re.is_match(field_value))
            .unwrap_or(false),
        _ => false,
    }
}

/// Check if any raw rule string matches the given tool input (OR logic).
/// Returns the first matched rule string, or `None`.
pub fn check_bypass_raw<'a>(rules: &'a [String], input: &serde_json::Value) -> Option<&'a str> {
    rules.iter().find(|r| evaluate_raw(r, input)).map(|s| s.as_str())
}
