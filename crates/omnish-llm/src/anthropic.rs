use crate::backend::{LlmBackend, LlmRequest, LlmResponse};
use anyhow::Result;
use async_trait::async_trait;

pub struct AnthropicBackend {
    pub model: String,
    pub api_key: String,
    pub client: reqwest::Client,
}

/// Strip thinking tags from LLM response content.
fn strip_thinking(content: &str) -> String {
    content.replace("\n<think>", "").replace("</think>", "")
}

#[async_trait]
impl LlmBackend for AnthropicBackend {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let client = &self.client;

        let user_content = crate::template::build_user_content(
            &req.context,
            req.query.as_deref(),
        );

        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 1024,
            "messages": [{
                "role": "user",
                "content": user_content
            }]
        });

        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
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
            let error_type = json["error"]["type"]
                .as_str()
                .unwrap_or("unknown");
            return Err(anyhow::anyhow!(
                "Anthropic API error ({}): {} - {}",
                status,
                error_type,
                error_msg
            ));
        }

        // Extract thinking and text content from response
        // Anthropic returns content as an array of blocks
        let mut thinking: Option<String> = None;
        let mut text_content = String::new();

        for block in json["content"].as_array().unwrap_or(&vec![]) {
            match block["type"].as_str() {
                Some("thinking") => {
                    thinking = block["thinking"].as_str().map(|s| s.to_string());
                }
                Some("text") => {
                    if !text_content.is_empty() {
                        text_content.push('\n');
                    }
                    text_content.push_str(block["text"].as_str().unwrap_or(""));
                }
                _ => {}
            }
        }

        if text_content.is_empty() {
            return Err(anyhow::anyhow!("Invalid response format: no text content found"));
        }

        // Strip thinking tags from text content (for backwards compatibility)
        let content = strip_thinking(&text_content);

        Ok(LlmResponse {
            content,
            model: self.model.clone(),
            thinking,
        })
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}
