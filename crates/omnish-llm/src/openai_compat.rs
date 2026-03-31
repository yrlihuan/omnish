use crate::backend::{ContentBlock, LlmBackend, LlmRequest, LlmResponse, StopReason, Usage};
use crate::tool::ToolCall;
use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;

/// Maximum number of retries for rate-limit (429) errors.
const MAX_RETRIES: u32 = 3;
/// Default backoff duration when no retry-after header is present.
const DEFAULT_BACKOFF: Duration = Duration::from_secs(5);
/// Maximum backoff duration to cap retry-after values.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

pub struct OpenAiCompatBackend {
    pub config_name: String,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub client: reqwest::Client,
    pub max_content_chars: Option<usize>,
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

/// Convert Anthropic-format extra_messages to OpenAI-format messages.
///
/// Anthropic format stores tool interactions as:
///   - assistant: `{"role":"assistant","content":[{"type":"tool_use","id":..,"name":..,"input":..}, {"type":"text","text":..}]}`
///   - user: `{"role":"user","content":[{"type":"tool_result","tool_use_id":..,"content":..}]}`
///
/// OpenAI format uses:
///   - assistant: `{"role":"assistant","tool_calls":[{"id":..,"type":"function","function":{"name":..,"arguments":..}}]}`
///   - tool: `{"role":"tool","tool_call_id":..,"content":..}`
fn convert_extra_messages(extra: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for msg in extra {
        let role = msg["role"].as_str().unwrap_or("");
        match role {
            "assistant" => {
                if let Some(content_arr) = msg["content"].as_array() {
                    let mut text_parts = Vec::new();
                    let mut thinking_parts = Vec::new();
                    let mut tool_calls = Vec::new();
                    for block in content_arr {
                        match block["type"].as_str() {
                            Some("tool_use") => {
                                let mut tc = serde_json::json!({
                                    "id": block["id"],
                                    "type": "function",
                                    "function": {
                                        "name": block["name"],
                                        "arguments": serde_json::to_string(&block["input"]).unwrap_or_default(),
                                    }
                                });
                                // Preserve vendor-specific extra fields (e.g. Gemini thought_signature)
                                if let Some(obj) = block.as_object() {
                                    if let Some(tc_obj) = tc.as_object_mut() {
                                        for (k, v) in obj {
                                            if !matches!(k.as_str(), "type" | "id" | "name" | "input") {
                                                tc_obj.insert(k.clone(), v.clone());
                                            }
                                        }
                                    }
                                }
                                tool_calls.push(tc);
                            }
                            Some("text") => {
                                if let Some(t) = block["text"].as_str() {
                                    if !t.is_empty() {
                                        text_parts.push(t.to_string());
                                    }
                                }
                            }
                            Some("thinking") => {
                                if let Some(t) = block["thinking"].as_str() {
                                    if !t.is_empty() {
                                        thinking_parts.push(t.to_string());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    let mut m = serde_json::Map::new();
                    m.insert("role".into(), serde_json::json!("assistant"));
                    if !text_parts.is_empty() {
                        m.insert("content".into(), serde_json::json!(text_parts.join("\n")));
                    } else {
                        m.insert("content".into(), serde_json::Value::Null);
                    }
                    if !thinking_parts.is_empty() {
                        m.insert("reasoning_content".into(), serde_json::json!(thinking_parts.join("\n")));
                    }
                    if !tool_calls.is_empty() {
                        m.insert("tool_calls".into(), serde_json::json!(tool_calls));
                    }
                    out.push(serde_json::Value::Object(m));
                } else {
                    // Plain text assistant message
                    out.push(msg.clone());
                }
            }
            "user" => {
                if let Some(content_arr) = msg["content"].as_array() {
                    // Convert tool_result blocks to separate "tool" role messages
                    for block in content_arr {
                        if block["type"].as_str() == Some("tool_result") {
                            out.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": block["tool_use_id"],
                                "content": block["content"].as_str().unwrap_or(""),
                            }));
                        }
                    }
                } else {
                    // Plain text user message
                    out.push(msg.clone());
                }
            }
            _ => {
                out.push(msg.clone());
            }
        }
    }
    out
}

#[async_trait]
impl LlmBackend for OpenAiCompatBackend {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let client = &self.client;

        let mut messages: Vec<serde_json::Value> = if req.conversation.is_empty() && req.extra_messages.is_empty() {
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
            // Convert and append extra messages (tool_use/tool_result exchanges)
            msgs.extend(convert_extra_messages(&req.extra_messages));
            msgs
        };

        // Prepend system message if provided
        if let Some(ref system) = req.system_prompt {
            messages.insert(0, serde_json::json!({"role": "system", "content": system}));
        }

        // Build request body
        let mut body_map = serde_json::Map::new();
        body_map.insert("model".to_string(), serde_json::Value::String(self.model.clone()));
        body_map.insert("messages".to_string(), serde_json::Value::Array(messages));

        // Add tools if provided (OpenAI format)
        if !req.tools.is_empty() {
            let tools_json: Vec<serde_json::Value> = req.tools
                .iter()
                .map(|t| serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                }))
                .collect();
            body_map.insert("tools".to_string(), serde_json::Value::Array(tools_json));
        }

        // Add thinking control for models like Qwen3
        if req.enable_thinking == Some(false) {
            let mut chat_template_kwargs = serde_json::Map::new();
            chat_template_kwargs.insert("enable_thinking".to_string(), serde_json::Value::Bool(false));
            body_map.insert("chat_template_kwargs".to_string(), serde_json::Value::Object(chat_template_kwargs));
        }

        let body = serde_json::Value::Object(body_map);
        crate::message_log::log_request(&body, req.use_case);

        /// Parse `retry-after` header value (seconds) from response headers.
        fn parse_retry_after(resp: &reqwest::Response) -> Option<Duration> {
            let val = resp.headers().get("retry-after")?.to_str().ok()?;
            let secs: f64 = val.parse().ok()?;
            Some(Duration::from_secs_f64(secs.min(MAX_BACKOFF.as_secs_f64())))
        }

        // Retry loop for connection errors and 429 (rate limit) errors
        let mut last_error = None;
        for attempt in 0..=MAX_RETRIES {
            let resp = match client
                .post(format!("{}/chat/completions", self.base_url.trim_end_matches('/')))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) if e.is_connect() || e.is_request() => {
                    let backoff = DEFAULT_BACKOFF * 2u32.pow(attempt);
                    let backoff = backoff.min(MAX_BACKOFF);
                    tracing::warn!(
                        "OpenAI API connection error (attempt {}/{}): {} — retrying in {:.1}s",
                        attempt + 1, MAX_RETRIES + 1, e, backoff.as_secs_f64()
                    );
                    last_error = Some(anyhow::anyhow!("OpenAI API connection error: {}", e));
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    return Err(last_error.unwrap());
                }
                Err(e) => return Err(e.into()),
            };

            let status = resp.status();
            let status_code = status.as_u16();

            // Retry on 429 (rate limit)
            if status_code == 429 {
                let backoff = parse_retry_after(&resp)
                    .unwrap_or(DEFAULT_BACKOFF * 2u32.pow(attempt));
                let backoff = backoff.min(MAX_BACKOFF);

                let json: serde_json::Value = resp.json().await.unwrap_or_default();
                let error_msg = json["error"]["message"]
                    .as_str()
                    .unwrap_or("rate limited");
                tracing::warn!(
                    "OpenAI API 429 (attempt {}/{}): {} — retrying in {:.1}s",
                    attempt + 1, MAX_RETRIES + 1, error_msg, backoff.as_secs_f64()
                );
                last_error = Some(anyhow::anyhow!(
                    "OpenAI API error ({}): {}",
                    status, error_msg
                ));

                if attempt < MAX_RETRIES {
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                return Err(last_error.unwrap());
            }

            let resp_text = resp.text().await?;
            let json: serde_json::Value = serde_json::from_str(&resp_text)
                .map_err(|e| {
                    let preview = if resp_text.len() > 1000 {
                        format!("{}...(truncated, total {} bytes)", &resp_text[..1000], resp_text.len())
                    } else {
                        resp_text.clone()
                    };
                    anyhow::anyhow!(
                        "OpenAI API response decode error ({}): {} — body: {}",
                        status, e, preview
                    )
                })?;

            // Check for other API errors
            if !status.is_success() {
                // Try OpenAI format first, then fall back to full body
                let error_msg = json["error"]["message"]
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        let body = resp_text.chars().take(1000).collect::<String>();
                        if resp_text.len() > 1000 {
                            format!("{}...(truncated)", body)
                        } else {
                            body
                        }
                    });
                return Err(anyhow::anyhow!(
                    "OpenAI API error ({}): {}",
                    status,
                    error_msg
                ));
            }

            let message = &json["choices"][0]["message"];

            // Parse finish_reason / stop_reason
            let finish_reason = json["choices"][0]["finish_reason"].as_str().unwrap_or("stop");
            let stop_reason = match finish_reason {
                "tool_calls" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                _ => StopReason::EndTurn,
            };

            // Parse content blocks
            let mut content_blocks = Vec::new();

            // Text content (may contain <think> tags for OpenAI-compat models)
            if let Some(raw_content) = message["content"].as_str() {
                let (think, text) = if req.enable_thinking == Some(false) {
                    (None, raw_content.to_string())
                } else {
                    extract_thinking(raw_content)
                };
                if let Some(thinking) = think {
                    content_blocks.push(ContentBlock::Thinking { thinking, signature: None });
                }
                if !text.is_empty() {
                    content_blocks.push(ContentBlock::Text(text));
                }
            }

            // Tool calls
            if let Some(tool_calls) = message["tool_calls"].as_array() {
                for tc in tool_calls {
                    let id = tc["id"].as_str().unwrap_or("").to_string();
                    let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                    let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                    let input: serde_json::Value = serde_json::from_str(args_str)
                        .unwrap_or(serde_json::json!({}));
                    // Capture vendor-specific extra fields (e.g. Gemini thought_signature)
                    let extra: serde_json::Map<String, serde_json::Value> = tc.as_object()
                        .map(|obj| obj.iter()
                            .filter(|(k, _)| !matches!(k.as_str(), "id" | "type" | "function" | "index"))
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect())
                        .unwrap_or_default();
                    content_blocks.push(ContentBlock::ToolUse(ToolCall { id, name, input, extra }));
                }
            }

            if content_blocks.is_empty() && stop_reason == StopReason::EndTurn {
                return Err(anyhow::anyhow!("Invalid response format: no content blocks found"));
            }

            let usage = json["usage"].as_object().map(|u| Usage {
                input_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                output_tokens: u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                cache_read_input_tokens: u.get("cached_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                cache_creation_input_tokens: 0,
            });

            return Ok(LlmResponse {
                content: content_blocks,
                stop_reason,
                model: self.model.clone(),
                usage,
            });
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("OpenAI API: max retries exhausted")))
    }

    fn name(&self) -> &str {
        &self.config_name
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn max_content_chars(&self) -> Option<usize> {
        self.max_content_chars
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

    #[test]
    fn test_convert_extra_messages_tool_use() {
        let extra = vec![
            serde_json::json!({
                "role": "user",
                "content": "what files are here?"
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Let me check"},
                    {"type": "tool_use", "id": "call_1", "name": "command_query", "input": {"action": "list_history"}}
                ]
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "file1.txt\nfile2.txt"}
                ]
            }),
            serde_json::json!({
                "role": "assistant",
                "content": "Here are the files: file1.txt and file2.txt"
            }),
        ];
        let converted = convert_extra_messages(&extra);
        assert_eq!(converted.len(), 4);

        // Plain user message passes through
        assert_eq!(converted[0]["role"], "user");
        assert_eq!(converted[0]["content"], "what files are here?");

        // Assistant with tool_use → OpenAI format
        assert_eq!(converted[1]["role"], "assistant");
        assert_eq!(converted[1]["content"], "Let me check");
        assert!(converted[1]["tool_calls"].is_array());
        let tc = &converted[1]["tool_calls"][0];
        assert_eq!(tc["id"], "call_1");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "command_query");

        // tool_result → "tool" role message
        assert_eq!(converted[2]["role"], "tool");
        assert_eq!(converted[2]["tool_call_id"], "call_1");
        assert_eq!(converted[2]["content"], "file1.txt\nfile2.txt");

        // Plain assistant message passes through
        assert_eq!(converted[3]["role"], "assistant");
    }

    #[test]
    fn test_convert_extra_messages_thinking_blocks() {
        let extra = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "Let me analyze this..."},
                    {"type": "text", "text": "I'll check the files"},
                    {"type": "tool_use", "id": "call_1", "name": "glob", "input": {"pattern": "*.rs"}}
                ]
            }),
        ];
        let converted = convert_extra_messages(&extra);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["role"], "assistant");
        assert_eq!(converted[0]["content"], "I'll check the files");
        assert_eq!(converted[0]["reasoning_content"], "Let me analyze this...");
        assert!(converted[0]["tool_calls"].is_array());
        assert_eq!(converted[0]["tool_calls"][0]["id"], "call_1");
    }

    #[test]
    fn test_convert_extra_messages_no_text_in_tool_use() {
        let extra = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "call_2", "name": "read_file", "input": {"path": "/tmp/test"}}
                ]
            }),
        ];
        let converted = convert_extra_messages(&extra);
        assert_eq!(converted.len(), 1);
        // content should be null when no text
        assert!(converted[0]["content"].is_null());
        assert_eq!(converted[0]["tool_calls"][0]["id"], "call_2");
    }
}
