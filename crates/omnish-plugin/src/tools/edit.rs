use omnish_llm::tool::ToolResult;

/// Build a self-contained diff snippet showing context, old lines (-), and new lines (+).
/// Uses both old and new content to produce a unified-diff-like output.
/// Line numbers are 1-based.
///
/// Format: `lineno:  content` (context), `lineno:-content` (removed), `lineno:+content` (added).
///
/// Common prefix/suffix lines between old and new edit regions are shown as context
/// rather than as removed-then-added.
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

    // Extract the edit region lines from old and new
    let old_edit: Vec<&str> = old_lines[edit_start_line..old_end.min(old_lines.len())].to_vec();
    let new_edit: Vec<&str> = new_lines[edit_start_line..new_end.min(new_lines.len())].to_vec();

    // Find common prefix length
    let common_prefix = old_edit.iter().zip(new_edit.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // Find common suffix length (don't overlap with prefix)
    let remaining_old = old_edit.len() - common_prefix;
    let remaining_new = new_edit.len() - common_prefix;
    let common_suffix = old_edit[common_prefix..].iter().rev()
        .zip(new_edit[common_prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(remaining_old)
        .min(remaining_new);

    // Limit common prefix/suffix context to `ctx` lines each to avoid noise
    let shown_prefix = common_prefix.min(ctx);
    let shown_suffix = common_suffix.min(ctx);

    let old_changed_end = old_edit.len() - common_suffix;
    let new_changed_end = new_edit.len() - common_suffix;
    let suffix_ctx_start = new_changed_end; // first common suffix line index
    let ctx_after_end = (new_end - common_suffix + shown_suffix + ctx).min(new_lines.len());

    let mut lines = Vec::new();

    // Context before (from file, before the edit region + common prefix)
    let file_ctx_start = (edit_start_line + common_prefix - shown_prefix).saturating_sub(ctx);
    let file_ctx_end = edit_start_line + common_prefix - shown_prefix;
    for i in file_ctx_start..file_ctx_end {
        if i < new_lines.len() {
            lines.push(format!("{}:  {}", i + 1, new_lines[i]));
        }
    }

    // Tail of common prefix (up to `ctx` lines before the change)
    let prefix_start = common_prefix - shown_prefix;
    for i in prefix_start..common_prefix {
        let line_idx = edit_start_line + i;
        lines.push(format!("{}:  {}", line_idx + 1, new_edit[i]));
    }

    // Changed old lines (removed)
    for i in common_prefix..old_changed_end {
        let line_idx = edit_start_line + i;
        lines.push(format!("{}:-{}", line_idx + 1, old_edit[i]));
    }

    // Changed new lines (added) - use new line numbers
    for i in common_prefix..new_changed_end {
        let line_idx = edit_start_line + i;
        lines.push(format!("{}:+{}", line_idx + 1, new_edit[i]));
    }

    // Head of common suffix (up to `ctx` lines after the change)
    for i in suffix_ctx_start..(suffix_ctx_start + shown_suffix) {
        let line_idx = edit_start_line + i;
        if i < new_edit.len() {
            lines.push(format!("{}:  {}", line_idx + 1, new_edit[i]));
        }
    }

    // Context after (from file, after the edit region + common suffix)
    let after_start = new_end - common_suffix + shown_suffix;
    for i in after_start..ctx_after_end {
        if i < new_lines.len() {
            lines.push(format!("{}:  {}", i + 1, new_lines[i]));
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

    #[test]
    fn test_unchanged_lines_shown_as_context() {
        // When old_string and new_string share common prefix/suffix lines,
        // those lines should appear as context (space), not as -/+.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.txt");
        fs::write(
            &path,
            "line1\nline2\nshared_a\nshared_b\nold_line\nshared_c\nline7\nline8\n",
        )
        .unwrap();
        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "old_string": "shared_a\nshared_b\nold_line\nshared_c",
            "new_string": "shared_a\nshared_b\nnew_line1\nnew_line2\nshared_c"
        }));
        assert!(!result.is_error, "{}", result.content);
        let snippet = result.content.split("\n---\n").nth(1).unwrap();
        // shared_a and shared_b should be context, not -/+
        assert!(snippet.contains("3:  shared_a"), "shared_a should be context: {}", snippet);
        assert!(snippet.contains("4:  shared_b"), "shared_b should be context: {}", snippet);
        // old_line should be removed
        assert!(snippet.contains("5:-old_line"), "old_line should be removed: {}", snippet);
        // new lines should be added
        assert!(snippet.contains("5:+new_line1"), "new_line1 should be added: {}", snippet);
        assert!(snippet.contains("6:+new_line2"), "new_line2 should be added: {}", snippet);
        // shared_c should be context
        assert!(snippet.contains(":  shared_c"), "shared_c should be context: {}", snippet);
        // No -/+ for shared lines
        assert!(!snippet.contains(":-shared_a"), "shared_a should NOT be removed: {}", snippet);
        assert!(!snippet.contains(":+shared_a"), "shared_a should NOT be added: {}", snippet);
    }

    #[test]
    fn test_large_common_suffix_is_truncated() {
        // When old_string/new_string have many common suffix lines,
        // only a few should be shown (ctx=3), not all of them.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        let file_lines: Vec<String> = (1..=50).map(|i| format!("line{}", i)).collect();
        fs::write(&path, file_lines.join("\n")).unwrap();

        // old_string: lines 5-45 (line5 through line45 = 41 lines)
        // new_string: change only "line5" to "modified5", rest identical
        let old_str: String = (5..=45).map(|i| format!("line{}", i)).collect::<Vec<_>>().join("\n");
        let new_str = format!("modified5\n{}", (6..=45).map(|i| format!("line{}", i)).collect::<Vec<_>>().join("\n"));

        let tool = EditTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "old_string": old_str,
            "new_string": new_str,
        }));
        assert!(!result.is_error, "{}", result.content);
        let snippet = result.content.split("\n---\n").nth(1).unwrap();

        // Should have: context before (3) + changed line (-/+) (2) + limited suffix context (3) + context after (3) = ~11
        let line_count = snippet.lines().count();
        assert!(line_count <= 15, "snippet should be compact but has {} lines:\n{}", line_count, snippet);

        // The changed line should be shown
        assert!(snippet.contains("5:-line5"), "should show old line5: {}", snippet);
        assert!(snippet.contains("5:+modified5"), "should show new modified5: {}", snippet);

        // Common suffix lines far from the change (e.g., line30) should NOT appear
        assert!(!snippet.contains("line30"), "line30 should not appear in snippet: {}", snippet);
    }
}
