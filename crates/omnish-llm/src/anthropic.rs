use crate::backend::{ContentBlock, LlmBackend, LlmRequest, LlmResponse, StopReason};
use crate::tool::ToolCall;
use anyhow::Result;
use async_trait::async_trait;

pub struct AnthropicBackend {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
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

        let messages: Vec<serde_json::Value> = if req.conversation.is_empty() && req.extra_messages.is_empty() {
            // Existing single-turn behavior
            let user_content = crate::template::build_user_content(
                &req.context,
                req.query.as_deref(),
            );
            vec![serde_json::json!({"role": "user", "content": user_content})]
        } else {
            // Multi-turn: conversation history + current query + extra (tool) messages
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
            // Append current query as user message (before extra messages on first call)
            if req.extra_messages.is_empty() {
                if let Some(ref q) = req.query {
                    msgs.push(serde_json::json!({"role": "user", "content": q}));
                }
            }
            // Append extra messages (tool_use assistant + tool_result user exchanges)
            msgs.extend(req.extra_messages.clone());
            msgs
        };

        // Build request body
        let mut body_map = serde_json::Map::new();
        body_map.insert("model".to_string(), serde_json::Value::String(self.model.clone()));
        body_map.insert("max_tokens".to_string(), serde_json::Value::Number(4096.into()));
        body_map.insert("messages".to_string(), serde_json::Value::Array(messages));

        // Add system prompt if provided
        if let Some(ref system) = req.system_prompt {
            body_map.insert("system".to_string(), serde_json::Value::String(system.clone()));
        }

        // Add tools if provided
        if !req.tools.is_empty() {
            let tools_json: Vec<serde_json::Value> = req.tools
                .iter()
                .map(|t| serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                }))
                .collect();
            body_map.insert("tools".to_string(), serde_json::Value::Array(tools_json));
        }

        // Add thinking parameter if explicitly disabled
        if req.enable_thinking == Some(false) {
            let mut thinking_map = serde_json::Map::new();
            thinking_map.insert("type".to_string(), serde_json::Value::String("enabled".to_string()));
            thinking_map.insert("disabled_reason".to_string(), serde_json::Value::String("disabled_by_client".to_string()));
            body_map.insert("thinking".to_string(), serde_json::Value::Object(thinking_map));
        }

        let body = serde_json::Value::Object(body_map);
        crate::message_log::log_request(&body);

        let resp = client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2024-04-04")
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

        // Parse stop_reason
        let stop_reason = match json["stop_reason"].as_str() {
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::MaxTokens,
            _ => StopReason::EndTurn,
        };

        // Extract content blocks
        let mut thinking: Option<String> = None;
        let mut content_blocks = Vec::new();

        for block in json["content"].as_array().unwrap_or(&vec![]) {
            match block["type"].as_str() {
                Some("thinking") => {
                    thinking = block["thinking"].as_str().map(|s| s.to_string());
                }
                Some("text") => {
                    let text = strip_thinking(block["text"].as_str().unwrap_or(""));
                    if !text.is_empty() {
                        content_blocks.push(ContentBlock::Text(text));
                    }
                }
                Some("tool_use") => {
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    let name = block["name"].as_str().unwrap_or("").to_string();
                    let input = block["input"].clone();
                    content_blocks.push(ContentBlock::ToolUse(ToolCall { id, name, input }));
                }
                _ => {}
            }
        }

        if content_blocks.is_empty() && stop_reason == StopReason::EndTurn {
            return Err(anyhow::anyhow!("Invalid response format: no content blocks found"));
        }

        Ok(LlmResponse {
            content: content_blocks,
            stop_reason,
            model: self.model.clone(),
            thinking,
        })
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}
