# RPC Transport Layer Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the raw send/recv transport with an RPC-style request/response multiplexing layer where every message gets a response and concurrent calls are safely correlated by request_id.

**Architecture:** Client uses `RpcClient` with split read/write halves and background tokio tasks for dispatching. Server uses `RpcServer` that spawns a tokio task per connection, reads frames, calls a handler, and writes responses. All messages are wrapped in `Frame { request_id, payload }`.

**Tech Stack:** Rust, Tokio (spawn, mpsc, oneshot, AsyncReadExt/AsyncWriteExt), bincode serialization.

---

### Task 1: Add `Ack` variant and `Frame` to omnish-protocol

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs`
- Test: `crates/omnish-protocol/src/message.rs` (inline tests)

**Step 1: Write the failing test**

Add at the bottom of `crates/omnish-protocol/src/message.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_round_trip() {
        let frame = Frame {
            request_id: 42,
            payload: Message::Ack,
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.request_id, 42);
        assert!(matches!(decoded.payload, Message::Ack));
    }

    #[test]
    fn test_frame_with_session_start() {
        let frame = Frame {
            request_id: 1,
            payload: Message::SessionStart(SessionStart {
                session_id: "abc".to_string(),
                parent_session_id: None,
                timestamp_ms: 1000,
                attrs: HashMap::new(),
            }),
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.request_id, 1);
        assert!(matches!(decoded.payload, Message::SessionStart(_)));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-protocol`
Expected: FAIL — `Frame` and `Message::Ack` don't exist yet.

**Step 3: Write minimal implementation**

Add `Ack` variant to the `Message` enum:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    SessionStart(SessionStart),
    SessionEnd(SessionEnd),
    IoData(IoData),
    Event(Event),
    Request(Request),
    Response(Response),
    CommandComplete(CommandComplete),
    Ack,
}
```

Add `Frame` struct and its serialization below the `Message` impl block:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub request_id: u64,
    pub payload: Message,
}

impl Frame {
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let payload_bytes = self.payload.to_bytes()?;
        let mut buf = Vec::with_capacity(8 + payload_bytes.len());
        buf.extend_from_slice(&self.request_id.to_be_bytes());
        buf.extend_from_slice(&payload_bytes);
        Ok(buf)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 8 {
            bail!("frame too short");
        }
        let request_id = u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        let payload = Message::from_bytes(&bytes[8..])?;
        Ok(Self { request_id, payload })
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-protocol`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/omnish-protocol/src/message.rs
git commit -m "feat(protocol): add Frame wrapper and Message::Ack variant"
```

---

### Task 2: Implement `RpcClient`

**Files:**
- Create: `crates/omnish-transport/src/rpc_client.rs`
- Modify: `crates/omnish-transport/src/lib.rs` (add `pub mod rpc_client;`)
- Modify: `crates/omnish-transport/Cargo.toml` (no new deps needed — tokio full is already in workspace)
- Test: `crates/omnish-transport/src/rpc_client.rs` (inline tests)

**Step 1: Write the failing test**

In `crates/omnish-transport/src/rpc_client.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use omnish_protocol::message::*;
    use tokio::net::UnixListener;
    use std::collections::HashMap;

    /// Minimal echo server: reads a Frame, replies with Ack using same request_id
    async fn echo_server(listener: UnixListener) {
        let (stream, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = stream.into_split();
        loop {
            // Read frame: [len: 4][frame_bytes]
            let len = match tokio::io::AsyncReadExt::read_u32(&mut reader).await {
                Ok(len) => len as usize,
                Err(_) => break,
            };
            let mut buf = vec![0u8; len];
            tokio::io::AsyncReadExt::read_exact(&mut reader, &mut buf).await.unwrap();
            let frame = Frame::from_bytes(&buf).unwrap();

            // Reply with Ack
            let reply = Frame {
                request_id: frame.request_id,
                payload: Message::Ack,
            };
            let reply_bytes = reply.to_bytes().unwrap();
            tokio::io::AsyncWriteExt::write_u32(&mut writer, reply_bytes.len() as u32).await.unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, &reply_bytes).await.unwrap();
            tokio::io::AsyncWriteExt::flush(&mut writer).await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_rpc_client_call_returns_ack() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        tokio::spawn(echo_server(listener));

        let client = RpcClient::connect_unix(sock.to_str().unwrap()).await.unwrap();
        let resp = client.call(Message::SessionStart(SessionStart {
            session_id: "s1".to_string(),
            parent_session_id: None,
            timestamp_ms: 1000,
            attrs: HashMap::new(),
        })).await.unwrap();

        assert!(matches!(resp, Message::Ack));
    }

    #[tokio::test]
    async fn test_rpc_client_concurrent_calls() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        // Server that delays response for Request messages (simulates slow LLM)
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            loop {
                let len = match tokio::io::AsyncReadExt::read_u32(&mut reader).await {
                    Ok(len) => len as usize,
                    Err(_) => break,
                };
                let mut buf = vec![0u8; len];
                tokio::io::AsyncReadExt::read_exact(&mut reader, &mut buf).await.unwrap();
                let frame = Frame::from_bytes(&buf).unwrap();

                let reply_payload = match &frame.payload {
                    Message::Request(_) => {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        Message::Response(Response {
                            request_id: String::new(),
                            content: "llm result".to_string(),
                            is_streaming: false,
                            is_final: true,
                        })
                    }
                    _ => Message::Ack,
                };

                let reply = Frame {
                    request_id: frame.request_id,
                    payload: reply_payload,
                };
                let reply_bytes = reply.to_bytes().unwrap();
                tokio::io::AsyncWriteExt::write_u32(&mut writer, reply_bytes.len() as u32).await.unwrap();
                tokio::io::AsyncWriteExt::write_all(&mut writer, &reply_bytes).await.unwrap();
                tokio::io::AsyncWriteExt::flush(&mut writer).await.unwrap();
            }
        });

        let client = RpcClient::connect_unix(sock.to_str().unwrap()).await.unwrap();

        // Fire IoData and Request concurrently
        let client_ref = &client;
        let (io_result, req_result) = tokio::join!(
            client_ref.call(Message::IoData(IoData {
                session_id: "s1".to_string(),
                direction: IoDirection::Input,
                timestamp_ms: 1000,
                data: b"ls\n".to_vec(),
            })),
            client_ref.call(Message::Request(Request {
                request_id: "r1".to_string(),
                session_id: "s1".to_string(),
                query: "what happened".to_string(),
                scope: RequestScope::AllSessions,
            })),
        );

        // IoData should get Ack
        assert!(matches!(io_result.unwrap(), Message::Ack));
        // Request should get Response
        assert!(matches!(req_result.unwrap(), Message::Response(_)));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-transport rpc_client`
Expected: FAIL — module and `RpcClient` don't exist.

**Step 3: Write minimal implementation**

In `crates/omnish-transport/src/rpc_client.rs`:

```rust
use anyhow::Result;
use omnish_protocol::message::{Frame, Message};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

struct WriteRequest {
    frame: Frame,
    reply_tx: oneshot::Sender<Message>,
}

pub struct RpcClient {
    tx: mpsc::Sender<WriteRequest>,
    next_id: AtomicU64,
    _write_task: JoinHandle<()>,
    _read_task: JoinHandle<()>,
}

impl RpcClient {
    pub async fn connect_unix(addr: &str) -> Result<Self> {
        let stream = UnixStream::connect(addr).await?;
        let (reader, writer) = stream.into_split();
        Self::from_split(reader, writer)
    }

    fn from_split(
        reader: tokio::net::unix::OwnedReadHalf,
        writer: tokio::net::unix::OwnedWriteHalf,
    ) -> Result<Self> {
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Message>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (tx, rx) = mpsc::channel::<WriteRequest>(256);

        let write_pending = pending.clone();
        let _write_task = tokio::spawn(Self::write_loop(rx, writer, write_pending));

        let read_pending = pending.clone();
        let _read_task = tokio::spawn(Self::read_loop(reader, read_pending));

        Ok(Self {
            tx,
            next_id: AtomicU64::new(1),
            _write_task,
            _read_task,
        })
    }

    pub async fn call(&self, msg: Message) -> Result<Message> {
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = Frame {
            request_id,
            payload: msg,
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(WriteRequest { frame, reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("write task closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("read task closed before response"))
    }

    async fn write_loop(
        mut rx: mpsc::Receiver<WriteRequest>,
        mut writer: tokio::net::unix::OwnedWriteHalf,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Message>>>>,
    ) {
        while let Some(req) = rx.recv().await {
            // Register callback before writing so read loop can find it
            pending.lock().await.insert(req.frame.request_id, req.reply_tx);

            let bytes = match req.frame.to_bytes() {
                Ok(b) => b,
                Err(_) => continue,
            };
            if writer.write_u32(bytes.len() as u32).await.is_err() {
                break;
            }
            if writer.write_all(&bytes).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    }

    async fn read_loop(
        mut reader: tokio::net::unix::OwnedReadHalf,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Message>>>>,
    ) {
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
                Err(_) => continue,
            };
            let mut map = pending.lock().await;
            if let Some(tx) = map.remove(&frame.request_id) {
                let _ = tx.send(frame.payload);
            }
        }
    }
}
```

Add to `crates/omnish-transport/src/lib.rs`:

```rust
pub mod rpc_client;
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-transport rpc_client`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/omnish-transport/src/rpc_client.rs crates/omnish-transport/src/lib.rs
git commit -m "feat(transport): add RpcClient with multiplexed request/response"
```

---

### Task 3: Implement `RpcServer`

**Files:**
- Create: `crates/omnish-transport/src/rpc_server.rs`
- Modify: `crates/omnish-transport/src/lib.rs` (add `pub mod rpc_server;`)
- Test: `crates/omnish-transport/src/rpc_server.rs` (inline tests)

**Step 1: Write the failing test**

In `crates/omnish-transport/src/rpc_server.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc_client::RpcClient;
    use omnish_protocol::message::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_rpc_server_handles_requests() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let sock_str = sock.to_str().unwrap().to_string();

        let mut server = RpcServer::bind_unix(&sock_str).await.unwrap();

        // Spawn server with a simple handler
        tokio::spawn(async move {
            server
                .serve(|msg| {
                    Box::pin(async move {
                        match msg {
                            Message::SessionStart(_) => Message::Ack,
                            Message::Request(req) => Message::Response(Response {
                                request_id: req.request_id,
                                content: "hello".to_string(),
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

        // Give server time to start listening
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let client = RpcClient::connect_unix(&sock_str).await.unwrap();

        // Test Ack response
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

        // Test Request/Response
        let resp = client
            .call(Message::Request(Request {
                request_id: "r1".to_string(),
                session_id: "s1".to_string(),
                query: "test".to_string(),
                scope: RequestScope::CurrentSession,
            }))
            .await
            .unwrap();
        assert!(matches!(resp, Message::Response(r) if r.content == "hello"));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-transport rpc_server`
Expected: FAIL — `RpcServer` doesn't exist.

**Step 3: Write minimal implementation**

In `crates/omnish-transport/src/rpc_server.rs`:

```rust
use anyhow::Result;
use omnish_protocol::message::{Frame, Message};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener as TokioUnixListener;

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
                let (mut reader, mut writer) = stream.into_split();
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
                        Err(_) => continue,
                    };

                    let response_payload = handler(frame.payload).await;
                    let reply = Frame {
                        request_id: frame.request_id,
                        payload: response_payload,
                    };
                    let reply_bytes = match reply.to_bytes() {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    if writer.write_u32(reply_bytes.len() as u32).await.is_err() {
                        break;
                    }
                    if writer.write_all(&reply_bytes).await.is_err() {
                        break;
                    }
                    if writer.flush().await.is_err() {
                        break;
                    }
                }
            });
        }
    }
}
```

Add to `crates/omnish-transport/src/lib.rs`:

```rust
pub mod rpc_server;
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-transport rpc_server`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/omnish-transport/src/rpc_server.rs crates/omnish-transport/src/lib.rs
git commit -m "feat(transport): add RpcServer with per-connection handler dispatch"
```

---

### Task 4: Migrate daemon server to RpcServer

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`
- Modify: `crates/omnish-daemon/src/main.rs`

**Step 1: Run existing tests to establish baseline**

Run: `cargo test -p omnish-daemon`
Expected: PASS (all existing tests still pass — they test SessionManager directly, not server)

**Step 2: Rewrite `server.rs` to use RpcServer**

Replace `DaemonServer::run` and `handle_connection` in `crates/omnish-daemon/src/server.rs`:

```rust
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

// resolve_context, handle_debug_request, handle_llm_request remain unchanged
```

**Step 3: Update `main.rs` to remove Transport parameter**

In `crates/omnish-daemon/src/main.rs`, change:

```rust
// Remove these imports:
// use omnish_transport::unix::UnixTransport;
// use omnish_transport::traits::{Connection, Transport};

// Change server.run call from:
//   server.run(&transport, &socket_path).await
// to:
    server.run(&socket_path).await
```

**Step 4: Run tests to verify nothing broke**

Run: `cargo test -p omnish-daemon`
Expected: PASS

Run: `cargo build -p omnish-daemon`
Expected: compiles successfully

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/main.rs
git commit -m "refactor(daemon): migrate server to RpcServer"
```

---

### Task 5: Migrate client to RpcClient

**Files:**
- Modify: `crates/omnish-client/src/main.rs`

**Step 1: Run existing tests to establish baseline**

Run: `cargo test -p omnish-client`
Expected: PASS

**Step 2: Replace `connect_daemon` to return `RpcClient`**

In `crates/omnish-client/src/main.rs`:

Replace imports:
```rust
// Remove:
// use omnish_transport::traits::{Connection, Transport};
// use omnish_transport::unix::UnixTransport;
// Add:
use omnish_transport::rpc_client::RpcClient;
```

Replace `connect_daemon` function:

```rust
async fn connect_daemon(
    session_id: &str,
    parent_session_id: Option<String>,
    child_pid: u32,
) -> Option<RpcClient> {
    let socket_path = get_socket_path();
    match RpcClient::connect_unix(&socket_path).await {
        Ok(client) => {
            let attrs = probe::default_session_probes(child_pid).collect_all();
            let msg = Message::SessionStart(SessionStart {
                session_id: session_id.to_string(),
                parent_session_id,
                timestamp_ms: timestamp_ms(),
                attrs,
            });
            match client.call(msg).await {
                Ok(_) => {
                    eprintln!("\x1b[32m[omnish]\x1b[0m Connected to daemon (session: {})", &session_id[..8]);
                    Some(client)
                }
                Err(_) => {
                    eprintln!("\x1b[33m[omnish]\x1b[0m Connected but failed to register session");
                    None
                }
            }
        }
        Err(e) => {
            eprintln!("\x1b[33m[omnish]\x1b[0m Daemon not available ({}), running in passthrough mode", e);
            eprintln!("\x1b[33m[omnish]\x1b[0m Socket: {}", socket_path);
            eprintln!("\x1b[33m[omnish]\x1b[0m To start daemon: omnish-daemon or cargo run -p omnish-daemon");
            None
        }
    }
}
```

**Step 3: Replace all `conn.send(&msg)` with `rpc.call(msg)`**

In main loop, change `daemon_conn` type from `Option<Box<dyn Connection>>` to `Option<RpcClient>`.

Replace all fire-and-forget sends:
```rust
// Before:
if let Some(ref conn) = daemon_conn {
    let _ = conn.send(&msg).await;
}

// After:
if let Some(ref rpc) = daemon_conn {
    let _ = rpc.call(msg).await;
}
```

**Step 4: Replace `send_daemon_query` to use `RpcClient`**

```rust
async fn send_daemon_query(
    query: &str,
    session_id: &str,
    rpc: &RpcClient,
    proxy: &PtyProxy,
    redirect: Option<&str>,
    show_thinking: bool,
) {
    if show_thinking {
        let status = display::render_thinking();
        nix::unistd::write(std::io::stdout(), status.as_bytes()).ok();
    }

    let request_id = Uuid::new_v4().to_string()[..8].to_string();
    let request = Message::Request(Request {
        request_id: request_id.clone(),
        session_id: session_id.to_string(),
        query: query.to_string(),
        scope: RequestScope::AllSessions,
    });

    match rpc.call(request).await {
        Ok(Message::Response(resp)) if resp.request_id == request_id => {
            if show_thinking {
                std::fs::write("/tmp/omnish_last_response.txt", &resp.content).ok();
            }
            handle_command_result(&resp.content, redirect, proxy);
            if show_thinking {
                let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
                let separator = display::render_separator(cols);
                let sep_line = format!("{}\r\n", separator);
                nix::unistd::write(std::io::stdout(), sep_line.as_bytes()).ok();
            }
        }
        _ => {
            let err = display::render_error("Failed to receive response");
            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
            proxy.write_all(b"\r").ok();
        }
    }
}
```

**Step 5: Build and test**

Run: `cargo build -p omnish-client`
Expected: compiles successfully

Run: `cargo test -p omnish-client`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "refactor(client): migrate to RpcClient"
```

---

### Task 6: Clean up old Transport traits

**Files:**
- Modify: `crates/omnish-transport/src/lib.rs` (remove old module exports)
- Delete: `crates/omnish-transport/src/traits.rs`
- Delete: `crates/omnish-transport/src/unix.rs`
- Modify: `crates/omnish-transport/Cargo.toml` (remove `async-trait` dependency)

**Step 1: Verify no remaining references to old traits**

Run: `grep -r "traits::" crates/ --include="*.rs"` and `grep -r "unix::UnixTransport" crates/ --include="*.rs"`
Expected: no matches (all migrated in Tasks 4-5)

**Step 2: Remove old files and update lib.rs**

Update `crates/omnish-transport/src/lib.rs`:
```rust
pub mod rpc_client;
pub mod rpc_server;
```

Remove `crates/omnish-transport/src/traits.rs` and `crates/omnish-transport/src/unix.rs`.

Remove `async-trait = "0.1"` from `crates/omnish-transport/Cargo.toml`.

**Step 3: Full build and test**

Run: `cargo build --workspace`
Expected: compiles

Run: `cargo test --workspace`
Expected: all tests pass

**Step 4: Commit**

```bash
git add -A crates/omnish-transport/
git commit -m "refactor(transport): remove old Connection/Transport/Listener traits"
```

---

### Task 7: Integration test — end-to-end RPC flow

**Files:**
- Test: `crates/omnish-transport/src/rpc_server.rs` (add integration test)

**Step 1: Write the test**

Add to `rpc_server.rs` tests module:

```rust
#[tokio::test]
async fn test_multiple_clients_concurrent() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("test.sock");
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

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

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
```

**Step 2: Run test**

Run: `cargo test -p omnish-transport test_multiple_clients`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/omnish-transport/src/rpc_server.rs
git commit -m "test(transport): add multi-client concurrent RPC integration test"
```
