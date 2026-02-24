use omnish_daemon::session_mgr::SessionManager;
use anyhow::Result;
use omnish_llm::backend::{LlmBackend, LlmRequest, TriggerType};
use omnish_protocol::message::*;
use omnish_transport::rpc_server::RpcServer;
use std::sync::Arc;

pub struct DaemonServer {
    session_mgr: Arc<SessionManager>,
    llm_backend: Option<Arc<dyn LlmBackend>>,
}

impl DaemonServer {
    pub fn new(session_mgr: Arc<SessionManager>, llm_backend: Option<Arc<dyn LlmBackend>>) -> Self {
        Self {
            session_mgr,
            llm_backend,
        }
    }

    pub async fn run(&self, addr: &str) -> Result<()> {
        let mut server = RpcServer::bind(addr).await?;
        tracing::info!("omnishd listening on {}", addr);

        let mgr = self.session_mgr.clone();
        let llm = self.llm_backend.clone();

        server
            .serve(move |msg| {
                let mgr = mgr.clone();
                let llm = llm.clone();
                Box::pin(async move { handle_message(msg, &mgr, &llm).await })
            })
            .await
    }
}

async fn handle_message(
    msg: Message,
    mgr: &SessionManager,
    llm: &Option<Arc<dyn LlmBackend>>,
) -> Message {
    match msg {
        Message::SessionStart(s) => {
            if let Err(e) = mgr.register(&s.session_id, s.parent_session_id, s.attrs).await {
                tracing::error!("register error: {}", e);
            }
            Message::Ack
        }
        Message::SessionEnd(s) => {
            if let Err(e) = mgr.end_session(&s.session_id).await {
                tracing::error!("end_session error: {}", e);
            }
            Message::Ack
        }
        Message::IoData(io) => {
            let dir = match io.direction {
                IoDirection::Input => 0,
                IoDirection::Output => 1,
            };
            if let Err(e) = mgr.write_io(&io.session_id, io.timestamp_ms, dir, &io.data).await {
                tracing::error!("write_io error: {}", e);
            }
            Message::Ack
        }
        Message::CommandComplete(cc) => {
            if let Err(e) = mgr.receive_command(&cc.session_id, cc.record).await {
                tracing::error!("receive_command error: {}", e);
            }
            Message::Ack
        }
        Message::Request(req) => {
            if req.query.starts_with("__cmd:") {
                let content = handle_builtin_command(&req, &mgr).await;
                return Message::Response(Response {
                    request_id: req.request_id,
                    content,
                    is_streaming: false,
                    is_final: true,
                });
            }

            let content = if let Some(ref backend) = llm {
                match handle_llm_request(&req, mgr, backend).await {
                    Ok(response) => response.content,
                    Err(e) => {
                        tracing::error!("LLM request failed: {}", e);
                        format!("Error: {}", e)
                    }
                }
            } else {
                "(LLM backend not configured)".to_string()
            };

            Message::Response(Response {
                request_id: req.request_id,
                content,
                is_streaming: false,
                is_final: true,
            })
        }
        Message::CompletionRequest(req) => {
            tracing::debug!("CompletionRequest: input={:?} seq={}", req.input, req.sequence_id);
            if let Some(ref backend) = llm {
                match handle_completion_request(&req, mgr, backend).await {
                    Ok(suggestions) => Message::CompletionResponse(
                        omnish_protocol::message::CompletionResponse {
                            sequence_id: req.sequence_id,
                            suggestions,
                        },
                    ),
                    Err(e) => {
                        tracing::error!("Completion request failed: {}", e);
                        Message::CompletionResponse(
                            omnish_protocol::message::CompletionResponse {
                                sequence_id: req.sequence_id,
                                suggestions: vec![],
                            },
                        )
                    }
                }
            } else {
                Message::CompletionResponse(
                    omnish_protocol::message::CompletionResponse {
                        sequence_id: req.sequence_id,
                        suggestions: vec![],
                    },
                )
            }
        }
        _ => Message::Ack,
    }
}

async fn resolve_context(req: &Request, mgr: &SessionManager) -> Result<String> {
    match &req.scope {
        RequestScope::CurrentSession => {
            mgr.get_session_context(&req.session_id).await
        }
        RequestScope::AllSessions => {
            mgr.get_all_sessions_context(&req.session_id).await
        }
        RequestScope::Sessions(ids) => {
            let mut combined = String::new();
            for sid in ids {
                match mgr.get_session_context(sid).await {
                    Ok(ctx) => {
                        combined.push_str(&format!("\n=== Session {} ===\n", sid));
                        combined.push_str(&ctx);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to get context for session {}: {}", sid, e);
                    }
                }
            }
            Ok(combined)
        }
    }
}

async fn handle_builtin_command(req: &Request, mgr: &SessionManager) -> String {
    let sub = req.query.strip_prefix("__cmd:").unwrap_or("");
    match sub {
        "context" => {
            match resolve_context(req, mgr).await {
                Ok(ctx) => ctx,
                Err(e) => format!("Error: {}", e),
            }
        }
        "sessions" => mgr.format_sessions_list().await,
        other => format!("Unknown command: {}", other),
    }
}

async fn handle_llm_request(
    req: &Request,
    mgr: &SessionManager,
    backend: &Arc<dyn LlmBackend>,
) -> Result<omnish_llm::backend::LlmResponse> {
    let context = resolve_context(req, mgr).await?;

    let llm_req = LlmRequest {
        context,
        query: Some(req.query.clone()),
        trigger: TriggerType::Manual,
        session_ids: vec![req.session_id.clone()],
    };

    backend.complete(&llm_req).await
}

async fn handle_completion_request(
    req: &omnish_protocol::message::CompletionRequest,
    mgr: &SessionManager,
    backend: &Arc<dyn LlmBackend>,
) -> Result<Vec<omnish_protocol::message::CompletionSuggestion>> {
    let context_req = Request {
        request_id: String::new(),
        session_id: req.session_id.clone(),
        query: String::new(),
        scope: RequestScope::AllSessions,
    };
    let context = resolve_context(&context_req, mgr).await?;

    let prompt = omnish_llm::template::build_completion_content(
        &context, &req.input, req.cursor_pos,
    );

    let llm_req = LlmRequest {
        context: String::new(),
        query: Some(prompt),
        trigger: TriggerType::Manual,
        session_ids: vec![req.session_id.clone()],
    };

    let response = backend.complete(&llm_req).await?;
    tracing::debug!("Completion LLM raw response: {:?}", response.content);
    parse_completion_suggestions(&response.content)
}

fn parse_completion_suggestions(
    content: &str,
) -> Result<Vec<omnish_protocol::message::CompletionSuggestion>> {
    let trimmed = content.trim();
    let start = trimmed.find('[').unwrap_or(0);
    let end = trimmed.rfind(']').map(|i| i + 1).unwrap_or(trimmed.len());
    let json_str = &trimmed[start..end];

    #[derive(serde::Deserialize)]
    struct RawSuggestion {
        text: String,
        confidence: f32,
    }

    let raw: Vec<RawSuggestion> = serde_json::from_str(json_str).unwrap_or_default();
    Ok(raw
        .into_iter()
        .map(|r| omnish_protocol::message::CompletionSuggestion {
            text: r.text,
            confidence: r.confidence.clamp(0.0, 1.0),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_completion_suggestions_valid() {
        let input = r#"[{"text": "tus", "confidence": 0.95}, {"text": "sh", "confidence": 0.7}]"#;
        let result = parse_completion_suggestions(input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "tus");
        assert!((result[0].confidence - 0.95).abs() < 0.01);
    }

    #[test]
    fn test_parse_completion_suggestions_with_surrounding_text() {
        let input = "Here are my suggestions:\n[{\"text\": \"tus\", \"confidence\": 0.9}]\nHope this helps!";
        let result = parse_completion_suggestions(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "tus");
    }

    #[test]
    fn test_parse_completion_suggestions_empty() {
        let result = parse_completion_suggestions("[]").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_completion_suggestions_invalid_json() {
        let result = parse_completion_suggestions("not json at all").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_completion_suggestions_clamps_confidence() {
        let input = r#"[{"text": "x", "confidence": 1.5}]"#;
        let result = parse_completion_suggestions(input).unwrap();
        assert!((result[0].confidence - 1.0).abs() < 0.01);
    }
}
