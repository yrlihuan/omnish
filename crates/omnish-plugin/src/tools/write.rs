use crate::{Plugin, PluginType};
use omnish_llm::tool::{Tool, ToolDef, ToolResult};

pub struct WriteTool;

impl WriteTool {
    pub fn new() -> Self {
        Self
    }

    fn write_file(&self, file_path: &str, content: &str) -> ToolResult {
        let path = std::path::Path::new(file_path);

        // Require absolute path
        if !path.is_absolute() {
            return ToolResult {
                tool_use_id: String::new(),
                content: format!("Error: file_path must be absolute, got: {}", file_path),
                is_error: true,
            };
        }

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolResult {
                        tool_use_id: String::new(),
                        content: format!("Error creating directory {}: {}", parent.display(), e),
                        is_error: true,
                    };
                }
            }
        }

        match std::fs::write(path, content) {
            Ok(()) => {
                let lines = content.lines().count();
                let bytes = content.len();
                ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Wrote {} bytes ({} lines) to {}", bytes, lines, file_path),
                    is_error: false,
                }
            }
            Err(e) => ToolResult {
                tool_use_id: String::new(),
                content: format!("Error writing {}: {}", file_path, e),
                is_error: true,
            },
        }
    }
}

impl Tool for WriteTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: "write".to_string(),
            description: "Write content to a file, creating it if it doesn't exist or \
                overwriting if it does. Parent directories are created automatically. \
                Use this for creating new files or completely replacing file contents."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write to the file"
                    }
                },
                "required": ["file_path", "content"]
            }),
        }
    }

    fn execute(&self, input: &serde_json::Value) -> ToolResult {
        let file_path = input["file_path"].as_str().unwrap_or("");
        let content = input["content"].as_str().unwrap_or("");

        if file_path.is_empty() {
            return ToolResult {
                tool_use_id: String::new(),
                content: "Error: 'file_path' is required".to_string(),
                is_error: true,
            };
        }

        self.write_file(file_path, content)
    }
}

impl Plugin for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::ClientTool
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![self.definition()]
    }

    fn call_tool(&self, _tool_name: &str, input: &serde_json::Value) -> ToolResult {
        self.execute(input)
    }

    fn status_text(&self, _tool_name: &str, input: &serde_json::Value) -> String {
        let path = input["file_path"].as_str().unwrap_or("");
        let preview: String = path.chars().rev().take(50).collect::<String>().chars().rev().collect();
        if preview.len() < path.len() {
            format!("写入: ...{}", preview)
        } else {
            format!("写入: {}", preview)
        }
    }

    fn system_prompt(&self) -> Option<String> {
        Some(
            "### write\n\
             Write content to files on the user's machine:\n\
             - file_path must be an absolute path.\n\
             - Parent directories are created automatically.\n\
             - Overwrites existing files. Use with care.\n\
             - Runs in a sandboxed environment with restricted write access."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_write_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let tool = WriteTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "content": "hello\nworld"
        }));
        assert!(!result.is_error, "should succeed: {}", result.content);
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello\nworld");
        assert!(result.content.contains("2 lines"));
    }

    #[test]
    fn test_overwrite_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "old content").unwrap();
        let tool = WriteTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "content": "new content"
        }));
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&path).unwrap(), "new content");
    }

    #[test]
    fn test_create_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/test.txt");
        let tool = WriteTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "content": "nested"
        }));
        assert!(!result.is_error, "should create parents: {}", result.content);
        assert_eq!(fs::read_to_string(&path).unwrap(), "nested");
    }

    #[test]
    fn test_relative_path_rejected() {
        let tool = WriteTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": "relative/path.txt",
            "content": "test"
        }));
        assert!(result.is_error);
        assert!(result.content.contains("absolute"));
    }

    #[test]
    fn test_empty_path() {
        let tool = WriteTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": "",
            "content": "test"
        }));
        assert!(result.is_error);
        assert!(result.content.contains("required"));
    }

    #[test]
    fn test_empty_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        let tool = WriteTool::new();
        let result = tool.execute(&serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "content": ""
        }));
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
    }
}
