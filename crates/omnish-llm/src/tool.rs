use serde::{Deserialize, Serialize};

/// Definition of a tool that can be provided to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// Result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

/// Trait for implementing tools that the LLM can call.
/// New tools are added by implementing this trait and registering at startup.
pub trait Tool: Send + Sync {
    /// Returns the tool definition (name, description, JSON schema) for the LLM.
    fn definition(&self) -> ToolDef;
    /// Executes the tool with the given input and returns the result.
    fn execute(&self, input: &serde_json::Value) -> ToolResult;
}
