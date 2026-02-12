use crate::backend::{LlmBackend, LlmRequest, LlmResponse};
use anyhow::Result;
use async_trait::async_trait;

pub struct OpenAiCompatBackend {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
}

#[async_trait]
impl LlmBackend for OpenAiCompatBackend {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let client = reqwest::Client::new();

        let user_content = if let Some(query) = &req.query {
            format!(
                "Here is the terminal session context:\n\n```\n{}\n```\n\nUser question: {}",
                req.context, query
            )
        } else {
            format!(
                "Analyze this terminal session output and explain any errors or issues:\n\n```\n{}\n```",
                req.context
            )
        };

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

        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid response format: missing choices[0].message.content"))?
            .to_string();

        Ok(LlmResponse {
            content,
            model: self.model.clone(),
        })
    }

    fn name(&self) -> &str {
        "openai_compat"
    }
}
