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

        let messages: Vec<serde_json::Value> = if req.conversation.is_empty() {
            // Existing single-turn behavior
            let user_content = crate::template::build_user_content(
                &req.context,
                req.query.as_deref(),
            );
            vec![serde_json::json!({"role": "user", "content": user_content})]
        } else {
            // Multi-turn: conversation history + current query
            let mut msgs = Vec::new();
            for (i, turn) in req.conversation.iter().enumerate() {
                let content = if i == 0 && !req.context.is_empty() {
                    // Prepend terminal context to first user message
                    format!("Terminal context:\n{}\n\n{}", req.context, turn.content)
                } else {
                    turn.content.clone()
                };
                msgs.push(serde_json::json!({"role": &turn.role, "content": content}));
            }
            // Append current query as final user message
            if let Some(ref q) = req.query {
                msgs.push(serde_json::json!({"role": "user", "content": q}));
            }
            msgs
        };

        // Build request body, conditionally disable thinking if requested
        let mut body_map = serde_json::Map::new();
        body_map.insert("model".to_string(), serde_json::Value::String(self.model.clone()));
        body_map.insert("max_tokens".to_string(), serde_json::Value::Number(1024.into()));
        body_map.insert("messages".to_string(), serde_json::Value::Array(messages));

        // Add system prompt if provided
        if let Some(ref system) = req.system_prompt {
            body_map.insert("system".to_string(), serde_json::Value::String(system.clone()));
        }

        // Add thinking parameter if explicitly disabled
        if req.enable_thinking == Some(false) {
            let mut thinking_map = serde_json::Map::new();
            thinking_map.insert("type".to_string(), serde_json::Value::String("enabled".to_string()));
            thinking_map.insert("disabled_reason".to_string(), serde_json::Value::String("disabled_by_client".to_string()));
            body_map.insert("thinking".to_string(), serde_json::Value::Object(thinking_map));
        }

        let body = serde_json::Value::Object(body_map);

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
