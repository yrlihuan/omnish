use omnish_daemon::session_mgr::SessionManager;
use anyhow::Result;
use omnish_protocol::message::*;
use omnish_transport::traits::{Connection, Transport};
use std::sync::Arc;

pub struct DaemonServer {
    session_mgr: Arc<SessionManager>,
}

impl DaemonServer {
    pub fn new(session_mgr: Arc<SessionManager>) -> Self {
        Self { session_mgr }
    }

    pub async fn run(&self, transport: &dyn Transport, addr: &str) -> Result<()> {
        let mut listener = transport.listen(addr).await?;
        tracing::info!("omnishd listening on {}", addr);

        loop {
            match listener.accept().await {
                Ok(conn) => {
                    let mgr = self.session_mgr.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(conn, mgr).await {
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
) -> Result<()> {
    loop {
        let msg = match conn.recv().await {
            Ok(msg) => msg,
            Err(_) => break,
        };

        match msg {
            Message::SessionStart(s) => {
                mgr.register(&s.session_id, &s.shell, s.pid, &s.tty).await?;
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
                let resp = Message::Response(Response {
                    request_id: req.request_id,
                    content: "(LLM not yet wired)".to_string(),
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
