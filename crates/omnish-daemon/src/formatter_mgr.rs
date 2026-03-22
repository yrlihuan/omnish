use omnish_plugin::formatter::{
    DefaultFormatter, EditFormatter, FormatInput, FormatOutput, ReadFormatter, ToolFormatter,
};
use std::collections::HashMap;

pub struct FormatterManager {
    builtins: HashMap<String, Box<dyn ToolFormatter>>,
}

impl FormatterManager {
    pub fn new() -> Self {
        let mut builtins: HashMap<String, Box<dyn ToolFormatter>> = HashMap::new();
        builtins.insert("default".into(), Box::new(DefaultFormatter));
        builtins.insert("read".into(), Box::new(ReadFormatter));
        builtins.insert("edit".into(), Box::new(EditFormatter));
        builtins.insert("write".into(), Box::new(EditFormatter));
        Self { builtins }
    }

    pub async fn format(&self, formatter_name: &str, input: &FormatInput) -> FormatOutput {
        let fmt = self
            .builtins
            .get(formatter_name)
            .or_else(|| self.builtins.get("default"))
            .unwrap();
        fmt.format(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_builtin_formatter_default() {
        let mgr = FormatterManager::new();
        let input = FormatInput {
            tool_name: "unknown_tool".into(),
            params: serde_json::json!({}),
            output: "hello\nworld".into(),
            is_error: false,
        };
        let out = mgr.format("default", &input).await;
        assert!(!out.result_compact.is_empty());
    }

    #[tokio::test]
    async fn test_builtin_formatter_edit() {
        let mgr = FormatterManager::new();
        let input = FormatInput {
            tool_name: "edit".into(),
            params: serde_json::json!({"file_path": "/tmp/test.txt", "old_string": "hello", "new_string": "goodbye"}),
            output: "Edited /tmp/test.txt\n---\n1:  before\n2:-hello\n2:+goodbye\n3:  after".into(),
            is_error: false,
        };
        let out = mgr.format("edit", &input).await;
        assert!(out.result_compact[0].contains("Edited 1 line"));
    }

    #[tokio::test]
    async fn test_unknown_formatter_falls_back_to_default() {
        let mgr = FormatterManager::new();
        let input = FormatInput {
            tool_name: "test".into(),
            params: serde_json::json!({}),
            output: "some output".into(),
            is_error: false,
        };
        let out = mgr.format("nonexistent", &input).await;
        assert!(!out.result_compact.is_empty());
    }
}
