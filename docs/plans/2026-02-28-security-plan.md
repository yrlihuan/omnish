# Daemon/Client Communication Security Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add layered security to omnish daemon/client communication: shared token auth + Unix socket permissions + SO_PEERCRED + TCP TLS.

**Architecture:** Token stored in `~/.omnish/auth_token` (0600). Client sends `Auth` message after connect. Server validates before accepting other messages. Unix socket set to 0600 with peer UID check. TCP wrapped in TLS using self-signed cert from `~/.omnish/tls/`.

**Tech Stack:** rcgen (cert generation), tokio-rustls (TLS), rand (token), nix (SO_PEERCRED/fchmod)

---

### Task 1: Add Auth/AuthFailed message variants to protocol

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs:8-22`
- Test: `crates/omnish-protocol/src/message.rs` (existing serialization tests)

**Step 1: Add Auth and AuthFailed structs and enum variants**

In `crates/omnish-protocol/src/message.rs`, add after the existing structs (before `impl Frame`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Auth {
    pub token: String,
}
```

Add variants to the END of the `Message` enum (after `Ack`):

```rust
    Ack,
    Auth(Auth),
    AuthFailed,
```

**Step 2: Run tests to verify serialization compatibility**

Run: `cargo test -p omnish-protocol`
Expected: All existing tests pass (new variants added at end preserve bincode indices).

**Step 3: Commit**

```
feat(protocol): add Auth and AuthFailed message variants
```

---

### Task 2: Add token generation and loading utilities

**Files:**
- Create: `crates/omnish-common/src/auth.rs`
- Modify: `crates/omnish-common/src/lib.rs` (add `pub mod auth;`)
- Modify: `crates/omnish-common/Cargo.toml` (add `rand` dependency)

**Step 1: Add rand dependency**

In `crates/omnish-common/Cargo.toml`, add:
```toml
rand = "0.8"
```

**Step 2: Write tests for token generation and loading**

Create `crates/omnish-common/src/auth.rs`:

```rust
use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::config::omnish_dir;

const TOKEN_BYTES: usize = 32;

/// Return the default auth token path: ~/.omnish/auth_token
pub fn default_token_path() -> PathBuf {
    omnish_dir().join("auth_token")
}

/// Load existing token from file, or generate a new one if it doesn't exist.
/// The file is created with permission 0600.
pub fn load_or_create_token(path: &Path) -> Result<String> {
    if path.exists() {
        let token = std::fs::read_to_string(path)?.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    // Generate new token
    let token = generate_token();

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Write with 0600 permissions
    std::fs::write(path, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(token)
}

/// Generate a random 32-byte hex token.
fn generate_token() -> String {
    use rand::Rng;
    let bytes: [u8; TOKEN_BYTES] = rand::thread_rng().gen();
    hex::encode(bytes)
}

/// Load token from file. Returns error if file doesn't exist or is empty.
pub fn load_token(path: &Path) -> Result<String> {
    let token = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read auth token from {}: {}", path.display(), e))?
        .trim()
        .to_string();
    if token.is_empty() {
        anyhow::bail!("auth token file {} is empty", path.display());
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_token_length() {
        let token = generate_token();
        assert_eq!(token.len(), TOKEN_BYTES * 2); // hex encoding doubles length
    }

    #[test]
    fn test_generate_token_unique() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
    }

    #[test]
    fn test_load_or_create_token_creates_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth_token");
        let token = load_or_create_token(&path).unwrap();
        assert_eq!(token.len(), TOKEN_BYTES * 2);
        assert!(path.exists());
    }

    #[test]
    fn test_load_or_create_token_reuses_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth_token");
        let token1 = load_or_create_token(&path).unwrap();
        let token2 = load_or_create_token(&path).unwrap();
        assert_eq!(token1, token2);
    }

    #[test]
    fn test_load_token_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent");
        assert!(load_token(&path).is_err());
    }
}
```

Also add `hex = "0.4"` to `crates/omnish-common/Cargo.toml`.

**Step 3: Add `pub mod auth;` to `crates/omnish-common/src/lib.rs`**

**Step 4: Run tests**

Run: `cargo test -p omnish-common`
Expected: All tests pass.

**Step 5: Commit**

```
feat(common): add auth token generation and loading utilities
```

---

### Task 3: Add authentication to RPC server (spawn_connection)

**Files:**
- Modify: `crates/omnish-transport/src/rpc_server.rs:75-127`
- Modify: `crates/omnish-transport/src/rpc_server.rs:52-72` (serve function)

**Step 1: Add auth_token parameter to serve() and spawn_connection()**

Modify `serve()` to accept an optional auth token:

```rust
pub async fn serve<F>(&mut self, handler: F, auth_token: Option<String>) -> Result<()>
where
    F: Fn(Message) -> Pin<Box<dyn Future<Output = Message> + Send>> + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    let auth_token = auth_token.map(Arc::new);
    loop {
        match &self.listener {
            Listener::Unix(l) => {
                let (stream, _) = l.accept().await?;
                let (reader, writer) = stream.into_split();
                spawn_connection(reader, writer, handler.clone(), auth_token.clone());
            }
            Listener::Tcp(l) => {
                let (stream, _) = l.accept().await?;
                stream.set_nodelay(true)?;
                let (reader, writer) = stream.into_split();
                spawn_connection(reader, writer, handler.clone(), auth_token.clone());
            }
        }
    }
}
```

**Step 2: Add authentication check in spawn_connection()**

Modify `spawn_connection()` to validate the first message as `Auth`:

```rust
fn spawn_connection<R, W, F>(
    reader: R,
    writer: W,
    handler: Arc<F>,
    auth_token: Option<Arc<String>>,
)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    F: Fn(Message) -> Pin<Box<dyn Future<Output = Message> + Send>> + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let mut reader = reader;
        let writer = Arc::new(Mutex::new(writer));

        // Authentication phase
        if let Some(ref expected_token) = auth_token {
            let auth_timeout = tokio::time::Duration::from_secs(5);
            let authenticated = match tokio::time::timeout(auth_timeout, read_frame(&mut reader)).await {
                Ok(Ok(frame)) => {
                    if let Message::Auth(auth) = &frame.payload {
                        if auth.token == **expected_token {
                            // Send Ack
                            let _ = write_reply(&writer, frame.request_id, Message::Ack).await;
                            true
                        } else {
                            tracing::warn!("auth failed: invalid token");
                            let _ = write_reply(&writer, frame.request_id, Message::AuthFailed).await;
                            false
                        }
                    } else {
                        tracing::warn!("auth failed: expected Auth message, got {:?}", std::mem::discriminant(&frame.payload));
                        let _ = write_reply(&writer, frame.request_id, Message::AuthFailed).await;
                        false
                    }
                }
                Ok(Err(_)) => {
                    tracing::debug!("connection closed before auth");
                    false
                }
                Err(_) => {
                    tracing::warn!("auth timeout (5s)");
                    false
                }
            };
            if !authenticated {
                return;
            }
        }

        // Normal message loop (existing code)
        loop {
            let frame = match read_frame(&mut reader).await {
                Ok(f) => f,
                Err(_) => break,
            };

            let handler = handler.clone();
            let writer = writer.clone();
            tokio::spawn(async move {
                let response_payload = handler(frame.payload).await;
                let _ = write_reply(&writer, frame.request_id, response_payload).await;
            });
        }
    });
}
```

Extract helper functions `read_frame` and `write_reply` from the inline code:

```rust
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
    let reply = Frame { request_id, payload };
    let bytes = reply.to_bytes()?;
    let mut w = writer.lock().await;
    w.write_u32(bytes.len() as u32).await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}
```

**Step 3: Run tests**

Run: `cargo test -p omnish-transport`
Expected: Existing tests may need updating (serve() signature changed). Update callers to pass `None` for auth_token to preserve existing behavior.

**Step 4: Commit**

```
feat(transport): add token authentication to RPC server
```

---

### Task 4: Set Unix socket permissions and add SO_PEERCRED verification

**Files:**
- Modify: `crates/omnish-transport/src/rpc_server.rs:22-28` (bind_unix)
- Modify: `crates/omnish-transport/src/rpc_server.rs` (serve, Unix accept)
- Modify: `crates/omnish-transport/Cargo.toml` (add nix dependency)

**Step 1: Add nix dependency**

In `crates/omnish-transport/Cargo.toml`:
```toml
[target.'cfg(unix)'.dependencies]
nix = { version = "0.29", features = ["socket", "user"] }
```

**Step 2: Set socket permissions to 0600 after bind**

```rust
pub async fn bind_unix(addr: &str) -> Result<Self> {
    let _ = std::fs::remove_file(addr);
    let listener = TokioUnixListener::bind(addr)?;
    // Set socket permissions to owner-only
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(addr, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(Self {
        listener: Listener::Unix(listener),
    })
}
```

**Step 3: Add peer credential check on Unix accept**

In `serve()`, after accepting a Unix connection, verify the peer UID:

```rust
Listener::Unix(l) => {
    let (stream, _) = l.accept().await?;
    // Verify peer credentials (same UID)
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
```

**Step 4: Run tests**

Run: `cargo test -p omnish-transport`
Expected: All tests pass.

**Step 5: Commit**

```
feat(transport): set socket permissions to 0600 and verify peer UID
```

---

### Task 5: Add TLS support for TCP connections

**Files:**
- Modify: `crates/omnish-transport/Cargo.toml` (add tokio-rustls, rcgen, rustls-pemfile)
- Create: `crates/omnish-transport/src/tls.rs`
- Modify: `crates/omnish-transport/src/lib.rs` (add `pub mod tls;`)
- Modify: `crates/omnish-transport/src/rpc_server.rs` (TCP accept with TLS)
- Modify: `crates/omnish-transport/src/rpc_client.rs` (TCP connect with TLS)

**Step 1: Add dependencies**

In `crates/omnish-transport/Cargo.toml`:
```toml
tokio-rustls = "0.26"
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
rustls-pemfile = "2"
rcgen = "0.13"
```

**Step 2: Create TLS utilities module**

Create `crates/omnish-transport/src/tls.rs`:

```rust
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Return the default TLS directory: ~/.omnish/tls/
pub fn default_tls_dir() -> PathBuf {
    omnish_common::config::omnish_dir().join("tls")
}

/// Load or generate self-signed certificate and key.
/// Saves to cert_path and key_path with 0600 permissions.
pub fn load_or_create_cert(tls_dir: &Path) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_path = tls_dir.join("cert.pem");
    let key_path = tls_dir.join("key.pem");

    if cert_path.exists() && key_path.exists() {
        return load_cert_and_key(&cert_path, &key_path);
    }

    // Generate self-signed cert
    std::fs::create_dir_all(tls_dir)?;

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();

    std::fs::write(&cert_path, &cert_pem)?;
    std::fs::write(&key_path, &key_pem)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cert_path, std::fs::Permissions::from_mode(0o600))?;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
    }

    load_cert_and_key(&cert_path, &key_path)
}

fn load_cert_and_key(cert_path: &Path, key_path: &Path) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_pem = std::fs::read(cert_path)?;
    let key_pem = std::fs::read(key_path)?;

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut &key_pem[..])?
        .ok_or_else(|| anyhow::anyhow!("no private key found in {}", key_path.display()))?;

    Ok((certs, key))
}

/// Create a TLS acceptor for the server.
pub fn make_acceptor(tls_dir: &Path) -> Result<TlsAcceptor> {
    let (certs, key) = load_or_create_cert(tls_dir)?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Create a TLS connector that trusts the daemon's self-signed cert.
pub fn make_connector(cert_path: &Path) -> Result<TlsConnector> {
    let cert_pem = std::fs::read(cert_path)?;
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<Result<Vec<_>, _>>()?;

    let mut root_store = rustls::RootCertStore::empty();
    for cert in certs {
        root_store.add(cert)?;
    }

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_or_create_cert_generates_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let tls_dir = dir.path().join("tls");

        // First call generates
        let (certs1, _) = load_or_create_cert(&tls_dir).unwrap();
        assert!(!certs1.is_empty());

        // Second call reloads same cert
        let (certs2, _) = load_or_create_cert(&tls_dir).unwrap();
        assert_eq!(certs1[0].as_ref(), certs2[0].as_ref());
    }

    #[test]
    fn test_make_acceptor() {
        let dir = tempfile::tempdir().unwrap();
        let tls_dir = dir.path().join("tls");
        let acceptor = make_acceptor(&tls_dir);
        assert!(acceptor.is_ok());
    }

    #[test]
    fn test_make_connector() {
        let dir = tempfile::tempdir().unwrap();
        let tls_dir = dir.path().join("tls");
        load_or_create_cert(&tls_dir).unwrap();
        let cert_path = tls_dir.join("cert.pem");
        let connector = make_connector(&cert_path);
        assert!(connector.is_ok());
    }
}
```

**Step 3: Integrate TLS into serve() for TCP**

Add an optional `tls_acceptor` parameter to `serve()`:

```rust
pub async fn serve<F>(
    &mut self,
    handler: F,
    auth_token: Option<String>,
    tls_acceptor: Option<TlsAcceptor>,
) -> Result<()>
```

In the TCP accept branch:
```rust
Listener::Tcp(l) => {
    let (stream, _) = l.accept().await?;
    stream.set_nodelay(true)?;
    if let Some(ref acceptor) = tls_acceptor {
        match acceptor.accept(stream).await {
            Ok(tls_stream) => {
                let (reader, writer) = tokio::io::split(tls_stream);
                spawn_connection(reader, writer, handler.clone(), auth_token.clone());
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
```

**Step 4: Integrate TLS into make_connector for client**

Modify the `make_connector` function in `rpc_client.rs` to accept an optional `TlsConnector`:

For TCP connections, wrap the stream with TLS before splitting:
```rust
TransportAddr::Tcp(hp) => {
    let stream = TcpStream::connect(&hp).await?;
    stream.set_nodelay(true)?;
    if let Some(ref tls) = tls_connector {
        let domain = rustls::pki_types::ServerName::try_from("localhost")?;
        let tls_stream = tls.connect(domain, stream).await?;
        let (r, w) = tokio::io::split(tls_stream);
        Ok((Box::new(r) as _, Box::new(w) as _))
    } else {
        let (r, w) = stream.into_split();
        Ok((Box::new(r) as _, Box::new(w) as _))
    }
}
```

**Step 5: Run tests**

Run: `cargo test -p omnish-transport`
Expected: All tests pass. TCP tests may need updating for optional TLS.

**Step 6: Commit**

```
feat(transport): add TLS support for TCP connections
```

---

### Task 6: Wire authentication into daemon startup

**Files:**
- Modify: `crates/omnish-daemon/src/main.rs`
- Modify: `crates/omnish-daemon/src/server.rs` (DaemonServer::run passes auth_token)
- Modify: `crates/omnish-daemon/Cargo.toml` (add omnish-common if not present)

**Step 1: Generate/load token and TLS cert at daemon startup**

In `main.rs` `async_main()`, after loading config:

```rust
// Load or create auth token
let token_path = omnish_common::auth::default_token_path();
let auth_token = omnish_common::auth::load_or_create_token(&token_path)?;
tracing::info!("auth token loaded from {}", token_path.display());

// Load or create TLS cert (for TCP mode)
let tls_dir = omnish_transport::tls::default_tls_dir();
let tls_acceptor = if socket_path.contains(':') {
    // TCP mode - enable TLS
    Some(omnish_transport::tls::make_acceptor(&tls_dir)?)
} else {
    None
};
```

**Step 2: Pass auth_token and tls_acceptor to DaemonServer::run()**

Modify `DaemonServer::run()` in `server.rs`:

```rust
pub async fn run(self, addr: &str, auth_token: String, tls_acceptor: Option<TlsAcceptor>) -> Result<()> {
    let mut server = RpcServer::bind(addr).await?;
    // ... existing handler setup ...
    server.serve(handler, Some(auth_token), tls_acceptor).await
}
```

Update the call in `main.rs`:

```rust
server.run(&socket_path, auth_token, tls_acceptor).await
```

**Step 3: Run tests**

Run: `cargo test -p omnish-daemon`
Expected: All tests pass.

**Step 4: Commit**

```
feat(daemon): wire auth token and TLS into daemon startup
```

---

### Task 7: Wire authentication into client connection

**Files:**
- Modify: `crates/omnish-client/src/main.rs:779-838` (connect_daemon function)

**Step 1: Load token and send Auth in on_reconnect callback**

In `connect_daemon()`, load the token and send Auth before SessionStart:

```rust
async fn connect_daemon(
    daemon_addr: &str,
    session_id: &str,
    parent_session_id: Option<String>,
    child_pid: u32,
    buffer: MessageBuffer,
) -> Option<RpcClient> {
    let socket_path = daemon_addr.to_string();
    let sid = session_id.to_string();
    let psid = parent_session_id.clone();

    // Load auth token
    let token_path = omnish_common::auth::default_token_path();
    let auth_token = match omnish_common::auth::load_token(&token_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("\x1b[33m[omnish]\x1b[0m Failed to load auth token: {}", e);
            eprintln!("\x1b[33m[omnish]\x1b[0m Running in passthrough mode (no daemon)");
            return None;
        }
    };

    match RpcClient::connect_with_reconnect(
        &socket_path,
        move |rpc| {
            let sid = sid.clone();
            let psid = psid.clone();
            let rpc = rpc.clone();
            let buffer = buffer.clone();
            let token = auth_token.clone();
            Box::pin(async move {
                // Authenticate first
                let auth_resp = rpc.call(Message::Auth(Auth { token })).await?;
                if matches!(auth_resp, Message::AuthFailed) {
                    anyhow::bail!("authentication failed");
                }

                // Then register session
                let attrs = probe::default_session_probes(child_pid).collect_all();
                rpc.call(Message::SessionStart(SessionStart {
                    session_id: sid,
                    parent_session_id: psid,
                    timestamp_ms: timestamp_ms(),
                    attrs,
                })).await?;

                // Replay buffered messages
                let buffered: Vec<Message> = {
                    buffer.lock().await.drain(..).collect()
                };
                for msg in buffered {
                    if rpc.call(msg).await.is_err() {
                        break;
                    }
                }
                Ok(())
            })
        },
    ).await {
        // ... existing match arms unchanged ...
    }
}
```

For TLS on the client side, modify the connector in `connect_with_reconnect` to use TLS when connecting via TCP. This can be done by checking if a cert file exists in `~/.omnish/tls/cert.pem` and creating a TLS connector.

**Step 2: Build and test end-to-end**

Run: `cargo build --workspace`
Expected: Compiles without errors.

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 3: Commit**

```
feat(client): send Auth message on connect and reconnect
```

---

### Task 8: Integration test

**Files:**
- Modify: `crates/omnish-transport/src/rpc_server.rs` or `crates/omnish-transport/src/rpc_client.rs` (add test)

**Step 1: Write integration test for auth flow**

```rust
#[tokio::test]
async fn test_auth_required_and_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("auth.sock");
    let sock_str = sock.to_str().unwrap().to_string();
    let token = "test-secret-token".to_string();

    let mut server = RpcServer::bind_unix(&sock_str).await.unwrap();
    let server_token = token.clone();

    let server_task = tokio::spawn(async move {
        server.serve(
            |msg| Box::pin(async move { Message::Ack }),
            Some(server_token),
            None,
        ).await.unwrap();
    });

    // Client with correct token
    let client = RpcClient::connect_unix(&sock_str).await.unwrap();
    let resp = client.call(Message::Auth(Auth { token: token.clone() })).await.unwrap();
    assert!(matches!(resp, Message::Ack));

    // Now normal messages should work
    let resp = client.call(Message::SessionStart(SessionStart {
        session_id: "s1".into(),
        parent_session_id: None,
        timestamp_ms: 1000,
        attrs: HashMap::new(),
    })).await.unwrap();
    assert!(matches!(resp, Message::Ack));

    server_task.abort();
}

#[tokio::test]
async fn test_auth_wrong_token_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("auth_fail.sock");
    let sock_str = sock.to_str().unwrap().to_string();

    let mut server = RpcServer::bind_unix(&sock_str).await.unwrap();

    let server_task = tokio::spawn(async move {
        server.serve(
            |msg| Box::pin(async move { Message::Ack }),
            Some("correct-token".to_string()),
            None,
        ).await.unwrap();
    });

    let client = RpcClient::connect_unix(&sock_str).await.unwrap();
    let resp = client.call(Message::Auth(Auth { token: "wrong-token".into() })).await;
    // Should get AuthFailed or connection closed
    match resp {
        Ok(Message::AuthFailed) => {} // expected
        Err(_) => {} // also acceptable (connection closed)
        other => panic!("expected AuthFailed or error, got {:?}", other),
    }

    server_task.abort();
}
```

**Step 2: Run tests**

Run: `cargo test -p omnish-transport`
Expected: All tests pass including new auth tests.

**Step 3: Final full workspace test**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 4: Commit**

```
test(transport): add integration tests for auth flow
```
