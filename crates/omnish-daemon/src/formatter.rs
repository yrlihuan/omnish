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

fn interpolate_template(template: &str, params: &serde_json::Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = params.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{}}}", key);
            let replacement = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }
    }
    result.replace('\n', "\\n").replace('\r', "\\r")
}

fn status_icon(output: &Option<String>, is_error: &Option<bool>) -> StatusIcon {
    match output {
        None => StatusIcon::Running,
        Some(_) => match is_error {
            Some(true) => StatusIcon::Error,
            _ => StatusIcon::Success,
        },
    }
}

fn head_lines(text: &str, n: usize) -> Vec<String> {
    text.lines().take(n).map(|l| l.to_string()).collect()
}

fn all_lines(text: &str) -> Vec<String> {
    text.lines().map(|l| l.to_string()).collect()
}

// --- DefaultFormatter ---

struct DefaultFormatter;

impl ToolFormatter for DefaultFormatter {
    fn format(&self, input: &FormatInput) -> FormatOutput {
        let param_desc = interpolate_template(&input.status_template, &input.params);
        let icon = status_icon(&input.output, &input.is_error);
        let (result_compact, result_full) = match &input.output {
            None => (vec![], vec![]),
            Some(text) => (head_lines(text, 5), all_lines(text)),
        };
        FormatOutput {
            status_icon: icon,
            param_desc,
            result_compact,
            result_full,
        }
    }
}

// --- ReadFormatter ---

struct ReadFormatter;

impl ToolFormatter for ReadFormatter {
    fn format(&self, input: &FormatInput) -> FormatOutput {
        let param_desc = input
            .params
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let icon = status_icon(&input.output, &input.is_error);
        let (result_compact, result_full) = match &input.output {
            None => (vec![], vec![]),
            Some(text) => {
                if input.is_error == Some(true) {
                    let full = all_lines(text);
                    let compact = head_lines(text, 5);
                    (compact, full)
                } else {
                    // Count content lines (exclude metadata like "N more lines after...")
                    let content_lines: Vec<&str> = text
                        .lines()
                        .filter(|l| l.contains('\u{2192}')) // arrow separator from read tool
                        .collect();
                    let n = content_lines.len();
                    let compact = vec![format!("Read {} lines", n)];
                    let full = if n <= 10 {
                        // Show numbered lines as "lineno\tcontent"
                        content_lines
                            .iter()
                            .map(|l| {
                                // Parse "  lineno→content" into "lineno\tcontent"
                                if let Some((num, content)) = l.split_once('\u{2192}') {
                                    format!("{}\t{}", num.trim(), content)
                                } else {
                                    l.to_string()
                                }
                            })
                            .collect()
                    } else {
                        vec![format!("Read {} lines", n)]
                    };
                    (compact, full)
                }
            }
        };
        FormatOutput {
            status_icon: icon,
            param_desc,
            result_compact,
            result_full,
        }
    }
}

// --- EditFormatter ---

struct EditFormatter;

/// Generate colored diff lines from old_string and new_string (no file context).
fn format_diff_simple(old: &str, new: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in old.lines() {
        lines.push(format!("\x1b[31m- {}\x1b[0m", line));
    }
    for line in new.lines() {
        lines.push(format!("\x1b[32m+ {}\x1b[0m", line));
    }
    lines
}

/// Parse the context snippet from the edit tool output and produce a colored diff.
/// The output format after "---" separator:
///   "  ctx_line"   → context (dim)
///   "> changed"    → new line (green), with old line (red) inserted before
///
/// Falls back to simple diff if no context snippet present.
fn format_diff_with_context(output: &str, old: &str, new: &str) -> Vec<String> {
    // Split output by "---" separator; context snippet is after the separator
    let snippet = match output.split_once("\n---\n") {
        Some((_, ctx)) => ctx,
        None => return format_diff_simple(old, new),
    };

    let old_lines: Vec<&str> = old.lines().collect();
    let mut old_idx = 0;
    let mut result = Vec::new();

    for line in snippet.lines() {
        if let Some(content) = line.strip_prefix("> ") {
            // This is a changed line — insert old line(s) first, then new line
            if old_idx == 0 {
                // Insert all old lines before the first new line
                for ol in &old_lines {
                    result.push(format!("\x1b[31m- {}\x1b[0m", ol));
                }
            }
            old_idx += 1;
            result.push(format!("\x1b[32m+ {}\x1b[0m", content));
        } else if let Some(content) = line.strip_prefix("  ") {
            result.push(format!("\x1b[2m  {}\x1b[0m", content));
        } else {
            // Unexpected format, include as-is
            result.push(line.to_string());
        }
    }

    if result.is_empty() {
        return format_diff_simple(old, new);
    }
    result
}

impl ToolFormatter for EditFormatter {
    fn format(&self, input: &FormatInput) -> FormatOutput {
        let param_desc = input
            .params
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let icon = status_icon(&input.output, &input.is_error);
        let (result_compact, result_full) = match &input.output {
            None => (vec![], vec![]),
            Some(_) => {
                if input.is_error == Some(true) {
                    let text = input.output.as_deref().unwrap_or("");
                    let old = input.params.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                    let mut full = all_lines(text);
                    if !old.is_empty() {
                        full.push(String::new());
                        full.push("old_string:".to_string());
                        for line in old.lines() {
                            full.push(format!("  {}", line));
                        }
                    }
                    let compact = head_lines(text, 5);
                    (compact, full)
                } else {
                    let output = input.output.as_deref().unwrap_or("");
                    let old = input.params.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                    let new = input.params.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                    let compact_diff = format_diff_simple(old, new);
                    let compact: Vec<String> = compact_diff.iter().take(5).cloned().collect();
                    let full = format_diff_with_context(output, old, new);
                    (compact, full)
                }
            }
        };
        FormatOutput {
            status_icon: icon,
            param_desc,
            result_compact,
            result_full,
        }
    }
}

// --- Lookup ---

pub fn get_formatter(name: &str) -> &'static dyn ToolFormatter {
    match name {
        "read" => &ReadFormatter,
        "edit" | "write" => &EditFormatter,
        _ => &DefaultFormatter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_input(
        tool_name: &str,
        status_template: &str,
        params: serde_json::Value,
        output: Option<&str>,
        is_error: Option<bool>,
    ) -> FormatInput {
        FormatInput {
            tool_name: tool_name.to_string(),
            display_name: tool_name.to_string(),
            status_template: status_template.to_string(),
            params,
            output: output.map(|s| s.to_string()),
            is_error,
        }
    }

    #[test]
    fn default_formatter_before_execution() {
        let input = make_input(
            "command_query",
            "pattern={pattern}",
            json!({"pattern": "git"}),
            None,
            None,
        );
        let out = DefaultFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Running);
        assert_eq!(out.param_desc, "pattern=git");
        assert!(out.result_compact.is_empty());
        assert!(out.result_full.is_empty());
    }

    #[test]
    fn default_formatter_after_success() {
        let input = make_input(
            "command_query",
            "pattern={pattern}",
            json!({"pattern": "git"}),
            Some("line1\nline2\nline3"),
            Some(false),
        );
        let out = DefaultFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Success);
        assert_eq!(out.result_compact, vec!["line1", "line2", "line3"]);
        assert_eq!(out.result_full, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn default_formatter_after_error() {
        let input = make_input(
            "command_query",
            "pattern={pattern}",
            json!({"pattern": "git"}),
            Some("error: something failed"),
            Some(true),
        );
        let out = DefaultFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Error);
        assert!(!out.result_compact.is_empty());
    }

    #[test]
    fn default_formatter_truncates_compact_to_5_lines() {
        let lines: Vec<&str> = (0..20).map(|_| "x").collect();
        let text = lines.join("\n");
        let input = make_input(
            "command_query",
            "",
            json!({}),
            Some(&text),
            Some(false),
        );
        let out = DefaultFormatter.format(&input);
        assert_eq!(out.result_compact.len(), 5);
        assert_eq!(out.result_full.len(), 20);
    }

    #[test]
    fn default_formatter_escapes_newlines_in_param_desc() {
        let input = make_input(
            "command_query",
            "content={content}",
            json!({"content": "line1\nline2\r\nline3"}),
            None,
            None,
        );
        let out = DefaultFormatter.format(&input);
        assert_eq!(out.param_desc, "content=line1\\nline2\\r\\nline3");
    }

    #[test]
    fn read_formatter_compact_shows_line_count() {
        let input = make_input(
            "read",
            "",
            json!({"file_path": "/tmp/test.txt"}),
            Some("     1\u{2192}aaa\n     2\u{2192}bbb\n     3\u{2192}ccc"),
            Some(false),
        );
        let out = ReadFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Success);
        assert_eq!(out.param_desc, "/tmp/test.txt");
        assert_eq!(out.result_compact, vec!["Read 3 lines"]);
        // N<=10: full shows numbered lines
        assert_eq!(out.result_full, vec!["1\taaa", "2\tbbb", "3\tccc"]);
    }

    #[test]
    fn read_formatter_full_many_lines_shows_summary() {
        let lines: Vec<String> = (1..=15)
            .map(|i| format!("{:>6}\u{2192}line{}", i, i))
            .collect();
        let text = lines.join("\n");
        let input = make_input(
            "read",
            "",
            json!({"file_path": "/tmp/test.txt"}),
            Some(&text),
            Some(false),
        );
        let out = ReadFormatter.format(&input);
        assert_eq!(out.result_compact, vec!["Read 15 lines"]);
        // N>10: full also shows summary
        assert_eq!(out.result_full, vec!["Read 15 lines"]);
    }

    #[test]
    fn edit_formatter_simple_diff_no_context() {
        // No context snippet in output → fallback to simple diff
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/test.txt", "old_string": "hello", "new_string": "goodbye"}),
            Some("Edited /tmp/test.txt"),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Success);
        assert_eq!(out.param_desc, "/tmp/test.txt");
        assert_eq!(out.result_compact.len(), 2);
        assert!(out.result_compact[0].contains("- hello"));
        assert!(out.result_compact[1].contains("+ goodbye"));
        assert_eq!(out.result_full.len(), 2);
    }

    #[test]
    fn edit_formatter_with_context_snippet() {
        // Output includes context snippet from the edit tool
        let output = "Edited /tmp/test.txt\n---\n  line1\n  line2\n  line3\n> goodbye\n  line5\n  line6\n  line7";
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/test.txt", "old_string": "hello", "new_string": "goodbye"}),
            Some(output),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        // compact: simple diff (no context)
        assert_eq!(out.result_compact.len(), 2);
        assert!(out.result_compact[0].contains("- hello"));
        assert!(out.result_compact[1].contains("+ goodbye"));
        // full: 3 context before + 1 old (red) + 1 new (green) + 3 context after = 8
        assert_eq!(out.result_full.len(), 8);
        // Context before (dim)
        assert!(out.result_full[0].contains("line1"));
        assert!(out.result_full[0].contains("\x1b[2m"));
        assert!(out.result_full[2].contains("line3"));
        // Old line (red)
        assert!(out.result_full[3].contains("- hello"));
        assert!(out.result_full[3].contains("\x1b[31m"));
        // New line (green)
        assert!(out.result_full[4].contains("+ goodbye"));
        assert!(out.result_full[4].contains("\x1b[32m"));
        // Context after (dim)
        assert!(out.result_full[5].contains("line5"));
        assert!(out.result_full[7].contains("line7"));
    }

    #[test]
    fn edit_formatter_multiline_with_context() {
        let output = "Edited /tmp/test.txt\n---\n  before\n> new_a\n> new_b\n  after";
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/test.txt", "old_string": "old_a\nold_b\nold_c", "new_string": "new_a\nnew_b"}),
            Some(output),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        // compact: 3 old + 2 new = 5
        assert_eq!(out.result_compact.len(), 5);
        // full: 1 ctx + 3 old + 2 new + 1 ctx = 7
        assert_eq!(out.result_full.len(), 7);
        assert!(out.result_full[0].contains("before"));
        assert!(out.result_full[1].contains("- old_a"));
        assert!(out.result_full[3].contains("- old_c"));
        assert!(out.result_full[4].contains("+ new_a"));
        assert!(out.result_full[5].contains("+ new_b"));
        assert!(out.result_full[6].contains("after"));
    }

    #[test]
    fn edit_formatter_error_shows_message_and_old_string() {
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/test.txt", "old_string": "fn foo() {\n    bar()\n}"}),
            Some("Error: old_string not found in /tmp/test.txt"),
            Some(true),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Error);
        assert_eq!(out.result_compact, vec!["Error: old_string not found in /tmp/test.txt"]);
        // full includes error message + blank line + old_string label + indented content
        assert_eq!(out.result_full[0], "Error: old_string not found in /tmp/test.txt");
        assert_eq!(out.result_full[1], "");
        assert_eq!(out.result_full[2], "old_string:");
        assert_eq!(out.result_full[3], "  fn foo() {");
        assert_eq!(out.result_full[4], "      bar()");
        assert_eq!(out.result_full[5], "  }");
    }

    #[test]
    fn edit_formatter_error_no_old_string() {
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/test.txt"}),
            Some("permission denied"),
            Some(true),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Error);
        assert_eq!(out.result_full, vec!["permission denied"]);
    }
}
