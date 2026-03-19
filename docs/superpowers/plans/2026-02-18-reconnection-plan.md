# RPC Client Reconnection Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add automatic reconnection to RpcClient so that disconnected daemon connections are transparently re-established with exponential backoff, and re-register the session via an on_reconnect callback.

**Architecture:** RpcClient gains internal state tracking (connected/disconnected) via an `Arc<AtomicBool>`. When read/write tasks detect connection loss, they flip the flag and spawn a reconnect task. The reconnect task re-establishes the connection, calls the on_reconnect callback, and replaces the internal mpsc/task machinery. Daemon-side `register()` becomes idempotent to handle duplicate SessionStart.

**Tech Stack:** Rust, Tokio (spawn, mpsc, oneshot, AtomicBool, sleep), bincode.

---

### Task 1: Make `SessionManager::register()` idempotent

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs:122-159`
- Test: `crates/omnish-daemon/tests/daemon_test.rs`

**Step 1: Write the failing test**

Add to `crates/omnish-daemon/tests/daemon_test.rs`:

```rust
#[tokio::test]
async fn test_register_idempotent_reuses_existing_session() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    let attrs1 = HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("pid".to_string(), "100".to_string()),
        ("tty".to_string(), "/dev/pts/0".to_string()),
        ("cwd".to_string(), "/home/user".to_string()),
    ]);
    mgr.register("sess1", None, attrs1).await.unwrap();

    // Record a command in the first registration
    mgr.receive_command("sess1", CommandRecord {
        command_id: 1,
        session_id: "sess1".to_string(),
        command_line: "echo hello".to_string(),
        started_at: 1000,
        ended_at: Some(2000),
        exit_code: Some(0),
        cwd: None,
        output_summary: Some("hello".to_string()),
        stream_offset: 0,
        stream_length: 0,
    }).await.unwrap();

    // Re-register with same session_id (simulating reconnect)
    let attrs2 = HashMap::from([
        ("shell".to_string(), "/bin/bash".to_string()),
        ("pid".to_string(), "100".to_string()),
        ("tty".to_string(), "/dev/pts/0".to_string()),
        ("cwd".to_string(), "/tmp".to_string()),
    ]);
    mgr.register("sess1", None, attrs2.clone()).await.unwrap();

    // Session should still be active
    let active = mgr.list_active().await;
    assert_eq!(active.len(), 1);

    // Previous commands should still exist
    let ctx = mgr.get_session_context("sess1").await.unwrap();
    assert!(ctx.contains("echo hello"), "previous commands should survive re-register");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon test_register_idempotent`
Expected: FAIL — second `register()` overwrites session, losing the command.

**Step 3: Implement idempotent register**

In `crates/omnish-daemon/src/session_mgr.rs`, modify `register()` (lines 122-159):

```rust
    pub async fn register(
        &self,
        session_id: &str,
        parent_session_id: Option<String>,
        attrs: std::collections::HashMap<String, String>,
    ) -> Result<()> {
        let mut sessions = self.sessions.lock().await;

        // Idempotent: if session already exists, update attrs and return
        if let Some(session) = sessions.get_mut(session_id) {
            session.meta.attrs = attrs;
            session.meta.save(&session.dir)?;
            tracing::info!("session {} re-registered (reconnect)", session_id);
            return Ok(());
        }

        let now = chrono::Utc::now().to_rfc3339();
        let session_dir = self.base_dir.join(format!(
            "{}_{}",
            now.replace(':', "-"),
            session_id
        ));
        std::fs::create_dir_all(&session_dir)?;

        let meta = SessionMeta {
            session_id: session_id.to_string(),
            parent_session_id,
            started_at: now,
            ended_at: None,
            attrs,
        };
        meta.save(&session_dir)?;

        let stream_writer = StreamWriter::create(&session_dir.join("stream.bin"))?;

        sessions.insert(
            session_id.to_string(),
            ActiveSession {
                meta,
                stream_writer,
                commands: Vec::new(),
                dir: session_dir,
                last_command_stream_pos: 0,
            },
        );
        Ok(())
    }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-daemon test_register_idempotent`
Expected: PASS

**Step 5: Run all daemon tests**

Run: `cargo test -p omnish-daemon`
Expected: all pass

**Step 6: Commit**

```bash
git add crates/omnish-daemon/src/session_mgr.rs crates/omnish-daemon/tests/daemon_test.rs
git commit -m "feat(daemon): make register() idempotent for reconnection support"
```

---

### Task 2: Refactor RpcClient internals to support reconnection

**Files:**
- Modify: `crates/omnish-transport/src/rpc_client.rs`

This task restructures RpcClient's internals so that the connection (mpsc sender, background tasks) can be replaced at runtime. No new public API yet — just internal refactoring.

**Step 1: Run existing tests to establish baseline**

Run: `cargo test -p omnish-transport rpc_client::tests`
Expected: PASS (2 tests)

**Step 2: Refactor RpcClient to use shared inner state**

Replace the current `RpcClient` struct with one that holds shared state via `Arc`:

```rust
use std::sync::atomic::AtomicBool;

struct Inner {
    tx: mpsc::Sender<WriteRequest>,
    connected: AtomicBool,
    _write_task: JoinHandle<()>,
    _read_task: JoinHandle<()>,
}

pub struct RpcClient {
    inner: Arc<Mutex<Inner>>,
    next_id: AtomicU64,
}
```

Update `from_split` to create the `Inner` and wrap in `Arc<Mutex>`:

```rust
    fn create_inner(
        reader: tokio::net::unix::OwnedReadHalf,
        writer: tokio::net::unix::OwnedWriteHalf,
    ) -> Inner {
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Message>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = mpsc::channel::<WriteRequest>(256);

        let write_pending = pending.clone();
        let _write_task = tokio::spawn(Self::write_loop(rx, writer, write_pending));

        let read_pending = pending.clone();
        let _read_task = tokio::spawn(Self::read_loop(reader, read_pending));

        Inner {
            tx,
            connected: AtomicBool::new(true),
            _write_task,
            _read_task,
        }
    }
```

Update `connect_unix`:

```rust
    pub async fn connect_unix(addr: &str) -> Result<Self> {
        let stream = UnixStream::connect(addr).await?;
        let (reader, writer) = stream.into_split();
        let inner = Self::create_inner(reader, writer);
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
            next_id: AtomicU64::new(1),
        })
    }
```

Update `call` to check connected state and use inner.tx:

```rust
    pub async fn call(&self, msg: Message) -> Result<Message> {
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = Frame { request_id, payload: msg };
        let (reply_tx, reply_rx) = oneshot::channel();

        {
            let inner = self.inner.lock().await;
            if !inner.connected.load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!("not connected"));
            }
            inner.tx
                .send(WriteRequest { frame, reply_tx })
                .await
                .map_err(|_| anyhow::anyhow!("write task closed"))?;
        }

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("read task closed before response"))
    }
```

**Step 3: Run existing tests**

Run: `cargo test -p omnish-transport rpc_client::tests`
Expected: PASS (2 tests, unchanged behavior)

**Step 4: Commit**

```bash
git add crates/omnish-transport/src/rpc_client.rs
git commit -m "refactor(transport): restructure RpcClient internals for reconnection"
```

---

### Task 3: Add `connect_unix_with_reconnect` and reconnection logic

**Files:**
- Modify: `crates/omnish-transport/src/rpc_client.rs`

**Step 1: Write the failing test**

Add to the tests module in `rpc_client.rs`:

```rust
    #[tokio::test]
    async fn test_rpc_client_reconnects_after_server_drop() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("reconnect.sock");
        let sock_path_str = sock_path.to_str().unwrap().to_string();

        // Start first server
        let listener1 = UnixListener::bind(&sock_path).unwrap();
        let server1 = tokio::spawn(async move {
            let (mut stream, _) = listener1.accept().await.unwrap();
            // Handle SessionStart from on_reconnect (initial connect)
            let frame = read_frame(&mut stream).await.unwrap();
            assert!(matches!(frame.payload, Message::SessionStart(_)));
            write_frame(&mut stream, &Frame { request_id: frame.request_id, payload: Message::Ack }).await.unwrap();
            // Handle one IoData call
            let frame = read_frame(&mut stream).await.unwrap();
            assert!(matches!(frame.payload, Message::IoData(_)));
            write_frame(&mut stream, &Frame { request_id: frame.request_id, payload: Message::Ack }).await.unwrap();
            // Drop stream to simulate disconnect
        });

        let reconnect_count = Arc::new(AtomicU64::new(0));
        let reconnect_count_clone = reconnect_count.clone();

        let client = RpcClient::connect_unix_with_reconnect(
            &sock_path_str,
            move |rpc| {
                let count = reconnect_count_clone.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    rpc.call(Message::SessionStart(SessionStart {
                        session_id: "s1".to_string(),
                        parent_session_id: None,
                        timestamp_ms: 1000,
                        attrs: HashMap::new(),
                    })).await?;
                    Ok(())
                })
            },
        ).await.unwrap();

        // First call should succeed (initial connection)
        let resp = client.call(Message::IoData(IoData {
            session_id: "s1".to_string(),
            direction: IoDirection::Input,
            timestamp_ms: 2000,
            data: b"ls".to_vec(),
        })).await.unwrap();
        assert!(matches!(resp, Message::Ack));

        // Wait for server1 to finish and drop connection
        server1.await.unwrap();

        // Give time for disconnect detection
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Calls during reconnection should fail
        let result = client.call(Message::IoData(IoData {
            session_id: "s1".to_string(),
            direction: IoDirection::Input,
            timestamp_ms: 3000,
            data: b"pwd".to_vec(),
        })).await;
        assert!(result.is_err());

        // Start second server on the same socket path
        let _ = std::fs::remove_file(&sock_path);
        let listener2 = UnixListener::bind(&sock_path).unwrap();
        let server2 = tokio::spawn(async move {
            let (mut stream, _) = listener2.accept().await.unwrap();
            // Handle SessionStart from on_reconnect callback
            let frame = read_frame(&mut stream).await.unwrap();
            assert!(matches!(frame.payload, Message::SessionStart(_)));
            write_frame(&mut stream, &Frame { request_id: frame.request_id, payload: Message::Ack }).await.unwrap();
            // Handle one more IoData
            let frame = read_frame(&mut stream).await.unwrap();
            assert!(matches!(frame.payload, Message::IoData(_)));
            write_frame(&mut stream, &Frame { request_id: frame.request_id, payload: Message::Ack }).await.unwrap();
        });

        // Wait for reconnection (backoff starts at 1s)
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        // After reconnection, calls should succeed again
        let resp = client.call(Message::IoData(IoData {
            session_id: "s1".to_string(),
            direction: IoDirection::Input,
            timestamp_ms: 4000,
            data: b"whoami".to_vec(),
        })).await.unwrap();
        assert!(matches!(resp, Message::Ack));

        // on_reconnect should have been called (initial + reconnect)
        assert_eq!(reconnect_count.load(Ordering::Relaxed), 2);

        server2.await.unwrap();
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-transport test_rpc_client_reconnects -- --nocapture`
Expected: FAIL — `connect_unix_with_reconnect` doesn't exist.

**Step 3: Implement `connect_unix_with_reconnect`**

Add to `RpcClient` in `rpc_client.rs`:

```rust
use std::sync::atomic::AtomicBool;
use std::future::Future;
use std::pin::Pin;

type ReconnectFn = Arc<
    dyn Fn(&RpcClient) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

impl RpcClient {
    pub async fn connect_unix_with_reconnect(
        addr: &str,
        on_reconnect: impl Fn(&RpcClient) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self> {
        let stream = UnixStream::connect(addr).await?;
        let (reader, writer) = stream.into_split();
        let inner = Self::create_inner(reader, writer);
        let client = Self {
            inner: Arc::new(Mutex::new(inner)),
            next_id: AtomicU64::new(1),
        };

        // Call on_reconnect for initial registration
        let on_reconnect = Arc::new(on_reconnect) as ReconnectFn;
        on_reconnect(&client).await?;

        // Spawn reconnect monitor
        let inner_ref = client.inner.clone();
        let addr = addr.to_string();
        let next_id_ref = Arc::new(AtomicU64::new(0)); // shared with client? No — we need to detect disconnection.
        // Actually, we monitor the write/read tasks. When they exit, inner.connected becomes false.
        // We need a notification mechanism. Let's use a watch channel.

        // Simpler approach: spawn a task that periodically checks connected state
        // and attempts reconnection when disconnected.
        Self::spawn_reconnect_monitor(inner_ref, addr, on_reconnect, client.next_id_ref());

        Ok(client)
    }
}
```

Actually, the cleanest approach is to have the read_loop notify when it exits. Add a `oneshot::Sender` that fires when read_loop breaks, and the reconnect monitor waits on it.

Here's the complete implementation:

```rust
    fn create_inner(
        reader: tokio::net::unix::OwnedReadHalf,
        writer: tokio::net::unix::OwnedWriteHalf,
        disconnect_tx: Option<oneshot::Sender<()>>,
    ) -> Inner {
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Message>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = mpsc::channel::<WriteRequest>(256);

        let write_pending = pending.clone();
        let _write_task = tokio::spawn(Self::write_loop(rx, writer, write_pending));

        let read_pending = pending.clone();
        let _read_task = tokio::spawn(Self::read_loop(reader, read_pending, disconnect_tx));

        Inner {
            tx,
            connected: AtomicBool::new(true),
            _write_task,
            _read_task,
        }
    }

    async fn read_loop(
        mut reader: tokio::net::unix::OwnedReadHalf,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Message>>>>,
        disconnect_tx: Option<oneshot::Sender<()>>,
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
        // Notify disconnect
        if let Some(tx) = disconnect_tx {
            let _ = tx.send(());
        }
    }

    pub async fn connect_unix_with_reconnect(
        addr: &str,
        on_reconnect: impl Fn(&RpcClient) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self> {
        let (disconnect_tx, disconnect_rx) = oneshot::channel();
        let stream = UnixStream::connect(addr).await?;
        let (reader, writer) = stream.into_split();
        let inner = Self::create_inner(reader, writer, Some(disconnect_tx));
        let client = Self {
            inner: Arc::new(Mutex::new(inner)),
            next_id: AtomicU64::new(1),
        };

        let on_reconnect = Arc::new(on_reconnect) as ReconnectFn;

        // Call on_reconnect for initial registration
        on_reconnect(&client).await?;

        // Spawn reconnect monitor
        let inner_ref = client.inner.clone();
        let next_id = // need to share AtomicU64...
        // Actually RpcClient.next_id is not in Arc. We need it to be.
        // Let's make next_id an Arc<AtomicU64> so the reconnect monitor
        // can create a temporary RpcClient for the callback.
        ...
    }
```

This is getting complex in the plan text. Let me restructure. The key insight is that the reconnect monitor needs to:
1. Wait for disconnect notification
2. Loop: try to reconnect with backoff
3. On success: create new Inner, call on_reconnect, replace client.inner

Since `on_reconnect` receives `&RpcClient`, we need a temporary client wrapping the new inner for the callback. But simpler: `on_reconnect` just needs `call()`. So we can pass the new inner's tx channel directly, or better — create a temporary RpcClient pointing at the new inner, call on_reconnect, then if successful, swap the client's inner.

Let me write the actual implementation more carefully:

```rust
    pub async fn connect_unix_with_reconnect(
        addr: &str,
        on_reconnect: impl Fn(&RpcClient) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self> {
        let (disconnect_tx, disconnect_rx) = oneshot::channel();
        let stream = UnixStream::connect(addr).await?;
        let (reader, writer) = stream.into_split();
        let inner = Self::create_inner(reader, writer, Some(disconnect_tx));
        let next_id = Arc::new(AtomicU64::new(1));
        let client = Self {
            inner: Arc::new(Mutex::new(inner)),
            next_id: next_id.clone(),
        };

        let on_reconnect = Arc::new(on_reconnect) as ReconnectFn;

        // Initial registration
        on_reconnect(&client).await?;

        // Spawn reconnect loop
        let inner_ref = client.inner.clone();
        let addr = addr.to_string();
        let on_reconnect_clone = on_reconnect.clone();
        tokio::spawn(Self::reconnect_loop(
            inner_ref,
            next_id,
            addr,
            on_reconnect_clone,
            disconnect_rx,
        ));

        Ok(client)
    }

    async fn reconnect_loop(
        inner: Arc<Mutex<Inner>>,
        next_id: Arc<AtomicU64>,
        addr: String,
        on_reconnect: ReconnectFn,
        mut disconnect_rx: oneshot::Receiver<()>,
    ) {
        // Wait for first disconnect
        let _ = disconnect_rx.await;

        // Mark disconnected
        inner.lock().await.connected.store(false, Ordering::Relaxed);

        let mut backoff = std::time::Duration::from_secs(1);
        let max_backoff = std::time::Duration::from_secs(30);

        loop {
            tokio::time::sleep(backoff).await;

            // Try to connect
            let stream = match UnixStream::connect(&addr).await {
                Ok(s) => s,
                Err(_) => {
                    backoff = std::cmp::min(backoff * 2, max_backoff);
                    continue;
                }
            };

            let (reader, writer) = stream.into_split();
            let (new_disconnect_tx, new_disconnect_rx) = oneshot::channel();
            let new_inner = Self::create_inner(reader, writer, Some(new_disconnect_tx));

            // Create temporary client to pass to on_reconnect callback
            let temp_client = RpcClient {
                inner: Arc::new(Mutex::new(new_inner)),
                next_id: next_id.clone(),
            };

            // Call on_reconnect (e.g. send SessionStart)
            if on_reconnect(&temp_client).await.is_err() {
                backoff = std::cmp::min(backoff * 2, max_backoff);
                continue;
            }

            // Success — swap inner
            let new_inner = Arc::try_unwrap(temp_client.inner)
                .expect("temp_client is sole owner")
                .into_inner();
            *inner.lock().await = new_inner;

            // Reset backoff
            backoff = std::time::Duration::from_secs(1);

            // Wait for next disconnect
            disconnect_rx = new_disconnect_rx;
            let _ = disconnect_rx.await;
            inner.lock().await.connected.store(false, Ordering::Relaxed);
        }
    }
```

Note: `next_id` needs to be `Arc<AtomicU64>` instead of bare `AtomicU64`. This requires updating the struct.

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-transport test_rpc_client_reconnects -- --nocapture`
Expected: PASS

**Step 5: Run all transport tests**

Run: `cargo test -p omnish-transport`
Expected: all pass

**Step 6: Commit**

```bash
git add crates/omnish-transport/src/rpc_client.rs
git commit -m "feat(transport): add RpcClient reconnection with exponential backoff"
```

---

### Task 4: Update client to use `connect_unix_with_reconnect`

**Files:**
- Modify: `crates/omnish-client/src/main.rs:276-309`

**Step 1: Replace `connect_daemon` to use reconnection**

```rust
async fn connect_daemon(
    session_id: &str,
    parent_session_id: Option<String>,
    child_pid: u32,
) -> Option<RpcClient> {
    let socket_path = get_socket_path();
    let sid = session_id.to_string();
    let psid = parent_session_id.clone();

    match RpcClient::connect_unix_with_reconnect(
        &socket_path,
        move |rpc| {
            let sid = sid.clone();
            let psid = psid.clone();
            Box::pin(async move {
                let attrs = probe::default_session_probes(child_pid).collect_all();
                rpc.call(Message::SessionStart(SessionStart {
                    session_id: sid,
                    parent_session_id: psid,
                    timestamp_ms: timestamp_ms(),
                    attrs,
                })).await?;
                Ok(())
            })
        },
    ).await {
        Ok(client) => {
            eprintln!("\x1b[32m[omnish]\x1b[0m Connected to daemon (session: {})", &session_id[..8]);
            Some(client)
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

**Step 2: Build and test**

Run: `cargo build -p omnish-client`
Expected: compiles

Run: `cargo test -p omnish-client`
Expected: all pass

**Step 3: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat(client): use RpcClient reconnection for daemon connection"
```

---

### Task 5: Full workspace verification

**Step 1: Run all tests**

Run: `cargo test --workspace`
Expected: all pass

**Step 2: Commit any fixups if needed**
