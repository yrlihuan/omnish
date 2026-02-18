use anyhow::Result;
use omnish_protocol::message::{Frame, Message};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener as TokioUnixListener;
use tokio::sync::Mutex;

pub struct RpcServer {
    listener: TokioUnixListener,
}

impl RpcServer {
    pub async fn bind_unix(addr: &str) -> Result<Self> {
        let _ = std::fs::remove_file(addr);
        let listener = TokioUnixListener::bind(addr)?;
        Ok(Self { listener })
    }

    pub async fn serve<F>(&mut self, handler: F) -> Result<()>
    where
        F: Fn(Message) -> Pin<Box<dyn Future<Output = Message> + Send>> + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        loop {
            let (stream, _) = self.listener.accept().await?;
            let handler = handler.clone();
            tokio::spawn(async move {
                let (mut reader, writer) = stream.into_split();
                let writer = Arc::new(Mutex::new(writer));
                loop {
                    let len = match reader.read_u32().await {
                        Ok(len) => len as usize,
                        Err(_) => break,
                    };
                    let mut buf = vec![0u8; len];
                    if reader.read_exact(&mut buf).await.is_err() {
                        break;
                    }
                    let frame = match Frame::from_bytes(&buf) {
                        Ok(f) => f,
                        Err(e) => {
                            tracing::warn!("failed to parse frame: {}", e);
                            continue;
                        }
                    };

                    let handler = handler.clone();
                    let writer = writer.clone();
                    tokio::spawn(async move {
                        let response_payload = handler(frame.payload).await;
                        let reply = Frame {
                            request_id: frame.request_id,
                            payload: response_payload,
                        };
                        let reply_bytes = match reply.to_bytes() {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::error!("failed to serialize response frame: {}", e);
                                return;
                            }
                        };
                        let mut w = writer.lock().await;
                        if w.write_u32(reply_bytes.len() as u32).await.is_err() {
                            return;
                        }
                        if w.write_all(&reply_bytes).await.is_err() {
                            return;
                        }
                        let _ = w.flush().await;
                    });
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc_client::RpcClient;
    use omnish_protocol::message::{Request, RequestScope, Response, SessionStart};
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_rpc_server_handles_requests() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("rpc_server_test.sock");
        let sock_path_str = sock_path.to_str().unwrap().to_string();

        // Start RpcServer with a handler
        let server_addr = sock_path_str.clone();
        let server_handle = tokio::spawn(async move {
            let mut server = RpcServer::bind_unix(&server_addr).await.unwrap();
            server
                .serve(|msg| {
                    Box::pin(async move {
                        match msg {
                            Message::SessionStart(_) => Message::Ack,
                            Message::Request(req) => Message::Response(Response {
                                request_id: req.request_id.clone(),
                                content: format!("answer to: {}", req.query),
                                is_streaming: false,
                                is_final: true,
                            }),
                            _ => Message::Ack,
                        }
                    })
                })
                .await
                .ok();
        });

        // Give the server a moment to bind
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Connect RpcClient
        let client = RpcClient::connect_unix(&sock_path_str).await.unwrap();

        // Call SessionStart -> verify Ack
        let session_msg = Message::SessionStart(SessionStart {
            session_id: "s1".to_string(),
            parent_session_id: None,
            timestamp_ms: 1000,
            attrs: HashMap::new(),
        });
        let resp = client.call(session_msg).await.unwrap();
        assert!(matches!(resp, Message::Ack));

        // Call Request -> verify Response with correct content
        let req_msg = Message::Request(Request {
            request_id: "r1".to_string(),
            session_id: "s1".to_string(),
            query: "what happened?".to_string(),
            scope: RequestScope::CurrentSession,
        });
        let resp = client.call(req_msg).await.unwrap();
        match resp {
            Message::Response(r) => {
                assert_eq!(r.request_id, "r1");
                assert_eq!(r.content, "answer to: what happened?");
                assert!(r.is_final);
            }
            other => panic!("expected Response, got {:?}", other),
        }

        // Clean up: abort the server (it loops forever)
        server_handle.abort();
    }

    #[tokio::test]
    async fn test_multiple_clients_concurrent() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("multi_client.sock");
        let sock_str = sock.to_str().unwrap().to_string();

        let mut server = RpcServer::bind_unix(&sock_str).await.unwrap();

        tokio::spawn(async move {
            server
                .serve(|msg| {
                    Box::pin(async move {
                        match msg {
                            Message::Request(req) => {
                                // Simulate slow handler
                                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                                Message::Response(Response {
                                    request_id: req.request_id.clone(),
                                    content: format!("echo: {}", req.query),
                                    is_streaming: false,
                                    is_final: true,
                                })
                            }
                            _ => Message::Ack,
                        }
                    })
                })
                .await
                .ok();
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Two clients connecting simultaneously
        let client_a = RpcClient::connect_unix(&sock_str).await.unwrap();
        let client_b = RpcClient::connect_unix(&sock_str).await.unwrap();

        // Both send requests at the same time
        let (resp_a, resp_b) = tokio::join!(
            client_a.call(Message::Request(Request {
                request_id: "a1".to_string(),
                session_id: "sa".to_string(),
                query: "from A".to_string(),
                scope: RequestScope::CurrentSession,
            })),
            client_b.call(Message::Request(Request {
                request_id: "b1".to_string(),
                session_id: "sb".to_string(),
                query: "from B".to_string(),
                scope: RequestScope::CurrentSession,
            })),
        );

        // Each client gets its own response — no cross-talk
        match resp_a.unwrap() {
            Message::Response(r) => assert_eq!(r.content, "echo: from A"),
            other => panic!("expected Response, got {:?}", other),
        }
        match resp_b.unwrap() {
            Message::Response(r) => assert_eq!(r.content, "echo: from B"),
            other => panic!("expected Response, got {:?}", other),
        }
    }
}
