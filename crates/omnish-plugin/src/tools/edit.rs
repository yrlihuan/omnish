use omnish_llm::tool::ToolResult;

/// Build a self-contained diff snippet showing context, old lines (-), and new lines (+).
/// Uses both old and new content to produce a unified-diff-like output.
/// Line numbers are 1-based.
///
/// Format: `lineno:  content` (context), `lineno:-content` (removed), `lineno:+content` (added).
#[allow(clippy::needless_range_loop)]
fn build_context_snippet(
    old_content: &str,
    new_content: &str,
    edit_start_line: usize,
    old_line_count: usize,
    new_line_count: usize,
    ctx: usize,
) -> String {
    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();
    let old_end = edit_start_line + old_line_count;
    let new_end = edit_start_line + new_line_count;

    let ctx_start = edit_start_line.saturating_sub(ctx);
    let ctx_after_end = (new_end + ctx).min(new_lines.len());

    let mut lines = Vec::new();

    // Context before (same in old and new)
    for i in ctx_start..edit_start_line {
        if i < new_lines.len() {
            lines.push(format!("{}:  {}", i + 1, new_lines[i]));
        }
    }

    // Old lines (removed)
    for i in edit_start_line..old_end {
        if i < old_lines.len() {
            lines.push(format!("{}:-{}", i + 1, old_lines[i]));
        }
    }

    // New lines (added)
    for i in edit_start_line..new_end {
        if i < new_lines.len() {
            lines.push(format!("{}:+{}", i + 1, new_lines[i]));
        }
    }

    // Context after (from new content)
    for i in new_end..ctx_after_end {
        lines.push(format!("{}:  {}", i + 1, new_lines[i]));
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

        // Build context snippet using both old and new content
        let old_line_count = if old_string.is_empty() { 0 } else { old_string.lines().count() };
        let new_line_count = if new_string.is_empty() { 0 } else { new_string.lines().count() };
        let snippet = build_context_snippet(
            &content, &new_content, edit_start_line,
            old_line_count, new_line_count, 3,
        );
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
        // Old line removed
        assert!(snippet.contains("4:-hello"), "snippet: {}", snippet);
        // New line added
        assert!(snippet.contains("4:+goodbye"), "snippet: {}", snippet);
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
    fn test_deletion_snippet() {
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
        // Old lines shown as removed
        assert!(snippet.contains("3:-c"), "snippet: {}", snippet);
        assert!(snippet.contains("5:-e"), "snippet: {}", snippet);
        // Context after deletion
        assert!(snippet.contains("f"), "snippet: {}", snippet);
        // No + lines (deletion only)
        assert!(!snippet.contains(":+"), "snippet should have no + lines: {}", snippet);
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
        // The old and new lines should be at line 4, not line 1
        assert!(snippet.contains("4:-hello"), "snippet should show old at line 4: {}", snippet);
        assert!(snippet.contains("4:+goodbye"), "snippet should show new at line 4: {}", snippet);
    }
}
