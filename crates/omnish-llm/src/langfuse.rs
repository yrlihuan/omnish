//! Langfuse observability integration.
//!
//! Wraps an `LlmBackend` to report traces and generations to a Langfuse
//! instance via the `/api/public/ingestion` batch API.

use crate::backend::{LlmBackend, LlmRequest, LlmResponse, UseCase};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

/// Configuration for Langfuse integration.
#[derive(Debug, Clone)]
pub struct LangfuseConfig {
    pub public_key: String,
    pub secret_key: String,
    pub host: String,
}

/// Wrapper backend that sends traces to Langfuse after each LLM call.
pub struct LangfuseBackend {
    inner: Arc<dyn LlmBackend>,
    config: LangfuseConfig,
    client: Client,
}

impl LangfuseBackend {
    pub fn wrap(inner: Arc<dyn LlmBackend>, config: LangfuseConfig) -> Arc<dyn LlmBackend> {
        let client = Client::new();
        Arc::new(Self {
            inner,
            config,
            client,
        })
    }
}

#[async_trait]
impl LlmBackend for LangfuseBackend {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let start = Instant::now();
        let start_time = chrono::Utc::now();
        let result = self.inner.complete(req).await;
        let duration = start.elapsed();
        let end_time = chrono::Utc::now();

        // Fire-and-forget: send trace to Langfuse in background
        let config = self.config.clone();
        let client = self.client.clone();
        let model = self.inner.name().to_string();
        let use_case = format!("{:?}", req.use_case);
        let input = build_langfuse_input(req);
        let session_id = req.session_ids.first().cloned();
        let (output_text, is_error, tool_count, usage) = match &result {
            Ok(resp) => {
                let text = resp.text();
                let tools = resp.tool_calls().len();
                let usage = resp.usage.clone();
                (text, false, tools, usage)
            }
            Err(e) => (e.to_string(), true, 0, None),
        };
        let duration_ms = duration.as_millis() as u64;

        tokio::spawn(async move {
            if let Err(e) = send_langfuse_event(
                &client,
                &config,
                &model,
                &use_case,
                input,
                &output_text,
                is_error,
                tool_count,
                duration_ms,
                session_id.as_deref(),
                start_time,
                end_time,
                usage,
            )
            .await
            {
                tracing::debug!("langfuse send failed: {}", e);
            }
        });

        result
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn max_content_chars(&self) -> Option<usize> {
        self.inner.max_content_chars()
    }

    fn max_content_chars_for_use_case(&self, use_case: UseCase) -> Option<usize> {
        self.inner.max_content_chars_for_use_case(use_case)
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_langfuse_event(
    client: &Client,
    config: &LangfuseConfig,
    model: &str,
    use_case: &str,
    input: serde_json::Value,
    output_text: &str,
    is_error: bool,
    tool_count: usize,
    duration_ms: u64,
    session_id: Option<&str>,
    start_time: chrono::DateTime<chrono::Utc>,
    end_time: chrono::DateTime<chrono::Utc>,
    usage: Option<crate::backend::Usage>,
) -> Result<()> {
    let trace_id = uuid_v4();
    let generation_id = uuid_v4();

    let mut trace_body = json!({
        "id": trace_id,
        "name": format!("omnish-{}", use_case.to_lowercase()),
        "timestamp": start_time.to_rfc3339(),
        "metadata": {
            "use_case": use_case,
            "model": model,
        },
    });
    if let Some(sid) = session_id {
        trace_body["sessionId"] = json!(sid);
    }

    let level = if is_error { "ERROR" } else { "DEFAULT" };
    let status_msg = if is_error {
        output_text.chars().take(500).collect::<String>()
    } else {
        String::new()
    };

    let mut gen_body = json!({
        "id": generation_id,
        "traceId": trace_id,
        "name": "llm-completion",
        "startTime": start_time.to_rfc3339(),
        "endTime": end_time.to_rfc3339(),
        "model": model,
        "input": input,
        "output": truncate(output_text, 1000),
        "metadata": {
            "use_case": use_case,
            "tool_count": tool_count,
            "duration_ms": duration_ms,
        },
        "level": level,
    });
    if !status_msg.is_empty() {
        gen_body["statusMessage"] = json!(status_msg);
    }
    if let Some(ref u) = usage {
        gen_body["usage"] = json!({
            "input": u.input_tokens,
            "output": u.output_tokens,
            "total": u.input_tokens + u.output_tokens,
        });
        gen_body["metadata"]["cache_read_input_tokens"] = json!(u.cache_read_input_tokens);
        gen_body["metadata"]["cache_creation_input_tokens"] = json!(u.cache_creation_input_tokens);
    }

    let payload = json!({
        "batch": [
            {
                "id": uuid_v4(),
                "timestamp": start_time.to_rfc3339(),
                "type": "trace-create",
                "body": trace_body,
            },
            {
                "id": uuid_v4(),
                "timestamp": start_time.to_rfc3339(),
                "type": "generation-create",
                "body": gen_body,
            },
        ],
    });

    let url = format!("{}/api/public/ingestion", config.host.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .basic_auth(&config.public_key, Some(&config.secret_key))
        .json(&payload)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::debug!("langfuse ingestion returned {}: {}", status, body);
    }

    Ok(())
}

/// Build a structured JSON representation of the LLM input for Langfuse.
fn build_langfuse_input(req: &LlmRequest) -> serde_json::Value {
    let mut input = serde_json::Map::new();

    if let Some(ref sp) = req.system_prompt {
        input.insert("system".into(), json!(truncate(sp, 2000)));
    }

    if !req.conversation.is_empty() {
        let turns: Vec<serde_json::Value> = req.conversation.iter().map(|t| {
            json!({"role": &t.role, "content": truncate(&t.content, 2000)})
        }).collect();
        input.insert("messages".into(), json!(turns));
    }

    if !req.context.is_empty() {
        input.insert("context".into(), json!(truncate(&req.context, 2000)));
    }

    if let Some(ref q) = req.query {
        input.insert("query".into(), json!(truncate(q, 2000)));
    }

    if !req.tools.is_empty() {
        let names: Vec<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
        input.insert("tools".into(), json!(names));
    }

    if !req.extra_messages.is_empty() {
        input.insert("extra_messages_count".into(), json!(req.extra_messages.len()));
    }

    serde_json::Value::Object(input)
}

fn uuid_v4() -> String {
    // Simple UUID v4 using random bytes
    use std::fmt::Write;
    let mut buf = [0u8; 16];
    getrandom(&mut buf);
    buf[6] = (buf[6] & 0x0f) | 0x40; // version 4
    buf[8] = (buf[8] & 0x3f) | 0x80; // variant 1
    let mut s = String::with_capacity(36);
    for (i, b) in buf.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            s.push('-');
        }
        write!(s, "{:02x}", b).unwrap();
    }
    s
}

fn getrandom(buf: &mut [u8]) {
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(buf);
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.min(s.len())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uuid_v4_format() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[8..9], "-");
        assert_eq!(&id[13..14], "-");
        assert_eq!(&id[18..19], "-");
        assert_eq!(&id[23..24], "-");
        // version nibble
        assert_eq!(&id[14..15], "4");
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let s = "a".repeat(100);
        let t = truncate(&s, 10);
        assert!(t.ends_with("..."));
        assert_eq!(t.len(), 13);
    }
}
