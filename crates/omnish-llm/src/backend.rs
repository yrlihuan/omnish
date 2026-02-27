use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Use case for LLM requests - determines which model to use
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UseCase {
    /// Auto-completion - fast, lightweight suggestions
    Completion,
    /// Analysis - deeper context understanding
    Analysis,
    /// Chat mode - conversational interaction
    Chat,
}

impl Default for UseCase {
    fn default() -> Self {
        UseCase::Analysis
    }
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
}

#[derive(Debug, Clone)]
pub enum TriggerType {
    Manual,
    AutoError,
    AutoPattern,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
    pub model: String,
    /// Thinking content from models that support it (e.g., o1, Claude with extended thinking)
    pub thinking: Option<String>,
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
    /// Default implementation returns max_content_chars() for backwards compatibility
    fn max_content_chars_for_use_case(&self, _use_case: UseCase) -> Option<usize> {
        self.max_content_chars()
    }
}
