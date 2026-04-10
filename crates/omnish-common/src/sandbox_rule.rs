/// Shared sandbox permit-rule utilities used by both daemon and client.

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
