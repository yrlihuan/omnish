use serde::{Deserialize, Serialize};

/// Definition of a tool that can be provided to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// Cache hint applied to this tool's wire entry. Defaults to `None`
    /// so callers (and JSON-deserialized plugin defs) need not specify it.
    #[serde(default)]
    pub cache: crate::backend::CacheHint,
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    /// Vendor-specific extra fields (e.g. Gemini `thought_signature`).
    /// Automatically captured via `#[serde(flatten)]` during JSONL roundtrip.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

