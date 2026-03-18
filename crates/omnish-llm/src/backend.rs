use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::tool::{ToolCall, ToolDef};

/// Info about an available backend for listing purposes.
#[derive(Debug, Clone)]
pub struct BackendInfo {
    pub name: String,
    pub model: String,
}

/// Use case for LLM requests - determines which model to use
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum UseCase {
    /// Auto-completion - fast, lightweight suggestions
    Completion,
    /// Analysis - deeper context understanding
    #[default]
    Analysis,
    /// Chat mode - conversational interaction
    Chat,
}

/// A block of content in an LLM response.
#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text(String),
    ToolUse(ToolCall),
    Thinking(String),
}

/// Why the LLM stopped generating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub context: String,
    pub query: Option<String>,
    pub trigger: TriggerType,
    pub session_ids: Vec<String>,
    /// Use case for this request - determines which model to use
    pub use_case: UseCase,
    /// Maximum content characters for context (model-specific limit)
    pub max_content_chars: Option<usize>,
    pub conversation: Vec<omnish_protocol::message::ChatTurn>,
    /// Optional system prompt (e.g., chat mode system prompt).
    pub system_prompt: Option<String>,
    /// Whether to enable extended thinking mode (e.g., Claude extended thinking, DeepSeek R1).
    /// None means use backend default. Set to false to disable, true to enable.
    pub enable_thinking: Option<bool>,
    /// Tool definitions to provide to the LLM. Empty means no tools.
    pub tools: Vec<ToolDef>,
    /// Extra messages for agent loop (tool_use + tool_result exchanges).
    /// These are raw serde_json::Value objects appended after conversation + query.
    pub extra_messages: Vec<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub enum TriggerType {
    Manual,
    AutoError,
    AutoPattern,
}

/// Token usage statistics from an LLM API response.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Tokens read from KV cache (Anthropic: cache_read_input_tokens, OpenAI: cached_tokens)
    pub cache_read_input_tokens: u64,
    /// Tokens written to KV cache (Anthropic-specific)
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub model: String,
    /// Token usage statistics from the API response
    pub usage: Option<Usage>,
}

impl LlmResponse {
    /// Extract concatenated text from all Text blocks.
    /// Convenience method for callers that don't use tool-use.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Extract concatenated thinking content (if any).
    pub fn thinking(&self) -> Option<String> {
        let parts: Vec<&str> = self.content.iter().filter_map(|b| match b {
            ContentBlock::Thinking(t) => Some(t.as_str()),
            _ => None,
        }).collect();
        if parts.is_empty() { None } else { Some(parts.join("\n")) }
    }

    /// Extract all tool calls from the response.
    pub fn tool_calls(&self) -> Vec<&ToolCall> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse(tc) => Some(tc),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
pub trait LlmBackend: Send + Sync {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse>;
    fn name(&self) -> &str;
    /// Returns the maximum content characters limit for this backend's model
    fn max_content_chars(&self) -> Option<usize> {
        None
    }
    /// Returns the maximum content characters limit for the given use case
    fn max_content_chars_for_use_case(&self, _use_case: UseCase) -> Option<usize> {
        self.max_content_chars()
    }
    /// List available backends (only meaningful for MultiBackend).
    fn list_backends(&self) -> Vec<BackendInfo> { vec![] }
    /// Default chat backend name.
    fn chat_default_name(&self) -> &str { "" }
    /// Get backend by config name (for per-thread model override).
    fn get_backend_by_name(&self, _name: &str) -> Option<Arc<dyn LlmBackend>> { None }
}
