use crate::backend::{LlmBackend, LlmRequest, LlmResponse};
use anyhow::Result;
use async_trait::async_trait;

pub struct OpenAiCompatBackend {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub client: reqwest::Client,
}

/// Extract thinking from content and return (thinking, cleaned_content)
fn extract_thinking(content: &str) -> (Option<String>, String) {
    let trimmed = content.trim_start();
    let tag_start = "<think>";
    let tag_end = "</think>";

    if let Some(start) = trimmed.find(tag_start) {
        if let Some(end) = trimmed[start..].find(tag_end) {
            let thinking = trimmed[start + tag_start.len()..start + end].trim().to_string();
            let before = trimmed[..start].to_string();
            let after = trimmed[start + end + tag_end.len()..].to_string();
            let cleaned = (before + &after).trim().to_string();
            let thinking = if thinking.is_empty() { None } else { Some(thinking) };
            return (thinking, cleaned);
        }
    }
    (None, content.to_string())
}

#[async_trait]
impl LlmBackend for OpenAiCompatBackend {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let client = &self.client;

        let user_content = crate::template::build_user_content(
            &req.context,
            req.query.as_deref(),
        );

        let body = serde_json::json!({
            "model": self.model,
            "messages": [{
                "role": "user",
                "content": user_content
            }]
        });

        let resp = client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let json: serde_json::Value = resp.json().await?;

        // Check for API errors
        if !status.is_success() {
            let error_msg = json["error"]["message"]
                .as_str()
                .unwrap_or("Unknown API error");
            return Err(anyhow::anyhow!(
                "OpenAI API error ({}): {}",
                status,
                error_msg
            ));
        }

        let raw_content = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid response format: missing choices[0].message.content"))?
            .to_string();

        // Extract thinking from content
        let (thinking, content) = extract_thinking(&raw_content);

        Ok(LlmResponse {
            content,
            model: self.model.clone(),
            thinking,
        })
    }

    fn name(&self) -> &str {
        "openai_compat"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_thinking_with_thinking_tags() {
        let input = "\n<think>\nThe user wants to run a command.\n</think>\nYou can run it with: cargo build";
        let (thinking, content) = extract_thinking(input);

        assert!(thinking.is_some());
        assert_eq!(thinking.unwrap(), "The user wants to run a command.");
        assert_eq!(content, "You can run it with: cargo build");
    }

    #[test]
    fn test_extract_thinking_without_thinking_tags() {
        let input = "Just a plain response without thinking.";
        let (thinking, content) = extract_thinking(input);

        assert!(thinking.is_none());
        assert_eq!(content, "Just a plain response without thinking.");
    }

    #[test]
    fn test_extract_thinking_only_thinking_no_content() {
        let input = "\n<think>\nOnly thinking here.\n</think>";
        let (thinking, content) = extract_thinking(input);

        assert!(thinking.is_some());
        assert_eq!(thinking.unwrap(), "Only thinking here.");
        assert!(content.is_empty());
    }

    #[test]
    fn test_extract_thinking_multiple_thinking_blocks() {
        // Only the first thinking block is extracted
        let input = "<think>\nFirst thinking.\n</think>\nContent\n</think>\nSecond thinking.";
        let (thinking, content) = extract_thinking(input);

        assert!(thinking.is_some());
        assert_eq!(thinking.unwrap(), "First thinking.");
        assert_eq!(content, "Content\n</think>\nSecond thinking.");
    }

    #[test]
    fn test_extract_thinking_empty_input() {
        let input = "";
        let (thinking, content) = extract_thinking(input);

        assert!(thinking.is_none());
        assert_eq!(content, "");
    }

    #[test]
    fn test_extract_thinking_thinking_at_end() {
        let input = "Some content\n<think>\nThinking at end\n</think>";
        let (thinking, content) = extract_thinking(input);

        assert!(thinking.is_some());
        assert_eq!(thinking.unwrap(), "Thinking at end");
        assert_eq!(content, "Some content");
    }

    #[test]
    fn test_extract_thinking_starts_with_think_no_newline() {
        let input = "<think>\nDeepSeek thinking here.\n</think>\nThe answer is 42.";
        let (thinking, content) = extract_thinking(input);

        assert!(thinking.is_some());
        assert_eq!(thinking.unwrap(), "DeepSeek thinking here.");
        assert_eq!(content, "The answer is 42.");
    }

    #[test]
    fn test_extract_thinking_empty_think_block() {
        let input = "<think>\n</think>\nSome content";
        let (thinking, content) = extract_thinking(input);

        assert!(thinking.is_none());
        assert_eq!(content, "Some content");
    }
}
