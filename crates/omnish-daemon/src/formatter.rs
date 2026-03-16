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
                let full = all_lines(text);
                let compact = if input.is_error == Some(true) {
                    head_lines(text, 5)
                } else {
                    vec![format!("Read {} lines", full.len())]
                };
                (compact, full)
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

const CONTEXT_LINES: usize = 3;

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

/// Generate diff with surrounding context lines from the edited file.
/// Falls back to simple diff if file cannot be read or new_string not found.
fn format_diff_with_context(file_path: &str, old: &str, new: &str) -> Vec<String> {
    let file_content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(_) => return format_diff_simple(old, new),
    };

    let new_pos = match file_content.find(new) {
        Some(pos) => pos,
        None => return format_diff_simple(old, new),
    };

    let file_lines: Vec<&str> = file_content.lines().collect();

    // Find the line range where new_string sits
    let prefix = &file_content[..new_pos];
    let start_line = prefix.chars().filter(|&c| c == '\n').count();
    let new_line_count = new.lines().count().max(1);
    let end_line = start_line + new_line_count; // exclusive

    // Context range
    let ctx_start = start_line.saturating_sub(CONTEXT_LINES);
    let ctx_end = (end_line + CONTEXT_LINES).min(file_lines.len());

    let mut result = Vec::new();

    // Context before
    for i in ctx_start..start_line {
        result.push(format!("\x1b[2m  {}\x1b[0m", file_lines[i]));
    }
    // Old lines (removed)
    for line in old.lines() {
        result.push(format!("\x1b[31m- {}\x1b[0m", line));
    }
    // New lines (added)
    for line in new.lines() {
        result.push(format!("\x1b[32m+ {}\x1b[0m", line));
    }
    // Context after
    for i in end_line..ctx_end {
        result.push(format!("\x1b[2m  {}\x1b[0m", file_lines[i]));
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
                    let full = all_lines(text);
                    let compact = head_lines(text, 5);
                    (compact, full)
                } else {
                    let file_path = input.params.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
                    let old = input.params.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                    let new = input.params.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                    let compact_diff = format_diff_simple(old, new);
                    let compact: Vec<String> = compact_diff.iter().take(5).cloned().collect();
                    let full = format_diff_with_context(file_path, old, new);
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
            Some("aaa\nbbb\nccc"),
            Some(false),
        );
        let out = ReadFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Success);
        assert_eq!(out.param_desc, "/tmp/test.txt");
        assert_eq!(out.result_compact, vec!["Read 3 lines"]);
    }

    #[test]
    fn edit_formatter_shows_colored_diff() {
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/nonexistent.txt", "old_string": "hello", "new_string": "goodbye"}),
            Some("ok"),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Success);
        assert_eq!(out.param_desc, "/tmp/nonexistent.txt");
        // compact: simple diff (fallback, file doesn't exist)
        assert_eq!(out.result_compact.len(), 2);
        assert!(out.result_compact[0].contains("- hello"));
        assert!(out.result_compact[1].contains("+ goodbye"));
        // full: also simple diff fallback
        assert_eq!(out.result_full.len(), 2);
        assert!(out.result_full[0].contains("- hello"));
        assert!(out.result_full[1].contains("+ goodbye"));
    }

    #[test]
    fn edit_formatter_multiline_diff() {
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/nonexistent.txt", "old_string": "a\nb\nc", "new_string": "x\ny"}),
            Some("ok"),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        // compact: 3 old + 2 new = 5, take 5
        assert_eq!(out.result_compact.len(), 5);
        assert!(out.result_compact[0].contains("- a"));
        assert!(out.result_compact[3].contains("+ x"));
        // full: fallback simple diff (file doesn't exist)
        assert_eq!(out.result_full.len(), 5);
    }

    #[test]
    fn edit_formatter_with_file_context() {
        use std::io::Write;
        // Create a temp file with known content
        let dir = std::env::temp_dir();
        let path = dir.join("omnish_test_edit_ctx.txt");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "line1").unwrap();
            writeln!(f, "line2").unwrap();
            writeln!(f, "line3").unwrap();
            writeln!(f, "REPLACED").unwrap();
            writeln!(f, "line5").unwrap();
            writeln!(f, "line6").unwrap();
            writeln!(f, "line7").unwrap();
        }
        let input = make_input(
            "edit",
            "",
            json!({
                "file_path": path.to_str().unwrap(),
                "old_string": "ORIGINAL",
                "new_string": "REPLACED"
            }),
            Some("ok"),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        // compact: simple diff (no context)
        assert_eq!(out.result_compact.len(), 2);
        assert!(out.result_compact[0].contains("- ORIGINAL"));
        assert!(out.result_compact[1].contains("+ REPLACED"));
        // full: context lines + diff lines + context lines
        // "REPLACED" is at line index 3, context=3 → lines 0..3 before, lines 4..7 after
        assert!(out.result_full.len() > 2, "full should include context lines");
        // Check context lines are dim
        assert!(out.result_full[0].contains("line1"));
        assert!(out.result_full[0].contains("\x1b[2m"));
        // Check diff lines present
        let has_old = out.result_full.iter().any(|l| l.contains("- ORIGINAL"));
        let has_new = out.result_full.iter().any(|l| l.contains("+ REPLACED"));
        assert!(has_old, "should have old line");
        assert!(has_new, "should have new line");
        // Check context after
        let has_after = out.result_full.iter().any(|l| l.contains("line5"));
        assert!(has_after, "should have context after");
        // Cleanup
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn edit_formatter_error_shows_message() {
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/test.txt"}),
            Some("permission denied\ndetails here\nmore info"),
            Some(true),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Error);
        assert_eq!(
            out.result_compact,
            vec!["permission denied", "details here", "more info"]
        );
        assert_eq!(
            out.result_full,
            vec!["permission denied", "details here", "more info"]
        );
    }
}
