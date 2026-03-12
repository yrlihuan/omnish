use omnish_llm::tool::ToolResult;

/// Maximum bytes to return.
const MAX_OUTPUT_BYTES: usize = 50_000;
/// Default and maximum lines to return.
const DEFAULT_LIMIT: usize = 500;
/// Maximum characters per line before truncation.
const MAX_LINE_CHARS: usize = 200;

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
        let limit = input["limit"].as_u64().unwrap_or(DEFAULT_LIMIT as u64) as usize;

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

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        let start = (offset - 1).min(total_lines);
        let end = (start + limit).min(total_lines);

        let mut result = String::new();
        let mut bytes = 0;

        for (i, &line) in lines[start..end].iter().enumerate() {
            let line_no = start + i + 1;
            let display_line = if line.len() > MAX_LINE_CHARS {
                format!("{}...", &line[..MAX_LINE_CHARS])
            } else {
                line.to_string()
            };
            let numbered = format!("{:>6}\u{2192}{}\n", line_no, display_line);
            bytes += numbered.len();
            if bytes > MAX_OUTPUT_BYTES {
                result.push_str(&format!(
                    "\n... (truncated at {} bytes, showing {}/{} lines)",
                    MAX_OUTPUT_BYTES,
                    i,
                    total_lines
                ));
                break;
            }
            result.push_str(&numbered);
        }

        if end < total_lines && bytes <= MAX_OUTPUT_BYTES {
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
        assert!(result.content.contains("\u{2192}"));
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
        assert!(!result.content.contains("\u{2192}a\n"));
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
        let long_line = "x".repeat(300);
        write!(tmp, "{}", long_line).unwrap();
        let tool = ReadTool::new();
        let result = tool.execute(&serde_json::json!({"file_path": tmp.path().to_str().unwrap()}));
        assert!(!result.is_error);
        assert!(result.content.contains("..."));
        // Should contain exactly 200 x's + "..."
        assert!(!result.content.contains(&"x".repeat(201)));
    }
}
