use crate::backend::{CacheHint, ContentBlock, LlmBackend, LlmRequest, LlmResponse, StopReason, Usage};
use crate::tool::ToolCall;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;
use std::time::Duration;

/// Maximum number of retries for rate-limit (429) and overloaded (529) errors.
const MAX_RETRIES: u32 = 3;
/// Default backoff duration when no retry-after header is present.
const DEFAULT_BACKOFF: Duration = Duration::from_secs(5);
/// Maximum backoff duration to cap retry-after values.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Maximum cache_control breakpoints in a single Anthropic request.
const MAX_CACHE_BREAKPOINTS: usize = 4;

pub struct AnthropicBackend {
    pub config_name: String,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub client: reqwest::Client,
    pub max_content_chars: Option<usize>,
}

/// Strip thinking tags from LLM response content.
fn strip_thinking(content: &str) -> String {
    content.replace("\n<think>", "").replace("</think>", "")
}

/// Claude Opus 4.7 dropped manual extended-thinking (`budget_tokens`) mode and
/// now requires `thinking: {type: "adaptive"}`. Returns true for 4.7 and any
/// later Opus release named `claude-opus-<major>-<minor>[-...]` whose version
/// is 4.7 or greater. Unknown or non-Opus models return false so they keep the
/// manual shape that older Claude 4 models and Anthropic-compat providers expect.
fn is_opus_4_7_or_later(model: &str) -> bool {
    let Some(rest) = model.strip_prefix("claude-opus-") else {
        return false;
    };
    let mut parts = rest.split('-');
    let (Some(major), Some(minor)) = (parts.next(), parts.next()) else {
        return false;
    };
    let Ok(major) = major.parse::<u32>() else { return false };
    let Ok(minor) = minor.parse::<u32>() else { return false };
    (major, minor) >= (4, 7)
}

/// Render a `CacheHint` into Anthropic's `cache_control` JSON object.
/// Returns `None` for `CacheHint::None` (no field should be emitted).
fn cache_control_value(hint: CacheHint) -> Option<serde_json::Value> {
    match hint {
        CacheHint::None => None,
        CacheHint::Short => Some(serde_json::json!({"type": "ephemeral"})),
        CacheHint::Long => Some(serde_json::json!({"type": "ephemeral", "ttl": "1h"})),
    }
}

/// Apply a cache hint to a specific content block of a message JSON value.
/// `pos` selects the block index (`None` = last block). Out-of-range `pos`
/// falls back to the last block with a warning.
/// Handles both string content (converted to array form) and array content.
/// No-op if hint is `None` or content shape is empty.
fn apply_cache_hint_to_message(msg: &mut serde_json::Value, hint: CacheHint, pos: Option<usize>) {
    let Some(cc) = cache_control_value(hint) else { return };
    match msg.get("content").cloned() {
        Some(serde_json::Value::String(s)) => {
            msg["content"] = serde_json::json!([
                {"type": "text", "text": s, "cache_control": cc}
            ]);
        }
        Some(serde_json::Value::Array(arr)) if !arr.is_empty() => {
            let mut new_arr = arr;
            let last = new_arr.len() - 1;
            let idx = match pos {
                None => last,
                Some(p) if p <= last => p,
                Some(p) => {
                    tracing::warn!(
                        "cache_pos {} out of range (content has {} blocks), falling back to last",
                        p, new_arr.len()
                    );
                    last
                }
            };
            new_arr[idx]["cache_control"] = cc;
            msg["content"] = serde_json::Value::Array(new_arr);
        }
        _ => {}
    }
}

/// Compute effective per-message cache hints after applying the breakpoint budget.
/// Anthropic supports up to 4 cache_control breakpoints per request.
/// Strategy: count static breakpoints (system + tools), give the remainder to
/// messages, retain only the last N marked messages, downgrade the rest to None.
fn enforce_breakpoint_budget(req: &LlmRequest) -> Vec<CacheHint> {
    let used_static = req.tools.iter().filter(|t| t.cache != CacheHint::None).count()
        + req.system_prompt.as_ref().map_or(0, |s| (s.cache != CacheHint::None) as usize);
    let remaining = MAX_CACHE_BREAKPOINTS.saturating_sub(used_static);

    let marked: Vec<usize> = req
        .extra_messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.cache != CacheHint::None)
        .map(|(i, _)| i)
        .collect();

    if marked.len() > remaining {
        tracing::warn!(
            "cache breakpoint budget exceeded: {} static + {} message hints, \
             dropping {} earliest message hints (max breakpoints = {})",
            used_static,
            marked.len(),
            marked.len() - remaining,
            MAX_CACHE_BREAKPOINTS
        );
    }

    let kept: HashSet<usize> = marked.iter().rev().take(remaining).copied().collect();

    req.extra_messages
        .iter()
        .enumerate()
        .map(|(i, m)| if kept.contains(&i) { m.cache } else { CacheHint::None })
        .collect()
}

fn build_request_body(req: &LlmRequest, model: &str) -> serde_json::Value {
    // Build messages array from extra_messages (or single-turn fallback).
    let mut messages: Vec<serde_json::Value> = if req.extra_messages.is_empty() {
        if !req.context.is_empty() && req.query.is_some() {
            // Completion-style single-turn: context + query as separate content blocks.
            // cache_control on the context block keeps the stable prefix cached
            // across requests with varying query input.
            let query = req.query.as_deref().unwrap();
            vec![serde_json::json!({"role": "user", "content": [
                {"type": "text", "text": req.context, "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": query},
            ]})]
        } else {
            let user_content =
                crate::template::build_user_content(&req.context, req.query.as_deref());
            vec![serde_json::json!({"role": "user", "content": user_content})]
        }
    } else {
        req.extra_messages.iter().map(|m| m.content.clone()).collect()
    };

    // Apply per-message cache hints (after budget enforcement).
    if !req.extra_messages.is_empty() {
        let effective_hints = enforce_breakpoint_budget(req);
        for ((msg, hint), tagged) in messages
            .iter_mut()
            .zip(effective_hints.iter().copied())
            .zip(req.extra_messages.iter())
        {
            apply_cache_hint_to_message(msg, hint, tagged.cache_pos);
        }
    }

    let mut body_map = serde_json::Map::new();
    body_map.insert("model".to_string(), serde_json::Value::String(model.to_string()));
    body_map.insert("max_tokens".to_string(), serde_json::Value::Number(8192.into()));
    body_map.insert("messages".to_string(), serde_json::Value::Array(messages));

    // System prompt: optional cache_control based on hint.
    if let Some(ref system) = req.system_prompt {
        let mut block = serde_json::json!({"type": "text", "text": system.text});
        if let Some(cc) = cache_control_value(system.cache) {
            block["cache_control"] = cc;
        }
        body_map.insert("system".to_string(), serde_json::Value::Array(vec![block]));
    }

    // Tools: per-tool cache_control based on each tool's hint.
    if !req.tools.is_empty() {
        let tools_json: Vec<serde_json::Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut entry = serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                });
                if let Some(cc) = cache_control_value(t.cache) {
                    entry["cache_control"] = cc;
                }
                entry
            })
            .collect();
        body_map.insert("tools".to_string(), serde_json::Value::Array(tools_json));
    }

    // Translate enable_thinking to an explicit wire value. We avoid relying on
    // backend defaults because Anthropic defaults to off when the field is
    // omitted while DeepSeek's Anthropic-compat endpoint defaults to on; the
    // daemon's Some(false) call sites must disable thinking on both.
    //   Some(true)  + Opus 4.7+ -> {type: "adaptive"}
    //                              (manual budget_tokens mode returns 400)
    //   Some(true)  + others    -> {type: "enabled", budget_tokens: 4096}
    //   Some(false)             -> {type: "disabled"}
    //   None                    -> omit (backend default)
    match req.enable_thinking {
        Some(true) if is_opus_4_7_or_later(model) => {
            body_map.insert(
                "thinking".to_string(),
                serde_json::json!({"type": "adaptive"}),
            );
        }
        Some(true) => {
            body_map.insert(
                "thinking".to_string(),
                serde_json::json!({"type": "enabled", "budget_tokens": 4096}),
            );
        }
        Some(false) => {
            body_map.insert(
                "thinking".to_string(),
                serde_json::json!({"type": "disabled"}),
            );
        }
        None => {}
    }

    serde_json::Value::Object(body_map)
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

        let body = build_request_body(req, &self.model);
        crate::message_log::log_request(&body, req.use_case);

        // Retry loop for connection errors, 429 (rate limit) and 529 (overloaded)
        let mut last_error = None;
        for attempt in 0..=MAX_RETRIES {
            let resp = match client
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2024-04-04")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) if e.is_connect() || e.is_request() => {
                    let backoff = DEFAULT_BACKOFF * 2u32.pow(attempt);
                    let backoff = backoff.min(MAX_BACKOFF);
                    tracing::warn!(
                        "Anthropic API connection error (attempt {}/{}): {} - retrying in {:.1}s",
                        attempt + 1, MAX_RETRIES + 1, e, backoff.as_secs_f64()
                    );
                    last_error = Some(anyhow::anyhow!("Anthropic API connection error: {}", e));
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
                    "Anthropic API {} (attempt {}/{}): {} - retrying in {:.1}s",
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
                // Final attempt exhausted - fall through to return error
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
                        "Anthropic API response decode error ({}): {} - body: {}",
                        status, e, preview
                    )
                })?;

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

            // Extract content blocks - preserve original order (including thinking)
            let mut content_blocks = Vec::new();

            for block in json["content"].as_array().unwrap_or(&vec![]) {
                match block["type"].as_str() {
                    Some("thinking") => {
                        if let Some(text) = block["thinking"].as_str() {
                            let signature = block["signature"].as_str().map(|s| s.to_string());
                            content_blocks.push(ContentBlock::Thinking {
                                thinking: text.to_string(),
                                signature,
                            });
                        }
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
                        let extra: serde_json::Map<String, serde_json::Value> = block.as_object()
                            .map(|obj| obj.iter()
                                .filter(|(k, _)| !matches!(k.as_str(), "type" | "id" | "name" | "input"))
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect())
                            .unwrap_or_default();
                        content_blocks.push(ContentBlock::ToolUse(ToolCall { id, name, input, extra }));
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
                usage,
            });
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Anthropic API: max retries exhausted")))
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
    use super::{build_request_body, is_opus_4_7_or_later};
    use crate::backend::{CacheHint, CachedText, LlmRequest, TaggedMessage, TriggerType, UseCase};
    use crate::tool::ToolDef;

    #[test]
    fn thinking_enabled_manual_emits_budget_tokens() {
        let mut req = empty_req();
        req.enable_thinking = Some(true);
        let body = build_request_body(&req, "claude-opus-4-6");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 4096);
    }

    #[test]
    fn thinking_enabled_on_opus_4_7_emits_adaptive() {
        let mut req = empty_req();
        req.enable_thinking = Some(true);
        let body = build_request_body(&req, "claude-opus-4-7");
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert!(
            body["thinking"].get("budget_tokens").is_none(),
            "adaptive mode must not send budget_tokens"
        );
    }

    #[test]
    fn thinking_disabled_emits_explicit_disabled() {
        let mut req = empty_req();
        req.enable_thinking = Some(false);
        let body = build_request_body(&req, "claude-opus-4-6");
        assert_eq!(body["thinking"]["type"], "disabled");
    }

    #[test]
    fn thinking_none_omits_field() {
        let req = empty_req();
        let body = build_request_body(&req, "claude-opus-4-6");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn opus_version_detection() {
        assert!(!is_opus_4_7_or_later("claude-opus-4-6"));
        assert!(!is_opus_4_7_or_later("claude-sonnet-4-6"));
        assert!(!is_opus_4_7_or_later("deepseek-chat"));
        assert!(is_opus_4_7_or_later("claude-opus-4-7"));
        assert!(is_opus_4_7_or_later("claude-opus-4-8"));
        assert!(is_opus_4_7_or_later("claude-opus-5-0"));
        assert!(is_opus_4_7_or_later("claude-opus-4-10"));
    }

    fn empty_req() -> LlmRequest {
        LlmRequest {
            context: String::new(),
            query: None,
            trigger: TriggerType::Manual,
            session_ids: vec![],
            use_case: UseCase::Chat,
            max_content_chars: None,
            system_prompt: None,
            enable_thinking: None,
            tools: vec![],
            extra_messages: vec![],
        }
    }

    #[test]
    fn anthropic_system_long_emits_1h_ttl() {
        let mut req = empty_req();
        req.system_prompt = Some(CachedText {
            text: "you are helpful".into(),
            cache: CacheHint::Long,
        });
        req.extra_messages = vec![TaggedMessage {
            content: serde_json::json!({"role":"user","content":"hi"}),
            cache: CacheHint::None,
            cache_pos: None,
        }];
        let body = build_request_body(&req, "test-model");
        let sys_block = &body["system"][0];
        assert_eq!(sys_block["text"], "you are helpful");
        assert_eq!(sys_block["cache_control"]["type"], "ephemeral");
        assert_eq!(sys_block["cache_control"]["ttl"], "1h");
    }

    #[test]
    fn anthropic_system_short_omits_ttl() {
        let mut req = empty_req();
        req.system_prompt = Some(CachedText {
            text: "sys".into(),
            cache: CacheHint::Short,
        });
        req.extra_messages = vec![TaggedMessage {
            content: serde_json::json!({"role":"user","content":"hi"}),
            cache: CacheHint::None,
            cache_pos: None,
        }];
        let body = build_request_body(&req, "test-model");
        let cc = &body["system"][0]["cache_control"];
        assert_eq!(cc["type"], "ephemeral");
        assert!(cc.get("ttl").is_none(), "Short hint must not emit ttl field, got {:?}", cc);
    }

    #[test]
    fn anthropic_system_none_emits_no_cache_control() {
        let mut req = empty_req();
        req.system_prompt = Some(CachedText {
            text: "sys".into(),
            cache: CacheHint::None,
        });
        req.extra_messages = vec![TaggedMessage {
            content: serde_json::json!({"role":"user","content":"hi"}),
            cache: CacheHint::None,
            cache_pos: None,
        }];
        let body = build_request_body(&req, "test-model");
        assert!(body["system"][0].get("cache_control").is_none());
    }

    #[test]
    fn anthropic_message_cache_marks_last_block() {
        let mut req = empty_req();
        req.extra_messages = vec![
            TaggedMessage {
                content: serde_json::json!({"role":"user","content":"a"}),
                cache: CacheHint::None,
                cache_pos: None,
            },
            TaggedMessage {
                content: serde_json::json!({"role":"user","content":"b"}),
                cache: CacheHint::Long,
                cache_pos: None,
            },
        ];
        let body = build_request_body(&req, "test-model");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        // first message: no cache_control anywhere
        assert!(messages[0]["content"].as_array().is_none() || messages[0]["content"][0].get("cache_control").is_none());
        // second message: content was a string, becomes array with cache_control on the (single) block
        let last_block = &messages[1]["content"][0];
        assert_eq!(last_block["cache_control"]["type"], "ephemeral");
        assert_eq!(last_block["cache_control"]["ttl"], "1h");
    }

    #[test]
    fn anthropic_tool_cache_marks_that_tool() {
        let mut req = empty_req();
        req.tools = vec![
            ToolDef {
                name: "a".into(), description: "ad".into(),
                input_schema: serde_json::json!({"type":"object"}),
                cache: CacheHint::None,
            },
            ToolDef {
                name: "b".into(), description: "bd".into(),
                input_schema: serde_json::json!({"type":"object"}),
                cache: CacheHint::Long,
            },
        ];
        req.extra_messages = vec![TaggedMessage {
            content: serde_json::json!({"role":"user","content":"x"}),
            cache: CacheHint::None,
            cache_pos: None,
        }];
        let body = build_request_body(&req, "test-model");
        let tools = body["tools"].as_array().unwrap();
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
        assert_eq!(tools[1]["cache_control"]["ttl"], "1h");
    }

    #[test]
    fn anthropic_budget_keeps_last_n_message_marks() {
        let mut req = empty_req();
        req.system_prompt = Some(CachedText {
            text: "s".into(), cache: CacheHint::Long,
        });
        req.tools = vec![ToolDef {
            name: "t".into(), description: "d".into(),
            input_schema: serde_json::json!({"type":"object"}),
            cache: CacheHint::Long,
        }];
        // 5 marked messages, budget = 4 - 2 = 2 remaining → keep last 2 (indices 3,4)
        req.extra_messages = (0..5).map(|i| TaggedMessage {
            content: serde_json::json!({"role":"user","content": format!("m{}", i)}),
            cache: CacheHint::Long,
            cache_pos: None,
        }).collect();

        let body = build_request_body(&req, "test-model");
        let messages = body["messages"].as_array().unwrap();
        let has_cache = |idx: usize| -> bool {
            let c = &messages[idx]["content"];
            if let Some(arr) = c.as_array() {
                arr.iter().any(|b: &serde_json::Value| b.get("cache_control").is_some())
            } else {
                false
            }
        };
        assert!(!has_cache(0), "msg 0 should be dropped");
        assert!(!has_cache(1), "msg 1 should be dropped");
        assert!(!has_cache(2), "msg 2 should be dropped");
        assert!(has_cache(3), "msg 3 should be kept");
        assert!(has_cache(4), "msg 4 should be kept");
    }
}
