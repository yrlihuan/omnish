use omnish_daemon::session_mgr::SessionManager;
use anyhow::Result;
use omnish_llm::backend::{LlmBackend, LlmRequest, TriggerType};
use omnish_protocol::message::*;
use omnish_transport::traits::{Connection, Transport};
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

    pub async fn run(&self, transport: &dyn Transport, addr: &str) -> Result<()> {
        let mut listener = transport.listen(addr).await?;
        tracing::info!("omnishd listening on {}", addr);

        loop {
            match listener.accept().await {
                Ok(conn) => {
                    let mgr = self.session_mgr.clone();
                    let llm = self.llm_backend.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(conn, mgr, llm).await {
                            tracing::error!("connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("accept error: {}", e);
                }
            }
        }
    }
}

async fn handle_connection(
    conn: Box<dyn Connection>,
    mgr: Arc<SessionManager>,
    llm: Option<Arc<dyn LlmBackend>>,
) -> Result<()> {
    loop {
        let msg = match conn.recv().await {
            Ok(msg) => msg,
            Err(_) => break,
        };

        match msg {
            Message::SessionStart(s) => {
                mgr.register(&s.session_id, s.parent_session_id, s.attrs).await?;
            }
            Message::SessionEnd(s) => {
                mgr.end_session(&s.session_id).await?;
            }
            Message::IoData(io) => {
                let dir = match io.direction {
                    IoDirection::Input => 0,
                    IoDirection::Output => 1,
                };
                mgr.write_io(&io.session_id, io.timestamp_ms, dir, &io.data).await?;
            }
            Message::Request(req) => {
                #[cfg(debug_assertions)]
                if req.query.starts_with("__debug:") {
                    let content = handle_debug_request(&req, &mgr).await;
                    let resp = Message::Response(Response {
                        request_id: req.request_id,
                        content,
                        is_streaming: false,
                        is_final: true,
                    });
                    conn.send(&resp).await?;
                    continue;
                }

                let content = if let Some(ref backend) = llm {
                    match handle_llm_request(&req, &mgr, backend).await {
                        Ok(response) => response.content,
                        Err(e) => {
                            tracing::error!("LLM request failed: {}", e);
                            format!("Error: {}", e)
                        }
                    }
                } else {
                    "(LLM backend not configured)".to_string()
                };

                let resp = Message::Response(Response {
                    request_id: req.request_id,
                    content,
                    is_streaming: false,
                    is_final: true,
                });
                conn.send(&resp).await?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(debug_assertions)]
async fn handle_debug_request(req: &Request, mgr: &SessionManager) -> String {
    let sub = req.query.strip_prefix("__debug:").unwrap_or("");
    match sub {
        "context" => {
            match mgr.get_session_context(&req.session_id).await {
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
    let context = match &req.scope {
        RequestScope::CurrentSession => {
            mgr.get_session_context(&req.session_id).await?
        }
        RequestScope::AllSessions => {
            mgr.get_all_sessions_context().await?
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
            combined
        }
    };

    let llm_req = LlmRequest {
        context,
        query: Some(req.query.clone()),
        trigger: TriggerType::Manual,
        session_ids: vec![req.session_id.clone()],
    };

    backend.complete(&llm_req).await
}
