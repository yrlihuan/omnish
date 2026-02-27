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
    let thinking_start = "\n<think>";
    let thinking_end = "</think>";

    if let Some(start) = content.find(thinking_start) {
        if let Some(end) = content.find(thinking_end) {
            let thinking = content[start + thinking_start.len()..end].to_string();
            let cleaned = content[..start].to_string() + &content[end + thinking_end.len()..];
            return (Some(thinking), cleaned.trim().to_string());
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
