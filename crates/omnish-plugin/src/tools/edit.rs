use crate::{Plugin, PluginType};
use omnish_llm::tool::{Tool, ToolDef, ToolResult};

pub struct EditTool;

impl EditTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for EditTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: "edit".to_string(),
            description: "Perform exact string replacements in files. The old_string must match \
                exactly. If old_string appears more than once and replace_all is false, the edit \
                will fail — provide more surrounding context to make it unique, or set replace_all \
                to true."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the file to edit"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact text to find and replace"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement text"
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace all occurrences (default: false)"
                    }
                },
                "required": ["file_path", "old_string", "new_string"]
            }),
        }
    }

    fn execute(&self, input: &serde_json::Value) -> ToolResult {
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

        ToolResult {
            tool_use_id: String::new(),
            content: msg,
            is_error: false,
        }
    }
}

impl Plugin for EditTool {
    fn name(&self) -> &str {
        "edit"
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
        format!("编辑: {}", path)
    }

    fn system_prompt(&self) -> Option<String> {
        Some(
            "### edit\n\
             Performs exact string replacements in files.\n\n\
             Usage:\n\
             - When editing text from Read tool output, ensure you preserve the exact indentation \
             (tabs/spaces) as it appears AFTER the line number prefix. The line number prefix format is: \
             spaces + line number + →. Everything after that → is the actual file content to match. \
             Never include any part of the line number prefix in the old_string or new_string.\n\
             - ALWAYS prefer editing existing files in the codebase. NEVER write new files unless explicitly required.\n\
             - Only use emojis if the user explicitly requests it. Avoid adding emojis to files unless asked."
                .to_string(),
        )
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
