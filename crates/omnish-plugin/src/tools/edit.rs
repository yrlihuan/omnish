use omnish_llm::tool::ToolResult;

/// Extract context lines around an edit location in `content`.
/// `edit_start_line` is the 0-based line index where the edit starts.
/// `changed_count` is the number of new lines at that position (0 for deletion).
/// Each line is formatted as "lineno:  content" (context) or "lineno:>content" (changed).
/// For deletion (changed_count == 0), a "lineno:D" marker is inserted.
/// Line numbers are 1-based.
fn build_context_snippet(
    content: &str,
    edit_start_line: usize,
    changed_count: usize,
    ctx: usize,
) -> String {
    let file_lines: Vec<&str> = content.lines().collect();
    let end_line = edit_start_line + changed_count;

    let ctx_start = edit_start_line.saturating_sub(ctx);
    let ctx_end = (end_line + ctx).min(file_lines.len());

    let mut lines = Vec::new();
    let mut deletion_marker_inserted = false;

    for (i, file_line) in file_lines.iter().enumerate().take(ctx_end).skip(ctx_start) {
        let lineno = i + 1; // 1-based
        // For deletion: insert marker before first context-after line
        if changed_count == 0 && i >= edit_start_line && !deletion_marker_inserted {
            lines.push(format!("{}:D", lineno));
            deletion_marker_inserted = true;
        }
        if i >= edit_start_line && i < end_line {
            lines.push(format!("{}:>{}", lineno, file_line));
        } else {
            lines.push(format!("{}:  {}", lineno, file_line));
        }
    }

    // Deletion at end of file
    if changed_count == 0 && !deletion_marker_inserted {
        let lineno = edit_start_line + 1;
        lines.push(format!("{}:D", lineno));
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

        // Compute edit position (line number) before replacement
        let edit_start_line = content[..content.find(old_string).unwrap()]
            .chars()
            .filter(|&c| c == '\n')
            .count();

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

        // Build context snippet using the known edit position
        let new_line_count = if new_string.is_empty() {
            0
        } else {
            new_string.lines().count()
        };
        let snippet = build_context_snippet(&new_content, edit_start_line, new_line_count, 3);
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
    fn test_edit_returns_numbered_context_snippet() {
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
        assert!(result.content.contains("\n---\n"), "should have context separator");
        let snippet = result.content.split("\n---\n").nth(1).unwrap();
        // Numbered context before (lines 1-3)
        assert!(snippet.contains("1:  line1"), "snippet: {}", snippet);
        assert!(snippet.contains("3:  line3"));
        // Numbered changed line (line 4)
        assert!(snippet.contains("4:>goodbye"));
        // Numbered context after (lines 5-7)
        assert!(snippet.contains("5:  line5"));
        assert!(snippet.contains("7:  line7"));
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

    #[test]
    fn test_deletion_snippet_has_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("del.txt");
        fs::write(&path, "a\nb\nc\nd\ne\nf\ng\n").unwrap();
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "old_string": "c\nd\ne",
            "new_string": ""
        }));
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\nb\n\nf\ng\n");
        let snippet = result.content.split("\n---\n").nth(1).unwrap();
        // Context before deletion
        assert!(snippet.contains("2:  b"), "snippet: {}", snippet);
        // Deletion marker
        assert!(snippet.contains(":D"), "should have deletion marker, snippet: {}", snippet);
        // Context after deletion
        assert!(snippet.contains("f"), "snippet: {}", snippet);
    }

    #[test]
    fn test_snippet_uses_correct_position_when_new_string_appears_earlier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dup.txt");
        // "goodbye" already appears at line 1; editing "hello" at line 4
        fs::write(&path, "goodbye\nline2\nline3\nhello\nline5\nline6\n").unwrap();
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "old_string": "hello",
            "new_string": "goodbye"
        }));
        assert!(!result.is_error, "{}", result.content);
        let snippet = result.content.split("\n---\n").nth(1).unwrap();
        // The changed line should be at line 4, not line 1
        assert!(snippet.contains("4:>goodbye"), "snippet should show change at line 4: {}", snippet);
    }
}
