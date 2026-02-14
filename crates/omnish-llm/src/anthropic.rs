use crate::backend::{LlmBackend, LlmRequest, LlmResponse};
use anyhow::Result;
use async_trait::async_trait;

pub struct AnthropicBackend {
    pub model: String,
    pub api_key: String,
}

#[async_trait]
impl LlmBackend for AnthropicBackend {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let client = reqwest::Client::new();

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

        let content = json["content"][0]["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid response format: missing content[0].text"))?
            .to_string();

        Ok(LlmResponse {
            content,
            model: self.model.clone(),
        })
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}
