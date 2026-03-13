use crate::backend::{ContentBlock, LlmBackend, LlmRequest, LlmResponse, StopReason, Usage};
use crate::tool::ToolCall;
use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;

/// Maximum number of retries for rate-limit (429) and overloaded (529) errors.
const MAX_RETRIES: u32 = 3;
/// Default backoff duration when no retry-after header is present.
const DEFAULT_BACKOFF: Duration = Duration::from_secs(5);
/// Maximum backoff duration to cap retry-after values.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

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

/// Parse `retry-after` header value (seconds) from response headers.
fn parse_retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let val = resp.headers().get("retry-after")?.to_str().ok()?;
    let secs: f64 = val.parse().ok()?;
    Some(Duration::from_secs_f64(secs.min(MAX_BACKOFF.as_secs_f64())))
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
        body_map.insert("max_tokens".to_string(), serde_json::Value::Number(8192.into()));
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

        // Add thinking parameter if explicitly enabled or disabled
        // None means use backend default (no thinking parameter sent)
        if let Some(enabled) = req.enable_thinking {
            if enabled {
                let mut thinking_map = serde_json::Map::new();
                thinking_map.insert("type".to_string(), serde_json::Value::String("enabled".to_string()));
                // Default budget_tokens: 4096 (can be made configurable in the future)
                thinking_map.insert("budget_tokens".to_string(), serde_json::Value::Number(4096.into()));
                body_map.insert("thinking".to_string(), serde_json::Value::Object(thinking_map));
            } else {
                let mut thinking_map = serde_json::Map::new();
                thinking_map.insert("type".to_string(), serde_json::Value::String("enabled".to_string()));
                thinking_map.insert("disabled_reason".to_string(), serde_json::Value::String("disabled_by_client".to_string()));
                body_map.insert("thinking".to_string(), serde_json::Value::Object(thinking_map));
            }
        }

        let body = serde_json::Value::Object(body_map);
        crate::message_log::log_request(&body, req.use_case);

        // Retry loop for 429 (rate limit) and 529 (overloaded) errors
        let mut last_error = None;
        for attempt in 0..=MAX_RETRIES {
            let resp = client
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2024-04-04")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await?;

            let status = resp.status();
            let status_code = status.as_u16();

            // Retry on 429 (rate limit) or 529 (overloaded)
            if status_code == 429 || status_code == 529 {
                let backoff = parse_retry_after(&resp)
                    .unwrap_or(DEFAULT_BACKOFF * 2u32.pow(attempt));
                let backoff = backoff.min(MAX_BACKOFF);

                let json: serde_json::Value = resp.json().await.unwrap_or_default();
                let error_msg = json["error"]["message"]
                    .as_str()
                    .unwrap_or("rate limited");
                tracing::warn!(
                    "Anthropic API {} (attempt {}/{}): {} — retrying in {:.1}s",
                    status_code, attempt + 1, MAX_RETRIES + 1, error_msg, backoff.as_secs_f64()
                );
                last_error = Some(anyhow::anyhow!(
                    "Anthropic API error ({}): {}",
                    status, error_msg
                ));

                if attempt < MAX_RETRIES {
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                // Final attempt exhausted — fall through to return error
                return Err(last_error.unwrap());
            }

            let json: serde_json::Value = resp.json().await?;

            // Check for other API errors
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

            let usage = json["usage"].as_object().map(|u| Usage {
                input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                cache_read_input_tokens: u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                cache_creation_input_tokens: u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            });

            return Ok(LlmResponse {
                content: content_blocks,
                stop_reason,
                model: self.model.clone(),
                thinking,
                usage,
            });
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Anthropic API: max retries exhausted")))
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}
