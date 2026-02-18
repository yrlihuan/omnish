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
        let mut server = RpcServer::bind_unix(addr).await?;
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
            #[cfg(debug_assertions)]
            if req.query.starts_with("__debug:") {
                let content = handle_debug_request(&req, mgr).await;
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

#[cfg(debug_assertions)]
async fn handle_debug_request(req: &Request, mgr: &SessionManager) -> String {
    let sub = req.query.strip_prefix("__debug:").unwrap_or("");
    match sub {
        "context" => {
            match resolve_context(req, mgr).await {
                Ok(ctx) => ctx,
                Err(e) => format!("Error: {}", e),
            }
        }
        other => format!("Unknown debug subcommand: {}", other),
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
