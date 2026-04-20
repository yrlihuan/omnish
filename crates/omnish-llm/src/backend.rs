use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::tool::{ToolCall, ToolDef};

/// Info about an available backend for listing purposes.
#[derive(Debug, Clone)]
pub struct BackendInfo {
    pub name: String,
    pub model: String,
}

/// Backend-agnostic cache lifetime hint.
/// Anthropic backend translates this into `cache_control` TTL.
/// OpenAI-compat backend ignores this entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum CacheHint {
    #[default]
    None,
    /// Anthropic: ephemeral with default 5min TTL.
    Short,
    /// Anthropic: ephemeral with `ttl: "1h"`.
    Long,
}

/// A cacheable text payload (used for `LlmRequest.system_prompt`).
#[derive(Debug, Clone)]
pub struct CachedText {
    pub text: String,
    pub cache: CacheHint,
}

/// A message wrapped with a cache hint (used for `LlmRequest.extra_messages`).
/// `content` is raw Anthropic-format JSON (canonical internal format).
///
/// `cache_pos` selects which content block inside the message receives the
/// `cache_control` marker:
/// - `None` (default): last block, preserves legacy behavior
/// - `Some(n)`: the n-th block (0-indexed); out-of-range falls back to last with a warning
#[derive(Debug, Clone, Default)]
pub struct TaggedMessage {
    pub content: serde_json::Value,
    pub cache: CacheHint,
    pub cache_pos: Option<usize>,
}

impl TaggedMessage {
    /// Convenience constructor matching the pre-`cache_pos` two-field shape.
    /// Leaves `cache_pos` as `None` (marks the last block).
    pub fn new(content: serde_json::Value, cache: CacheHint) -> Self {
        Self { content, cache, cache_pos: None }
    }
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
    /// Summarize tool results before feeding back to the conversation
    Summarize,
}

/// A block of content in an LLM response.
#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text(String),
    ToolUse(ToolCall),
    Thinking { thinking: String, signature: Option<String> },
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
    /// Single-turn context. Used only when `extra_messages` is empty;
    /// otherwise ignored - multi-turn callers must fold context into
    /// `system_prompt` (e.g., via system-reminder) or into `extra_messages`.
    pub context: String,
    /// Single-turn query. Used only when `extra_messages` is empty;
    /// otherwise ignored.
    pub query: Option<String>,
    pub trigger: TriggerType,
    pub session_ids: Vec<String>,
    /// Use case for this request - determines which model to use
    pub use_case: UseCase,
    /// Maximum content characters for context (model-specific limit)
    pub max_content_chars: Option<usize>,
    /// Optional system prompt (e.g., chat mode system prompt).
    pub system_prompt: Option<CachedText>,
    /// Whether to enable extended thinking mode (e.g., Claude extended thinking, DeepSeek R1).
    /// None means use backend default. Set to false to disable, true to enable.
    pub enable_thinking: Option<bool>,
    /// Tool definitions to provide to the LLM. Empty means no tools.
    pub tools: Vec<ToolDef>,
    /// Messages for multi-turn / agent loop. Each carries an optional cache hint.
    /// Content is raw Anthropic-format JSON (canonical internal format).
    /// When non-empty, `context` and `query` are ignored.
    pub extra_messages: Vec<TaggedMessage>,
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
            ContentBlock::Thinking { thinking: t, .. } => Some(t.as_str()),
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
    /// Model name of this backend.
    fn model_name(&self) -> &str;
}

/// Fallback backend used when no LLM is configured or initialization fails.
/// All calls to `complete()` return an error.
pub struct UnavailableBackend;

#[async_trait]
impl LlmBackend for UnavailableBackend {
    async fn complete(&self, _req: &LlmRequest) -> Result<LlmResponse> {
        Err(anyhow::anyhow!("LLM backend not configured"))
    }
    fn name(&self) -> &str { "unavailable" }
    fn model_name(&self) -> &str { "unavailable" }
}
