use omnish_llm::tool::ToolResult;

/// Maximum bytes to return.
const MAX_OUTPUT_BYTES: usize = 50_000;
/// Maximum characters per line before truncation.
const MAX_LINE_CHARS: usize = 2000;

#[derive(Default)]
pub struct ReadTool;

impl ReadTool {
    pub fn new() -> Self {
        Self
    }

    pub fn execute(&self, input: &serde_json::Value) -> ToolResult {
        let file_path = input["file_path"].as_str().unwrap_or("");
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

        let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
        // 0 means unlimited; only apply DEFAULT_LIMIT when explicitly requested
        let limit = input["limit"].as_u64().unwrap_or(0) as usize;

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

        if content.is_empty() {
            return ToolResult {
                tool_use_id: String::new(),
                content: "<system-reminder>This file exists but has empty contents.</system-reminder>".to_string(),
                is_error: false,
            };
        }

        let total_chars = content.chars().count();

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        let start = (offset - 1).min(total_lines);
        let effective_limit = if limit == 0 { total_lines - start } else { limit };
        let end = (start + effective_limit).min(total_lines);

        let mut result = String::new();
        let mut char_count = 0;
        let mut truncated_by_bytes = false;

        for (i, &line) in lines[start..end].iter().enumerate() {
            let line_no = start + i + 1;
            let display_line = if line.chars().count() > MAX_LINE_CHARS {
                let truncated: String = line.chars().take(MAX_LINE_CHARS).collect();
                format!("{truncated}...")
            } else {
                line.to_string()
            };
            let numbered = format!("{:>6}\t{}\n", line_no, display_line);
            char_count += numbered.chars().count();
            if char_count > MAX_OUTPUT_BYTES {
                result.push_str(&format!(
                    "\nFile content ({} characters) exceeds maximum allowed characters ({}). \
                    Please use offset and limit parameters to read specific portions of the file, \
                    or use the GrepTool to search for specific content.",
                    total_chars,
                    MAX_OUTPUT_BYTES,
                ));
                truncated_by_bytes = true;
                break;
            }
            result.push_str(&numbered);
        }

        if end < total_lines && !truncated_by_bytes {
            result.push_str(&format!(
                "\n({} more lines after line {})",
                total_lines - end,
                end
            ));
        }

        ToolResult {
            tool_use_id: String::new(),
            content: result,
            is_error: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_read_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "line1\nline2\nline3").unwrap();
        let tool = ReadTool::new();
        let result = tool.execute(&serde_json::json!({"file_path": tmp.path().to_str().unwrap()}));
        assert!(!result.is_error);
        assert!(result.content.contains("line1"));
        assert!(result.content.contains("\t"));
    }

    #[test]
    fn test_read_with_offset() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "a\nb\nc\nd\ne").unwrap();
        let tool = ReadTool::new();
        let result = tool.execute(&serde_json::json!({"file_path": tmp.path().to_str().unwrap(), "offset": 3}));
        assert!(!result.is_error);
        assert!(result.content.contains("c"));
        assert!(result.content.contains("d"));
        // Line "a" should not appear (it's before offset)
        assert!(!result.content.contains("\ta\n"));
    }

    #[test]
    fn test_read_with_limit() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "a\nb\nc\nd\ne").unwrap();
        let tool = ReadTool::new();
        let result = tool.execute(&serde_json::json!({"file_path": tmp.path().to_str().unwrap(), "limit": 2}));
        assert!(!result.is_error);
        assert!(result.content.contains("a"));
        assert!(result.content.contains("b"));
        assert!(result.content.contains("more lines"));
    }

    #[test]
    fn test_read_nonexistent() {
        let tool = ReadTool::new();
        let result = tool.execute(&serde_json::json!({"file_path": "/nonexistent/file.txt"}));
        assert!(result.is_error);
        assert!(result.content.contains("Error reading"));
    }

    #[test]
    fn test_read_empty_path() {
        let tool = ReadTool::new();
        let result = tool.execute(&serde_json::json!({"file_path": ""}));
        assert!(result.is_error);
        assert!(result.content.contains("required"));
    }

    #[test]
    fn test_read_relative_path() {
        let tool = ReadTool::new();
        let result = tool.execute(&serde_json::json!({"file_path": "relative/path.txt"}));
        assert!(result.is_error);
        assert!(result.content.contains("absolute path"));
    }

    #[test]
    fn test_read_empty_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let tool = ReadTool::new();
        let result = tool.execute(&serde_json::json!({"file_path": tmp.path().to_str().unwrap()}));
        assert!(!result.is_error);
        assert!(result.content.contains("empty contents"));
    }

    #[test]
    fn test_long_line_truncation() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let long_line = "x".repeat(2500);
        write!(tmp, "{}", long_line).unwrap();
        let tool = ReadTool::new();
        let result = tool.execute(&serde_json::json!({"file_path": tmp.path().to_str().unwrap()}));
        assert!(!result.is_error);
        assert!(result.content.contains("..."));
        // Should contain exactly 2000 x's + "..."
        assert!(!result.content.contains(&"x".repeat(2001)));
    }

    #[test]
    fn test_long_line_truncation_multibyte() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        // 2001 CJK characters (3 bytes each) - must not panic
        let long_line = "操".repeat(2001);
        write!(tmp, "{}", long_line).unwrap();
        let tool = ReadTool::new();
        let result = tool.execute(&serde_json::json!({"file_path": tmp.path().to_str().unwrap()}));
        assert!(!result.is_error);
        assert!(result.content.contains("..."));
        assert!(!result.content.contains(&"操".repeat(2001)));
    }
}
