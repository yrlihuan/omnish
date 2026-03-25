use crate::{parse_addr, TransportAddr};
use anyhow::Result;
use omnish_protocol::message::{Auth, AuthOk, Frame, Message};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener as TokioUnixListener};
use tokio::sync::{mpsc, Mutex};
use tokio_rustls::TlsAcceptor;

fn is_fd_exhausted(e: &std::io::Error) -> bool {
    // EMFILE (per-process limit) or ENFILE (system-wide limit)
    matches!(e.raw_os_error(), Some(24) | Some(23))
}

/// Read /proc/self/fd and log fd count by type (socket, pipe, file, etc.)
fn dump_fd_stats() {
    let fd_dir = match std::fs::read_dir("/proc/self/fd") {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("cannot read /proc/self/fd: {e}");
            return;
        }
    };

    let mut total = 0u32;
    let mut by_type: HashMap<String, u32> = HashMap::new();
    let mut samples: HashMap<String, Vec<String>> = HashMap::new();

    for entry in fd_dir.flatten() {
        total += 1;
        let link = match std::fs::read_link(entry.path()) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => "(unreadable)".to_string(),
        };

        let kind = if link.starts_with("socket:") {
            "socket"
        } else if link.starts_with("pipe:") {
            "pipe"
        } else if link.starts_with("anon_inode:") {
            "anon_inode"
        } else if link.starts_with("/dev/") {
            "device"
        } else if link.starts_with('/') {
            "file"
        } else {
            "other"
        }
        .to_string();

        *by_type.entry(kind.clone()).or_default() += 1;
        let s = samples.entry(kind).or_default();
        if s.len() < 5 {
            s.push(link);
        }
    }

    // Get soft/hard limit via nix
    let (soft, hard) = match nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_NOFILE) {
        Ok((s, h)) => (s, h),
        Err(_) => (0, 0),
    };

    tracing::error!("fd stats: total={total} soft_limit={soft} hard_limit={hard}");

    let mut types: Vec<_> = by_type.into_iter().collect();
    types.sort_by(|a, b| b.1.cmp(&a.1));
    for (kind, count) in &types {
        let sample_list = samples
            .get(kind)
            .map(|v| v.join(", "))
            .unwrap_or_default();
        tracing::error!("  {kind}: {count} (samples: {sample_list})");
    }
}

enum Listener {
    Unix(TokioUnixListener),
    Tcp(TcpListener),
}

pub struct RpcServer {
    listener: Listener,
}

impl RpcServer {
    pub async fn bind_unix(addr: &str) -> Result<Self> {
        let _ = std::fs::remove_file(addr);
        let listener = TokioUnixListener::bind(addr)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(addr, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self {
            listener: Listener::Unix(listener),
        })
    }

    pub async fn bind_tcp(addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            listener: Listener::Tcp(listener),
        })
    }

    pub async fn bind(addr: &str) -> Result<Self> {
        match parse_addr(addr) {
            TransportAddr::Unix(p) => Self::bind_unix(&p).await,
            TransportAddr::Tcp(hp) => Self::bind_tcp(&hp).await,
        }
    }

    /// Returns the local TCP address if this is a TCP listener.
    pub fn local_tcp_addr(&self) -> Option<SocketAddr> {
        match &self.listener {
            Listener::Tcp(l) => l.local_addr().ok(),
            Listener::Unix(_) => None,
        }
    }

    pub async fn serve<F>(
        &mut self,
        handler: F,
        auth_token: Option<String>,
        tls_acceptor: Option<TlsAcceptor>,
    ) -> Result<()>
    where
        F: Fn(Message, mpsc::Sender<Message>) -> Pin<Box<dyn Future<Output = ()> + Send>>
            + Send
            + Sync
            + 'static,
    {
        let handler = Arc::new(handler);
        let auth_token = auth_token.map(Arc::new);
        loop {
            match &self.listener {
                Listener::Unix(l) => {
                    let (stream, _) = match l.accept().await {
                        Ok(v) => v,
                        Err(e) if is_fd_exhausted(&e) => {
                            dump_fd_stats();
                            return Err(e.into());
                        }
                        Err(e) => return Err(e.into()),
                    };
                    #[cfg(unix)]
                    {
                        let peer_cred = stream.peer_cred()?;
                        let my_uid = nix::unistd::getuid();
                        if peer_cred.uid() != my_uid.as_raw() {
                            tracing::warn!(
                                "rejected connection from UID {} (expected {})",
                                peer_cred.uid(),
                                my_uid
                            );
                            continue;
                        }
                    }
                    let (reader, writer) = stream.into_split();
                    spawn_connection(reader, writer, handler.clone(), auth_token.clone());
                }
                Listener::Tcp(l) => {
                    let (stream, _) = match l.accept().await {
                        Ok(v) => v,
                        Err(e) if is_fd_exhausted(&e) => {
                            dump_fd_stats();
                            return Err(e.into());
                        }
                        Err(e) => return Err(e.into()),
                    };
                    stream.set_nodelay(true)?;
                    if let Some(ref acceptor) = tls_acceptor {
                        match acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                let (reader, writer) = tokio::io::split(tls_stream);
                                spawn_connection(
                                    reader,
                                    writer,
                                    handler.clone(),
                                    auth_token.clone(),
                                );
                            }
                            Err(e) => {
                                tracing::warn!("TLS handshake failed: {}", e);
                                continue;
                            }
                        }
                    } else {
                        let (reader, writer) = stream.into_split();
                        spawn_connection(reader, writer, handler.clone(), auth_token.clone());
                    }
                }
            }
        }
    }
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame> {
    let len = reader.read_u32().await? as usize;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Frame::from_bytes(&buf)
}

async fn write_reply<W: AsyncWrite + Unpin>(
    writer: &Arc<Mutex<W>>,
    request_id: u64,
    payload: Message,
) -> Result<()> {
    let reply = Frame {
        request_id,
        payload,
    };
    let reply_bytes = reply.to_bytes()?;
    let mut w = writer.lock().await;
    w.write_u32(reply_bytes.len() as u32).await?;
    w.write_all(&reply_bytes).await?;
    w.flush().await?;
    Ok(())
}

fn spawn_connection<R, W, F>(
    reader: R,
    writer: W,
    handler: Arc<F>,
    auth_token: Option<Arc<String>>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    F: Fn(Message, mpsc::Sender<Message>) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync
        + 'static,
{
    tokio::spawn(async move {
        let mut reader = reader;
        let writer = Arc::new(Mutex::new(writer));

        // Authentication phase
        if let Some(expected_token) = auth_token {
            let auth_result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                read_frame(&mut reader),
            )
            .await;

            let frame = match auth_result {
                Ok(Ok(frame)) => frame,
                Ok(Err(_)) => return,
                Err(_) => {
                    tracing::warn!("auth timeout: client did not send auth within 5 seconds");
                    return;
                }
            };

            match frame.payload {
                Message::Auth(Auth { ref token, protocol_version }) if token == expected_token.as_str() => {
                    if protocol_version != omnish_protocol::message::PROTOCOL_VERSION {
                        tracing::warn!(
                            "rejecting client: protocol version mismatch (client={}, server={})",
                            protocol_version,
                            omnish_protocol::message::PROTOCOL_VERSION
                        );
                        let _ = write_reply(&writer, frame.request_id, Message::AuthFailed).await;
                        return;
                    }
                    let reply = Message::AuthOk(AuthOk {
                        protocol_version: omnish_protocol::message::PROTOCOL_VERSION,
                    });
                    if write_reply(&writer, frame.request_id, reply)
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                _ => {
                    let _ = write_reply(&writer, frame.request_id, Message::AuthFailed).await;
                    return;
                }
            }
        }

        // Normal message loop
        loop {
            let frame = match read_frame(&mut reader).await {
                Ok(f) => f,
                Err(e) => {
                    // EOF is normal (client disconnected); only warn on real errors
                    let msg = e.to_string().to_lowercase();
                    if !msg.contains("eof") && !msg.contains("end of file") {
                        tracing::warn!("failed to read frame: {}", e);
                    }
                    break;
                }
            };

            let handler = handler.clone();
            let writer = writer.clone();
            tokio::spawn(async move {
                let (tx, mut rx) = mpsc::channel::<Message>(16);
                let request_id = frame.request_id;

                // Spawn handler — it sends messages through tx
                tokio::spawn(async move {
                    handler(frame.payload, tx).await;
                    // tx is dropped when handler completes
                });

                // Read from channel and write to connection as messages arrive
                let mut count = 0u32;
                while let Some(msg) = rx.recv().await {
                    count += 1;
                    if let Err(e) = write_reply(&writer, request_id, msg).await {
                        tracing::error!("write_reply failed: {}", e);
                        break;
                    }
                }
                // Send end-of-stream sentinel for multi-message responses
                if count > 1 {
                    let _ = write_reply(&writer, request_id, Message::Ack).await;
                }
            });
        }
    });
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
                .serve(
                    |msg, tx| {
                        Box::pin(async move {
                            let reply = match msg {
                                Message::SessionStart(_) => Message::Ack,
                                Message::Request(req) => Message::Response(Response {
                                    request_id: req.request_id.clone(),
                                    content: format!("answer to: {}", req.query),
                                    is_streaming: false,
                                    is_final: true,
                                }),
                                _ => Message::Ack,
                            };
                            let _ = tx.send(reply).await;
                        })
                    },
                    None,
                    None,
                )
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
                .serve(
                    |msg, tx| {
                        Box::pin(async move {
                            let reply = match msg {
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
                            };
                            let _ = tx.send(reply).await;
                        })
                    },
                    None,
                    None,
                )
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

    #[tokio::test]
    async fn test_rpc_server_tcp_handles_requests() {
        let mut server = RpcServer::bind_tcp("127.0.0.1:0").await.unwrap();
        let addr = server.local_tcp_addr().unwrap().to_string();

        let server_handle = tokio::spawn(async move {
            server
                .serve(
                    |msg, tx| {
                        Box::pin(async move {
                            let reply = match msg {
                                Message::SessionStart(_) => Message::Ack,
                                Message::Request(req) => Message::Response(Response {
                                    request_id: req.request_id.clone(),
                                    content: format!("tcp answer to: {}", req.query),
                                    is_streaming: false,
                                    is_final: true,
                                }),
                                _ => Message::Ack,
                            };
                            let _ = tx.send(reply).await;
                        })
                    },
                    None,
                    None,
                )
                .await
                .ok();
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client = RpcClient::connect_tcp(&addr).await.unwrap();

        // SessionStart -> Ack
        let resp = client
            .call(Message::SessionStart(SessionStart {
                session_id: "s1".to_string(),
                parent_session_id: None,
                timestamp_ms: 1000,
                attrs: HashMap::new(),
            }))
            .await
            .unwrap();
        assert!(matches!(resp, Message::Ack));

        // Request -> Response
        let resp = client
            .call(Message::Request(Request {
                request_id: "r1".to_string(),
                session_id: "s1".to_string(),
                query: "hello tcp".to_string(),
                scope: RequestScope::CurrentSession,
            }))
            .await
            .unwrap();
        match resp {
            Message::Response(r) => {
                assert_eq!(r.content, "tcp answer to: hello tcp");
                assert!(r.is_final);
            }
            other => panic!("expected Response, got {:?}", other),
        }

        server_handle.abort();
    }

    #[tokio::test]
    async fn test_rpc_server_tcp_multiple_clients() {
        let mut server = RpcServer::bind_tcp("127.0.0.1:0").await.unwrap();
        let addr = server.local_tcp_addr().unwrap().to_string();

        tokio::spawn(async move {
            server
                .serve(
                    |msg, tx| {
                        Box::pin(async move {
                            let reply = match msg {
                                Message::Request(req) => Message::Response(Response {
                                    request_id: req.request_id.clone(),
                                    content: format!("echo: {}", req.query),
                                    is_streaming: false,
                                    is_final: true,
                                }),
                                _ => Message::Ack,
                            };
                            let _ = tx.send(reply).await;
                        })
                    },
                    None,
                    None,
                )
                .await
                .ok();
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client_a = RpcClient::connect_tcp(&addr).await.unwrap();
        let client_b = RpcClient::connect_tcp(&addr).await.unwrap();

        let (resp_a, resp_b) = tokio::join!(
            client_a.call(Message::Request(Request {
                request_id: "a1".to_string(),
                session_id: "sa".to_string(),
                query: "tcp from A".to_string(),
                scope: RequestScope::CurrentSession,
            })),
            client_b.call(Message::Request(Request {
                request_id: "b1".to_string(),
                session_id: "sb".to_string(),
                query: "tcp from B".to_string(),
                scope: RequestScope::CurrentSession,
            })),
        );

        match resp_a.unwrap() {
            Message::Response(r) => assert_eq!(r.content, "echo: tcp from A"),
            other => panic!("expected Response, got {:?}", other),
        }
        match resp_b.unwrap() {
            Message::Response(r) => assert_eq!(r.content, "echo: tcp from B"),
            other => panic!("expected Response, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_auth_required_and_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("auth.sock");
        let sock_str = sock.to_str().unwrap().to_string();
        let token = "test-secret-token".to_string();

        let server_token = token.clone();
        let server_addr = sock_str.clone();
        let server_handle = tokio::spawn(async move {
            let mut server = RpcServer::bind_unix(&server_addr).await.unwrap();
            server
                .serve(
                    |_msg, tx| {
                        Box::pin(async move {
                            let _ = tx.send(Message::Ack).await;
                        })
                    },
                    Some(server_token),
                    None,
                )
                .await
                .ok();
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Client sends correct auth token
        let client = RpcClient::connect_unix(&sock_str).await.unwrap();
        let resp = client
            .call(Message::Auth(Auth {
                token: token.clone(),
                protocol_version: omnish_protocol::message::PROTOCOL_VERSION,
            }))
            .await
            .unwrap();
        assert!(matches!(resp, Message::AuthOk(_)));

        // Normal messages should work after auth
        let resp = client
            .call(Message::SessionStart(SessionStart {
                session_id: "s1".into(),
                parent_session_id: None,
                timestamp_ms: 1000,
                attrs: HashMap::new(),
            }))
            .await
            .unwrap();
        assert!(matches!(resp, Message::Ack));

        server_handle.abort();
    }

    #[tokio::test]
    async fn test_auth_wrong_token_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("auth_fail.sock");
        let sock_str = sock.to_str().unwrap().to_string();

        let server_addr = sock_str.clone();
        let server_handle = tokio::spawn(async move {
            let mut server = RpcServer::bind_unix(&server_addr).await.unwrap();
            server
                .serve(
                    |_msg, tx| Box::pin(async move { let _ = tx.send(Message::Ack).await; }),
                    Some("correct-token".to_string()),
                    None,
                )
                .await
                .ok();
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client = RpcClient::connect_unix(&sock_str).await.unwrap();
        let resp = client
            .call(Message::Auth(Auth {
                token: "wrong-token".into(),
                protocol_version: omnish_protocol::message::PROTOCOL_VERSION,
            }))
            .await;
        // Should get AuthFailed or connection closed
        match resp {
            Ok(Message::AuthFailed) => {} // expected
            Err(_) => {}                  // also acceptable (server closes connection)
            other => panic!("expected AuthFailed or error, got {:?}", other),
        }

        server_handle.abort();
    }

    #[tokio::test]
    async fn test_auth_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("auth_timeout.sock");
        let sock_str = sock.to_str().unwrap().to_string();

        let server_addr = sock_str.clone();
        let server_handle = tokio::spawn(async move {
            let mut server = RpcServer::bind_unix(&server_addr).await.unwrap();
            server
                .serve(
                    |_msg, tx| Box::pin(async move { let _ = tx.send(Message::Ack).await; }),
                    Some("some-token".to_string()),
                    None,
                )
                .await
                .ok();
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Connect but don't send anything — server should timeout after 5s
        let client = RpcClient::connect_unix(&sock_str).await.unwrap();

        // Wait for server to timeout the connection
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;

        // Now try to send something — should fail since server closed the connection
        let resp = client
            .call(Message::SessionStart(SessionStart {
                session_id: "s1".into(),
                parent_session_id: None,
                timestamp_ms: 1000,
                attrs: HashMap::new(),
            }))
            .await;
        assert!(resp.is_err());

        server_handle.abort();
    }
}
