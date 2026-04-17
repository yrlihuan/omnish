# Client Config Push Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Daemon owns client config in `daemon.toml` `[client]` section, pushes changes to connected clients via `ConfigClient` message, and clients hot-reload settings at runtime.

**Architecture:** Add `[client]` section to `DaemonConfig`, `ConfigSection::Client` to `ConfigWatcher`, per-connection push channels in `RpcServer`, and a `push_rx` channel in `RpcClient`. Client receives `ConfigClient` messages and updates `InputInterceptor`/`TimeGapGuard`/local variables at runtime, writing back to `client.toml` as cache.

**Tech Stack:** Rust, tokio (mpsc/watch channels), bincode (protocol), toml_edit (config persistence)

---

### Task 1: Add `ClientSection` to `DaemonConfig`

**Files:**
- Modify: `crates/omnish-common/src/config.rs:238-258` (DaemonConfig struct)
- Modify: `crates/omnish-common/src/config.rs:260-272` (DaemonConfig::default)

- [ ] **Step 1: Add `ClientSection` struct to config.rs**

After `ShellConfig` (around line 306), add:

```rust
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ClientSection {
    #[serde(default = "default_command_prefix")]
    pub command_prefix: String,
    #[serde(default = "default_resume_prefix")]
    pub resume_prefix: String,
    #[serde(default = "default_true")]
    pub completion_enabled: bool,
    #[serde(default = "default_ghost_timeout_ms", deserialize_with = "string_or_int::deserialize")]
    pub ghost_timeout_ms: u64,
    #[serde(default = "default_intercept_gap_ms", deserialize_with = "string_or_int::deserialize")]
    pub intercept_gap_ms: u64,
    #[serde(default = "default_developer_mode")]
    pub developer_mode: bool,
}

impl Default for ClientSection {
    fn default() -> Self {
        Self {
            command_prefix: default_command_prefix(),
            resume_prefix: default_resume_prefix(),
            completion_enabled: true,
            ghost_timeout_ms: default_ghost_timeout_ms(),
            intercept_gap_ms: default_intercept_gap_ms(),
            developer_mode: default_developer_mode(),
        }
    }
}
```

- [ ] **Step 2: Add `client` field to `DaemonConfig`**

In the `DaemonConfig` struct (line 238):

```rust
#[serde(default)]
pub client: ClientSection,
```

And in `DaemonConfig::default()` (line 260):

```rust
client: ClientSection::default(),
```

- [ ] **Step 3: Build and verify**

Run: `cargo build --release -p omnish-common`
Expected: compiles successfully

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-common/src/config.rs
git commit -m "feat(config): add ClientSection to DaemonConfig for daemon-owned client config"
```

---

### Task 2: Add `ConfigClient` message to protocol

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs:8` (PROTOCOL_VERSION)
- Modify: `crates/omnish-protocol/src/message.rs:48-104` (Message enum)
- Modify: `crates/omnish-protocol/src/message.rs:616` (EXPECTED_VARIANT_COUNT)
- Modify: `crates/omnish-protocol/src/message.rs:787-819` (exhaustive match in test)

- [ ] **Step 1: Bump PROTOCOL_VERSION**

At line 8, change:
```rust
pub const PROTOCOL_VERSION: u32 = 14;
```

- [ ] **Step 2: Add ConfigClient variant to Message enum**

After `ConfigUpdateResult` (line 78), add:

```rust
ConfigClient { changes: Vec<ConfigChange> },
```

- [ ] **Step 3: Update variant count test**

At line 616, change `EXPECTED_VARIANT_COUNT` from `31` to `32`.

In the exhaustive match (around line 787-819), add a new arm:

```rust
Message::ConfigClient { .. } => {}
```

- [ ] **Step 4: Update daemon server ignore arm**

In `crates/omnish-daemon/src/server.rs:894`, add `ConfigClient` to the ignored pattern:

```rust
Message::ConfigResponse { .. } | Message::ConfigUpdateResult { .. } | Message::ConfigClient { .. } => {
```

- [ ] **Step 5: Build and test**

Run: `cargo build --release -p omnish-protocol && cargo test -p omnish-protocol --release`
Expected: compiles, tests pass (variant count = 32)

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-protocol/src/message.rs crates/omnish-daemon/src/server.rs
git commit -m "feat(protocol): add ConfigClient message for daemon-to-client config push (v14)"
```

---

### Task 3: Add `ConfigSection::Client` to `ConfigWatcher`

**Files:**
- Modify: `crates/omnish-daemon/src/config_watcher.rs:8-16` (ConfigSection enum)
- Modify: `crates/omnish-daemon/src/config_watcher.rs:28-38` (from_toml_key)
- Modify: `crates/omnish-daemon/src/config_watcher.rs:46-54` (WATCHED_SECTIONS)
- Modify: `crates/omnish-daemon/src/config_watcher.rs:67-77` (senders init)
- Modify: `crates/omnish-daemon/src/config_watcher.rs:116-131` (reload diff)

- [ ] **Step 1: Add Client variant to ConfigSection**

At line 9, add `Client` to the enum:

```rust
#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug)]
pub enum ConfigSection {
    Tools,
    Sandbox,
    Context,
    Llm,
    Tasks,
    Plugins,
    Client,
}
```

- [ ] **Step 2: Add `from_toml_key` mapping**

In `from_toml_key` (line 28), add:
```rust
"client" => Some(ConfigSection::Client),
```

- [ ] **Step 3: Add Client to WATCHED_SECTIONS**

At line 46:
```rust
pub const WATCHED_SECTIONS: &[ConfigSection] = &[
    ConfigSection::Sandbox,
    ConfigSection::Llm,
    ConfigSection::Plugins,
    ConfigSection::Tasks,
    ConfigSection::Client,
];
```

- [ ] **Step 4: Add Client to senders init**

At line 67, add `ConfigSection::Client` to the list:
```rust
for section in [
    ConfigSection::Tools,
    ConfigSection::Sandbox,
    ConfigSection::Context,
    ConfigSection::Llm,
    ConfigSection::Tasks,
    ConfigSection::Plugins,
    ConfigSection::Client,
] {
```

- [ ] **Step 5: Add diff logic in reload()**

In the match at line 117, add:
```rust
ConfigSection::Client => current.client != new_config.client,
```

- [ ] **Step 6: Update tests to include Client section in senders**

In all three test functions (`test_reload_detects_sandbox_change`, `test_reload_no_change_no_notify`, `test_reload_invalid_toml_keeps_current`), add `ConfigSection::Client` to the senders initialization loop alongside the other sections.

- [ ] **Step 7: Write test for client section change detection**

```rust
#[test]
fn test_reload_detects_client_change() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("daemon.toml");
    std::fs::write(&config_path, "").unwrap();
    let initial = DaemonConfig::default();

    let initial_arc = Arc::new(initial.clone());
    let mut senders = HashMap::new();
    let (tx, rx) = watch::channel(Arc::clone(&initial_arc));
    senders.insert(ConfigSection::Client, tx);
    for section in [
        ConfigSection::Tools, ConfigSection::Sandbox, ConfigSection::Context,
        ConfigSection::Llm, ConfigSection::Tasks, ConfigSection::Plugins,
    ] {
        let (tx, _) = watch::channel(Arc::clone(&initial_arc));
        senders.insert(section, tx);
    }

    let cw = ConfigWatcher {
        config_path: config_path.clone(),
        current: RwLock::new(initial),
        senders,
    };

    std::fs::write(&config_path, r#"
[client]
command_prefix = "/"
"#).unwrap();

    cw.reload().unwrap();
    assert!(rx.has_changed().unwrap());
    let config = rx.borrow();
    assert_eq!(config.client.command_prefix, "/");
}
```

- [ ] **Step 8: Build and test**

Run: `cargo build --release -p omnish-daemon && cargo test -p omnish-daemon --release -- config_watcher`
Expected: all tests pass

- [ ] **Step 9: Commit**

```bash
git add crates/omnish-daemon/src/config_watcher.rs
git commit -m "feat(config-watcher): add Client section for daemon-to-client config push"
```

---

### Task 4: Per-connection push channel in `RpcServer`

**Files:**
- Modify: `crates/omnish-transport/src/rpc_server.rs:126-200` (serve method)
- Modify: `crates/omnish-transport/src/rpc_server.rs:227-338` (spawn_connection)

- [ ] **Step 1: Add PushRegistry type and pass to serve()**

Add a type alias and modify `serve()` to accept an optional push registry:

```rust
use std::sync::atomic::{AtomicU64, Ordering};

/// Registry of per-connection push channels. Daemon adds entries on
/// connection, removes on disconnect, and sends push messages by iterating.
pub type PushRegistry = Arc<Mutex<HashMap<u64, mpsc::Sender<Message>>>>;

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);
```

Modify `serve()` signature (line 126) to accept an optional push registry:

```rust
pub async fn serve<F>(
    &mut self,
    handler: F,
    auth_token: Option<String>,
    tls_acceptor: Option<TlsAcceptor>,
    push_registry: Option<PushRegistry>,
) -> Result<()>
```

Pass `push_registry` into each `spawn_connection` call (lines 165, 181, 195).

- [ ] **Step 2: Modify spawn_connection to accept push registry**

Add `push_registry: Option<PushRegistry>` parameter to `spawn_connection`.

After successful auth (line 294), register this connection:

```rust
let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
let push_rx = if let Some(ref registry) = push_registry {
    let (push_tx, push_rx) = mpsc::channel::<Message>(32);
    registry.lock().await.insert(conn_id, push_tx);
    Some(push_rx)
} else {
    None
};
```

- [ ] **Step 3: Add push message forwarding in the connection loop**

In the normal message loop (line 297), use `tokio::select!` to handle both incoming requests and push messages. Push messages are written with `request_id = 0`:

```rust
let mut push_rx = push_rx;
loop {
    tokio::select! {
        frame_result = read_frame(&mut reader) => {
            let frame = match frame_result {
                Ok(f) => f,
                Err(e) => {
                    let msg = e.to_string().to_lowercase();
                    if !msg.contains("eof") && !msg.contains("end of file") {
                        tracing::warn!("failed to read frame: {}", e);
                    }
                    break;
                }
            };
            // ... existing handler spawn code (lines 310-335) ...
        }
        push_msg = async {
            match push_rx.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        } => {
            if let Some(msg) = push_msg {
                if let Err(e) = write_reply(&writer, 0, msg).await {
                    tracing::warn!("push write failed: {}", e);
                    break;
                }
            } else {
                break; // push channel closed
            }
        }
    }
}
```

- [ ] **Step 4: Cleanup on disconnect**

After the loop breaks, remove from registry:

```rust
if let Some(ref registry) = push_registry {
    registry.lock().await.remove(&conn_id);
}
```

- [ ] **Step 5: Update all serve() callers to pass None**

Update the existing `serve()` call in `crates/omnish-daemon/src/server.rs:411` to pass `None` for now (will be changed in Task 6):

```rust
server.serve(
    move |msg, tx| { ... },
    Some(auth_token),
    tls_acceptor,
    None, // push_registry - wired in Task 6
).await
```

Also update any test calls in `rpc_server.rs` tests to pass `None`.

- [ ] **Step 6: Build and test**

Run: `cargo build --release -p omnish-transport && cargo test -p omnish-transport --release`
Expected: all existing tests pass (push_registry = None preserves old behavior)

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-transport/src/rpc_server.rs crates/omnish-daemon/src/server.rs
git commit -m "feat(transport): add per-connection push channel registry to RpcServer"
```

---

### Task 5: Add `push_rx` to `RpcClient`

**Files:**
- Modify: `crates/omnish-transport/src/rpc_client.rs:40-45` (Inner struct)
- Modify: `crates/omnish-transport/src/rpc_client.rs:146-181` (create_inner)
- Modify: `crates/omnish-transport/src/rpc_client.rs:514-588` (read_loop)

- [ ] **Step 1: Add push channel to Inner**

In `Inner` struct (line 40), add a push sender:

```rust
struct Inner {
    tx: mpsc::Sender<WriteRequest>,
    connected: Arc<AtomicBool>,
    _write_task: JoinHandle<()>,
    _read_task: JoinHandle<()>,
    push_tx: mpsc::Sender<Message>,
}
```

- [ ] **Step 2: Modify create_inner to create push channel**

In `create_inner` (line 146), add a push channel parameter and pass the sender to `read_loop`:

```rust
fn create_inner<R, W>(
    reader: R,
    writer: W,
    disconnect_tx: Option<oneshot::Sender<()>>,
    push_tx: mpsc::Sender<Message>,
) -> Inner
```

Pass `push_tx.clone()` to `read_loop`:

```rust
let _read_task = tokio::spawn(Self::read_loop(
    reader,
    read_pending,
    read_connected,
    disconnect_tx,
    push_tx.clone(),
));

Inner { tx, connected, _write_task, _read_task, push_tx }
```

- [ ] **Step 3: Handle request_id=0 in read_loop**

In `read_loop` (line 514), add `push_tx: mpsc::Sender<Message>` parameter. Before the pending map lookup (line 536), check for push messages:

```rust
if frame.request_id == 0 {
    // Unsolicited push from server
    let _ = push_tx.send(frame.payload).await;
    continue;
}
```

- [ ] **Step 4: Update all create_inner callers**

Every call to `create_inner` needs to pass a `push_tx`. Add a `push_channel()` method that returns the receiver:

In `RpcClient`:

```rust
/// Create a push channel and return the receiver.
/// The sender is stored inside Inner and fed by read_loop for request_id=0 frames.
fn make_push_channel() -> (mpsc::Sender<Message>, mpsc::Receiver<Message>) {
    mpsc::channel::<Message>(64)
}
```

Update `connect_unix`, `connect_tcp`, `connect` to create and store the push_rx:

In `RpcClient` struct, add:
```rust
push_rx: Arc<Mutex<mpsc::Receiver<Message>>>,
```

Update constructors (`connect_unix` at line 118, `connect_tcp` at line 128):
```rust
let (push_tx, push_rx) = Self::make_push_channel();
let inner = Self::create_inner(reader, writer, None, push_tx);
Ok(Self {
    inner: Arc::new(Mutex::new(inner)),
    next_id: Arc::new(AtomicU64::new(1)),
    push_rx: Arc::new(Mutex::new(push_rx)),
})
```

Update `connect_with_reconnect_full` (line 216) - on reconnect, create new push channel and swap:
```rust
// In initial connection (line 237):
let (push_tx, push_rx) = Self::make_push_channel();
let inner = Self::create_inner(reader, writer, Some(disc_tx), push_tx);
// ...
let client = Self {
    inner: Arc::new(Mutex::new(inner)),
    next_id: next_id.clone(),
    push_rx: Arc::new(Mutex::new(push_rx)),
};
```

In `reconnect_loop`, after creating new inner (line 355), create new push channel and swap:
```rust
let (push_tx, new_push_rx) = Self::make_push_channel();
let new_inner = Self::create_inner(reader, writer, Some(new_disc_tx), push_tx);
```

After swapping inner (line 394), also swap push_rx. Pass `push_rx: Arc<Mutex<mpsc::Receiver<Message>>>` into `reconnect_loop`:
```rust
{
    let mut guard = inner_ref.lock().await;
    *guard = new_inner;
}
// Swap push_rx
{
    let mut rx_guard = push_rx_ref.lock().await;
    *rx_guard = new_push_rx;
}
```

- [ ] **Step 5: Add try_recv_push() method**

```rust
/// Try to receive a push message (non-blocking).
pub async fn try_recv_push(&self) -> Option<Message> {
    let mut rx = self.push_rx.lock().await;
    rx.try_recv().ok()
}
```

- [ ] **Step 6: Handle disconnected client push_rx in initial-failure path**

In the initial connection failure path (line 264), create a dummy push channel:
```rust
let (push_tx, push_rx) = Self::make_push_channel();
// push_tx goes unused, push_rx will be swapped on reconnect
```

- [ ] **Step 7: Build and test**

Run: `cargo build --release -p omnish-transport && cargo test -p omnish-transport --release`
Expected: all tests pass

- [ ] **Step 8: Commit**

```bash
git add crates/omnish-transport/src/rpc_client.rs
git commit -m "feat(transport): add push_rx channel to RpcClient for server-initiated messages"
```

---

### Task 6: Daemon subscribes to ConfigSection::Client and pushes to clients

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs:132-147` (DaemonServer struct)
- Modify: `crates/omnish-daemon/src/server.rs:341-434` (run method)
- Modify: `crates/omnish-daemon/src/main.rs:210-240` (config_watcher subscriber setup)

- [ ] **Step 1: Store PushRegistry in DaemonServer**

Add to `DaemonServer` struct (line 132):

```rust
push_registry: PushRegistry,
```

Initialize in the constructor (wherever `DaemonServer` is built in `main.rs`) with:

```rust
push_registry: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
```

- [ ] **Step 2: Pass push_registry to serve()**

In `run()` (line 411), pass the push registry:

```rust
let push_registry = self.push_registry.clone();
server.serve(
    move |msg, tx| { ... },
    Some(auth_token),
    tls_acceptor,
    Some(push_registry),
).await
```

- [ ] **Step 3: Subscribe to ConfigSection::Client in main.rs**

In `main.rs`, after the ConfigWatcher is created (around line 210), add a subscriber similar to the existing sandbox/llm patterns:

```rust
// Push client config changes to connected clients
{
    let client_rx = config_watcher.subscribe(config_watcher::ConfigSection::Client);
    let push_reg = daemon_server.push_registry.clone();
    tokio::spawn(async move {
        let mut rx = client_rx;
        // Track previous values for diff
        let mut prev = rx.borrow_and_update().client.clone();
        while rx.changed().await.is_ok() {
            let config = rx.borrow_and_update().clone();
            let changes = diff_client_section(&prev, &config.client);
            if !changes.is_empty() {
                let msg = Message::ConfigClient { changes };
                let registry = push_reg.lock().await;
                for (_, push_tx) in registry.iter() {
                    let _ = push_tx.send(msg.clone()).await;
                }
                tracing::info!("pushed client config to {} connections", registry.len());
            }
            prev = config.client.clone();
        }
    });
}
```

- [ ] **Step 4: Implement diff_client_section()**

Add to `crates/omnish-daemon/src/server.rs` (or a helper module):

```rust
fn diff_client_section(
    old: &omnish_common::config::ClientSection,
    new: &omnish_common::config::ClientSection,
) -> Vec<ConfigChange> {
    let mut changes = Vec::new();
    if old.command_prefix != new.command_prefix {
        changes.push(ConfigChange { path: "client.command_prefix".into(), value: new.command_prefix.clone() });
    }
    if old.resume_prefix != new.resume_prefix {
        changes.push(ConfigChange { path: "client.resume_prefix".into(), value: new.resume_prefix.clone() });
    }
    if old.completion_enabled != new.completion_enabled {
        changes.push(ConfigChange { path: "client.completion_enabled".into(), value: new.completion_enabled.to_string() });
    }
    if old.ghost_timeout_ms != new.ghost_timeout_ms {
        changes.push(ConfigChange { path: "client.ghost_timeout_ms".into(), value: new.ghost_timeout_ms.to_string() });
    }
    if old.intercept_gap_ms != new.intercept_gap_ms {
        changes.push(ConfigChange { path: "client.intercept_gap_ms".into(), value: new.intercept_gap_ms.to_string() });
    }
    if old.developer_mode != new.developer_mode {
        changes.push(ConfigChange { path: "client.developer_mode".into(), value: new.developer_mode.to_string() });
    }
    changes
}
```

- [ ] **Step 5: Push full config on auth success**

Also need to push initial config when a client connects. This is best done in the existing `on_reconnect` / auth flow. One approach: after auth succeeds in `spawn_connection`, send a `ConfigClient` with all fields. This requires `spawn_connection` to have access to the current config.

Add a `current_config: Arc<std::sync::RwLock<DaemonConfig>>` parameter to `spawn_connection`. After auth succeeds and push channel is registered, send initial config:

```rust
if let Some(ref config) = current_config {
    let cfg = config.read().unwrap();
    let cs = &cfg.client;
    let changes = vec![
        ConfigChange { path: "client.command_prefix".into(), value: cs.command_prefix.clone() },
        ConfigChange { path: "client.resume_prefix".into(), value: cs.resume_prefix.clone() },
        ConfigChange { path: "client.completion_enabled".into(), value: cs.completion_enabled.to_string() },
        ConfigChange { path: "client.ghost_timeout_ms".into(), value: cs.ghost_timeout_ms.to_string() },
        ConfigChange { path: "client.intercept_gap_ms".into(), value: cs.intercept_gap_ms.to_string() },
        ConfigChange { path: "client.developer_mode".into(), value: cs.developer_mode.to_string() },
    ];
    let msg = Message::ConfigClient { changes };
    let _ = write_reply(&writer, 0, msg).await;
}
```

This needs `serve()` to accept an optional `Arc<std::sync::RwLock<DaemonConfig>>` and pass it through.

- [ ] **Step 6: Build and verify**

Run: `cargo build --release -p omnish-daemon`
Expected: compiles

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-daemon/src/main.rs crates/omnish-transport/src/rpc_server.rs
git commit -m "feat(daemon): subscribe to client config changes and push to connected clients"
```

---

### Task 7: Update config_schema.toml to use `client.*` toml_keys

**Files:**
- Modify: `crates/omnish-daemon/src/config_schema.toml`
- Modify: `crates/omnish-daemon/src/config_schema.rs` (tests)

- [ ] **Step 1: Update toml_keys from shell.* to client.***

Change toml_key values:

```toml
# ── General > Hotkeys ─────────────────────────────────
[[items]]
path = "general.hotkeys.command_prefix"
label = "Enter chat mode"
kind = "text"
toml_key = "client.command_prefix"
default = ":"

[[items]]
path = "general.hotkeys.resume_prefix"
label = "Resume chat"
kind = "text"
toml_key = "client.resume_prefix"
default = "::"

# ── General > Completion ──────────────────────────────
[[items]]
path = "general.shell_completion"
label = "Completion"
kind = "submenu"

[[items]]
path = "general.shell_completion.completion_enabled"
label = "Completion enabled"
kind = "toggle"
toml_key = "client.completion_enabled"
default = "true"

[[items]]
path = "general.shell_completion.ghost_timeout_ms"
label = "Ghost text timeout (ms)"
kind = "text"
toml_key = "client.ghost_timeout_ms"
default = "10000"
```

- [ ] **Step 2: Update config_schema.rs tests**

Update any test assertions that reference `shell.*` toml_keys to use `client.*`.

- [ ] **Step 3: Build and test**

Run: `cargo build --release -p omnish-daemon && cargo test -p omnish-daemon --release -- config_schema`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/config_schema.toml crates/omnish-daemon/src/config_schema.rs
git commit -m "feat(config-schema): update toml_keys from shell.* to client.* namespace"
```

---

### Task 8: Client-side hot-reload - setter methods on InputInterceptor and TimeGapGuard

**Files:**
- Modify: `crates/omnish-client/src/interceptor.rs:197-222` (TimeGapGuard)
- Modify: `crates/omnish-client/src/interceptor.rs:261-290` (InputInterceptor)

- [ ] **Step 1: Add update_min_gap() to TimeGapGuard**

After `should_intercept()` (around line 218):

```rust
pub fn update_min_gap(&mut self, gap: std::time::Duration) {
    self.min_gap = gap;
}
```

Since `TimeGapGuard` is behind `Box<dyn InterceptGuard>`, add this to the `InterceptGuard` trait:

```rust
pub trait InterceptGuard: Send {
    fn note_input(&mut self);
    fn should_intercept(&self) -> bool;
    fn update_min_gap(&mut self, _gap: std::time::Duration) {}
}
```

- [ ] **Step 2: Add setter methods to InputInterceptor**

After the constructor (around line 290):

```rust
pub fn update_prefix(&mut self, prefix: &str) {
    self.prefix = prefix.as_bytes().to_vec();
}

pub fn update_resume_prefix(&mut self, prefix: &str) {
    self.resume_prefix = prefix.as_bytes().to_vec();
}

pub fn set_developer_mode(&mut self, mode: bool) {
    self.developer_mode = mode;
}

pub fn update_min_gap(&mut self, gap: std::time::Duration) {
    self.guard.update_min_gap(gap);
}
```

- [ ] **Step 3: Build and test**

Run: `cargo build --release -p omnish-client && cargo test -p omnish-client --release -- interceptor`
Expected: compiles and existing tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/interceptor.rs
git commit -m "feat(client): add setter methods for hot-reloading InputInterceptor and TimeGapGuard"
```

---

### Task 9: Client main loop handles ConfigClient push

**Files:**
- Modify: `crates/omnish-client/src/main.rs:469` (config loading)
- Modify: `crates/omnish-client/src/main.rs:611-616` (main loop setup)
- Modify: `crates/omnish-client/src/main.rs` (main poll loop)

- [ ] **Step 1: Make hot-reloadable config fields mutable**

At line 614-616, change to mutable variables:

```rust
let mut completion_enabled = config.completion_enabled;
let mut ghost_timeout_ms = config.shell.ghost_timeout_ms;
```

Replace usages of `config.completion_enabled` (line 1368) with `completion_enabled` and `config.shell.ghost_timeout_ms` (line 1455) with `ghost_timeout_ms`.

- [ ] **Step 2: Add push message handling in the main loop**

In the main event loop, after processing PTY output but before sleep/poll, check for push messages from daemon:

```rust
if let Some(ref rpc) = daemon_conn {
    while let Some(msg) = rpc.try_recv_push().await {
        match msg {
            Message::ConfigClient { changes } => {
                apply_client_config_changes(
                    &changes,
                    &mut interceptor,
                    &mut completion_enabled,
                    &mut ghost_timeout_ms,
                    &config, // for client.toml writeback
                );
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 3: Implement apply_client_config_changes()**

```rust
fn apply_client_config_changes(
    changes: &[ConfigChange],
    interceptor: &mut InputInterceptor,
    completion_enabled: &mut bool,
    ghost_timeout_ms: &mut u64,
    config: &ClientConfig,
) {
    let mut updated = false;
    for change in changes {
        match change.path.as_str() {
            "client.command_prefix" => {
                interceptor.update_prefix(&change.value);
                updated = true;
            }
            "client.resume_prefix" => {
                interceptor.update_resume_prefix(&change.value);
                updated = true;
            }
            "client.completion_enabled" => {
                if let Ok(v) = change.value.parse::<bool>() {
                    *completion_enabled = v;
                    updated = true;
                }
            }
            "client.ghost_timeout_ms" => {
                if let Ok(v) = change.value.parse::<u64>() {
                    *ghost_timeout_ms = v;
                    updated = true;
                }
            }
            "client.intercept_gap_ms" => {
                if let Ok(v) = change.value.parse::<u64>() {
                    interceptor.update_min_gap(std::time::Duration::from_millis(v));
                    updated = true;
                }
            }
            "client.developer_mode" => {
                if let Ok(v) = change.value.parse::<bool>() {
                    interceptor.set_developer_mode(v);
                    updated = true;
                }
            }
            _ => {} // unknown paths silently ignored
        }
    }
    if updated {
        // Write back to client.toml
        if let Err(e) = save_client_config_changes(changes) {
            tracing::warn!("failed to save client config: {}", e);
        }
    }
}
```

- [ ] **Step 4: Implement save_client_config_changes()**

Uses `config_edit::update_toml_value` (existing utility) to write values to client.toml:

```rust
fn save_client_config_changes(changes: &[ConfigChange]) -> anyhow::Result<()> {
    let path = std::env::var("OMNISH_CLIENT_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| omnish_common::config::omnish_dir().join("client.toml"));
    for change in changes {
        // Strip "client." prefix for client.toml keys (they're top-level there)
        let key = change.path.strip_prefix("client.").unwrap_or(&change.path);
        omnish_common::config_edit::update_toml_value(&path, key, &change.value)?;
    }
    Ok(())
}
```

- [ ] **Step 5: Adjust the sync main loop**

The client uses synchronous `poll()` for I/O. `try_recv_push()` is async. Since the client already has a tokio runtime for daemon calls, use `block_on` or restructure. Check how existing daemon calls are made - likely via `tokio::runtime::Handle::current().block_on()` or similar.

If the client main loop is sync with `poll()`, add the push check as part of the existing poll cycle (similar to how completion responses are checked with `try_recv`).

- [ ] **Step 6: Build**

Run: `cargo build --release -p omnish-client`
Expected: compiles

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat(client): handle ConfigClient push - hot-reload settings and cache to client.toml"
```

---

### Task 10: Remove ConfigUpdate reload hack and update prefix_bytes

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs:876-892` (ConfigUpdate handler)
- Modify: `crates/omnish-client/src/main.rs:616` (prefix_bytes)

- [ ] **Step 1: Keep manual config reload in ConfigUpdate handler**

In `server.rs:876-892`, the ConfigUpdate handler reloads `opts.daemon_config` after writing. This must stay because `opts.daemon_config` is a separate `Arc<RwLock<DaemonConfig>>` used by ConfigQuery and other handlers. ConfigWatcher has its own copy. No change needed here - the existing flow is correct.

- [ ] **Step 2: Make prefix_bytes track interceptor updates**

At line 616, `prefix_bytes` is a one-time binding. Either remove it if unused outside interceptor, or make it derive from the same source. Check usages and update accordingly.

- [ ] **Step 3: Build and test full workspace**

Run: `cargo build --release && cargo test --release`
Expected: all builds, all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-daemon/src/server.rs crates/omnish-client/src/main.rs
git commit -m "refactor: remove redundant config reload in ConfigUpdate handler, let ConfigWatcher handle it"
```

---

### Task 11: Integration test

**Files:**
- Modify: `tools/integration_tests/` (new test or extend existing)

- [ ] **Step 1: Read existing integration test patterns**

Read `tools/integration_tests/lib.sh` and `tools/integration_tests/test_basic.sh` to understand test patterns.

- [ ] **Step 2: Write integration test**

Test: change a `[client]` setting via `/config`, verify the client receives the push and applies it. This may require a test helper that writes directly to daemon.toml and checks client behavior, since `/config` is interactive.

Alternative: write a daemon.toml with `[client] command_prefix = "/"`, start client, verify the prefix is loaded from daemon push (check client.toml is updated).

- [ ] **Step 3: Run test**

Run the integration test.
Expected: passes

- [ ] **Step 4: Commit**

```bash
git add tools/integration_tests/
git commit -m "test: integration test for client config push"
```
