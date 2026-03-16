use omnish_llm::tool::ToolResult;

/// Extract context lines around `needle` in `content`.
/// Returns the snippet as "ctx_before... | new_lines... | ctx_after..."
/// with each line prefixed by "  " (context) or "> " (changed).
fn build_context_snippet(content: &str, needle: &str, ctx: usize) -> String {
    let pos = match content.find(needle) {
        Some(p) => p,
        None => return String::new(),
    };
    let file_lines: Vec<&str> = content.lines().collect();
    let start_line = content[..pos].chars().filter(|&c| c == '\n').count();
    let needle_line_count = needle.lines().count().max(1);
    let end_line = start_line + needle_line_count; // exclusive

    let ctx_start = start_line.saturating_sub(ctx);
    let ctx_end = (end_line + ctx).min(file_lines.len());

    let mut lines = Vec::new();
    for (i, line) in file_lines.iter().enumerate().take(ctx_end).skip(ctx_start) {
        if i >= start_line && i < end_line {
            lines.push(format!("> {}", line));
        } else {
            lines.push(format!("  {}", line));
        }
    }
    lines.join("\n")
}

#[derive(Default)]
pub struct EditTool;

impl EditTool {
    pub fn new() -> Self {
        Self
    }

    pub fn execute(&self, input: &serde_json::Value) -> ToolResult {
        let file_path = input["file_path"].as_str().unwrap_or("");
        let old_string = input["old_string"].as_str().unwrap_or("");
        let new_string = input["new_string"].as_str().unwrap_or("");
        let replace_all = input["replace_all"].as_bool().unwrap_or(false);

        if file_path.is_empty() {
            return ToolResult {
                tool_use_id: String::new(),
                content: "Error: 'file_path' is required".to_string(),
                is_error: true,
            };
        }

        if !file_path.starts_with('/') {
            return ToolResult {
                tool_use_id: String::new(),
                content: format!("Error: file_path must be an absolute path, got: {}", file_path),
                is_error: true,
            };
        }

        if old_string.is_empty() {
            return ToolResult {
                tool_use_id: String::new(),
                content: "Error: 'old_string' must not be empty".to_string(),
                is_error: true,
            };
        }

        if old_string == new_string {
            return ToolResult {
                tool_use_id: String::new(),
                content: "Error: 'old_string' and 'new_string' must be different".to_string(),
                is_error: true,
            };
        }

        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(e) => {
                return ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Error reading {}: {}", file_path, e),
                    is_error: true,
                };
            }
        };

        let count = content.matches(old_string).count();

        if count == 0 {
            return ToolResult {
                tool_use_id: String::new(),
                content: format!(
                    "Error: old_string not found in {}. Make sure it matches exactly including whitespace and indentation.",
                    file_path
                ),
                is_error: true,
            };
        }

        if count > 1 && !replace_all {
            return ToolResult {
                tool_use_id: String::new(),
                content: format!(
                    "Error: old_string appears {} times in {}. Provide more context to make it unique, or set replace_all to true.",
                    count, file_path
                ),
                is_error: true,
            };
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        if let Err(e) = std::fs::write(file_path, &new_content) {
            return ToolResult {
                tool_use_id: String::new(),
                content: format!("Error writing {}: {}", file_path, e),
                is_error: true,
            };
        }

        let msg = if replace_all && count > 1 {
            format!("Replaced {} occurrences in {}", count, file_path)
        } else {
            format!("Edited {}", file_path)
        };

        // Build context snippet: N lines before + new_string lines + N lines after
        let snippet = build_context_snippet(&new_content, new_string, 3);
        let content = if snippet.is_empty() {
            msg
        } else {
            format!("{}\n---\n{}", msg, snippet)
        };

        ToolResult {
            tool_use_id: String::new(),
            content,
            is_error: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_basic_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "old_string": "hello",
            "new_string": "goodbye"
        }));
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(fs::read_to_string(&path).unwrap(), "goodbye world");
    }

    #[test]
    fn test_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "old_string": "missing",
            "new_string": "replacement"
        }));
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[test]
    fn test_ambiguous_without_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "foo bar foo baz foo").unwrap();
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "qux"
        }));
        assert!(result.is_error);
        assert!(result.content.contains("3 times"));
    }

    #[test]
    fn test_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "foo bar foo baz foo").unwrap();
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "qux",
            "replace_all": true
        }));
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(fs::read_to_string(&path).unwrap(), "qux bar qux baz qux");
        assert!(result.content.contains("3 occurrences"));
    }

    #[test]
    fn test_same_string_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello").unwrap();
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "old_string": "hello",
            "new_string": "hello"
        }));
        assert!(result.is_error);
        assert!(result.content.contains("different"));
    }

    #[test]
    fn test_relative_path_rejected() {
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": "relative.txt",
            "old_string": "a",
            "new_string": "b"
        }));
        assert!(result.is_error);
        assert!(result.content.contains("absolute"));
    }

    #[test]
    fn test_empty_old_string() {
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": "/tmp/test.txt",
            "old_string": "",
            "new_string": "b"
        }));
        assert!(result.is_error);
        assert!(result.content.contains("empty"));
    }

    #[test]
    fn test_edit_returns_context_snippet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ctx.txt");
        fs::write(&path, "line1\nline2\nline3\nhello\nline5\nline6\nline7\n").unwrap();
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "old_string": "hello",
            "new_string": "goodbye"
        }));
        assert!(!result.is_error, "{}", result.content);
        // Should contain the "---" separator and context
        assert!(result.content.contains("\n---\n"), "should have context separator");
        let snippet = result.content.split("\n---\n").nth(1).unwrap();
        // Context before (3 lines)
        assert!(snippet.contains("  line1"));
        assert!(snippet.contains("  line3"));
        // Changed line
        assert!(snippet.contains("> goodbye"));
        // Context after
        assert!(snippet.contains("  line5"));
        assert!(snippet.contains("  line7"));
    }

    #[test]
    fn test_multiline_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "line1\nline2\nline3\n").unwrap();
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "old_string": "line1\nline2",
            "new_string": "replaced1\nreplaced2"
        }));
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(fs::read_to_string(&path).unwrap(), "replaced1\nreplaced2\nline3\n");
    }
}
