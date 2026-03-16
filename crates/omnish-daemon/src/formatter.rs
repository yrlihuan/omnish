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
            Some(text) => {
                let full = all_lines(text);
                let compact = if input.is_error == Some(true) {
                    head_lines(text, 5)
                } else {
                    vec!["done".to_string()]
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
    fn edit_formatter_compact_shows_done() {
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/test.txt"}),
            Some("ok"),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Success);
        assert_eq!(out.param_desc, "/tmp/test.txt");
        assert_eq!(out.result_compact, vec!["done"]);
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
