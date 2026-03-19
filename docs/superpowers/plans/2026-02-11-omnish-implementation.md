# omnish Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a transparent shell wrapper (omnish) that captures I/O via PTY proxy, aggregates sessions across terminals via a daemon, and integrates remote LLMs for analysis.

**Architecture:** Client-Daemon over Unix socket. Client uses `forkpty()` for transparent PTY proxy. Daemon manages sessions, stores raw I/O streams, detects events, and dispatches LLM queries. Communication layer abstracted behind traits for future TCP/HTTP.

**Tech Stack:** Rust, tokio, nix (PTY/signals), serde + bincode (protocol), reqwest (LLM HTTP), toml (config)

---

### Task 1: Scaffold Cargo Workspace

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/omnish-common/Cargo.toml` + `crates/omnish-common/src/lib.rs`
- Create: `crates/omnish-protocol/Cargo.toml` + `crates/omnish-protocol/src/lib.rs`
- Create: `crates/omnish-transport/Cargo.toml` + `crates/omnish-transport/src/lib.rs`
- Create: `crates/omnish-pty/Cargo.toml` + `crates/omnish-pty/src/lib.rs`
- Create: `crates/omnish-store/Cargo.toml` + `crates/omnish-store/src/lib.rs`
- Create: `crates/omnish-llm/Cargo.toml` + `crates/omnish-llm/src/lib.rs`
- Create: `crates/omnish-daemon/Cargo.toml` + `crates/omnish-daemon/src/main.rs`
- Create: `crates/omnish-client/Cargo.toml` + `crates/omnish-client/src/main.rs`

**Step 1: Create workspace Cargo.toml**

```toml
[workspace]
resolver = "2"
members = [
    "crates/omnish-common",
    "crates/omnish-protocol",
    "crates/omnish-transport",
    "crates/omnish-pty",
    "crates/omnish-store",
    "crates/omnish-llm",
    "crates/omnish-daemon",
    "crates/omnish-client",
]

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
bincode = "1"
nix = { version = "0.29", features = ["term", "pty", "signal", "process"] }
toml = "0.8"
reqwest = { version = "0.12", features = ["json"] }
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
```

**Step 2: Create each crate with minimal Cargo.toml and empty src**

Each library crate gets:
```toml
[package]
name = "omnish-<name>"
version = "0.1.0"
edition = "2021"

[dependencies]
```

Each binary crate (`omnish-client`, `omnish-daemon`) gets a `main.rs` with:
```rust
fn main() {
    println!("omnish-<name> placeholder");
}
```

Each library crate gets a `lib.rs` with:
```rust
// omnish-<name>
```

**Step 3: Verify workspace builds**

Run: `cargo build`
Expected: Compiles successfully with no errors.

**Step 4: Commit**

```bash
git add -A
git commit -m "scaffold: initialize Cargo workspace with all crates"
```

---

### Task 2: omnish-common — Config Types and Shared Utilities

**Files:**
- Create: `crates/omnish-common/src/config.rs`
- Create: `crates/omnish-common/src/error.rs`
- Create: `config/default.toml`
- Modify: `crates/omnish-common/Cargo.toml`
- Modify: `crates/omnish-common/src/lib.rs`
- Test: `crates/omnish-common/tests/config_test.rs`

**Step 1: Write failing test for config parsing**

```rust
// crates/omnish-common/tests/config_test.rs
use omnish_common::config::OmnishConfig;

#[test]
fn test_parse_default_config() {
    let toml_str = r#"
[shell]
command = "/bin/bash"
command_prefix = "::"

[daemon]
socket_path = "/tmp/omnish.sock"

[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-sonnet-4-5-20250929"
api_key_cmd = "echo test-key"

[llm.auto_trigger]
on_nonzero_exit = true
on_stderr_patterns = ["error", "panic", "traceback", "fatal"]
cooldown_seconds = 5
"#;
    let config: OmnishConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.shell.command, "/bin/bash");
    assert_eq!(config.shell.command_prefix, "::");
    assert_eq!(config.llm.default, "claude");
    assert!(config.llm.auto_trigger.on_nonzero_exit);
    assert_eq!(config.llm.auto_trigger.cooldown_seconds, 5);
}

#[test]
fn test_config_defaults() {
    let toml_str = r#"
[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-sonnet-4-5-20250929"
api_key_cmd = "echo key"
"#;
    let config: OmnishConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.shell.command_prefix, "::");
    assert!(!config.llm.auto_trigger.on_nonzero_exit);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-common`
Expected: FAIL — `config` module not found.

**Step 3: Implement config types**

```rust
// crates/omnish-common/src/config.rs
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct OmnishConfig {
    #[serde(default)]
    pub shell: ShellConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    pub llm: LlmConfig,
}

#[derive(Debug, Deserialize)]
pub struct ShellConfig {
    #[serde(default = "default_shell_command")]
    pub command: String,
    #[serde(default = "default_command_prefix")]
    pub command_prefix: String,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            command: default_shell_command(),
            command_prefix: default_command_prefix(),
        }
    }
}

fn default_shell_command() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

fn default_command_prefix() -> String {
    "::".to_string()
}

#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_socket_path")]
    pub socket_path: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
        }
    }
}

fn default_socket_path() -> String {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        format!("{}/omnish.sock", runtime_dir)
    } else {
        "/tmp/omnish.sock".to_string()
    }
}

#[derive(Debug, Deserialize)]
pub struct LlmConfig {
    pub default: String,
    #[serde(default)]
    pub backends: HashMap<String, LlmBackendConfig>,
    #[serde(default)]
    pub auto_trigger: AutoTriggerConfig,
}

#[derive(Debug, Deserialize)]
pub struct LlmBackendConfig {
    pub backend_type: String,
    pub model: String,
    #[serde(default)]
    pub api_key_cmd: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct AutoTriggerConfig {
    #[serde(default)]
    pub on_nonzero_exit: bool,
    #[serde(default)]
    pub on_stderr_patterns: Vec<String>,
    #[serde(default = "default_cooldown")]
    pub cooldown_seconds: u64,
}

fn default_cooldown() -> u64 {
    5
}
```

```rust
// crates/omnish-common/src/lib.rs
pub mod config;
```

Update `crates/omnish-common/Cargo.toml` dependencies:
```toml
[dependencies]
serde = { workspace = true }
toml = { workspace = true }
anyhow = { workspace = true }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-common`
Expected: PASS

**Step 5: Create default.toml**

```toml
# config/default.toml
# Default omnish configuration — copy to ~/.config/omnish/config.toml

[shell]
# command = "/bin/bash"    # defaults to $SHELL
command_prefix = "::"

[daemon]
# socket_path = "$XDG_RUNTIME_DIR/omnish.sock"

[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-sonnet-4-5-20250929"
api_key_cmd = "pass show anthropic/api-key"

# [llm.backends.openai]
# backend_type = "openai_compat"
# model = "gpt-4o"
# api_key_cmd = "pass show openai/api-key"
# base_url = "https://api.openai.com/v1"

[llm.auto_trigger]
on_nonzero_exit = true
on_stderr_patterns = ["error", "panic", "traceback", "fatal"]
cooldown_seconds = 5
```

**Step 6: Commit**

```bash
git add -A
git commit -m "feat(common): add config types and default configuration"
```

---

### Task 3: omnish-protocol — Message Types and Serialization

**Files:**
- Create: `crates/omnish-protocol/src/message.rs`
- Modify: `crates/omnish-protocol/Cargo.toml`
- Modify: `crates/omnish-protocol/src/lib.rs`
- Test: `crates/omnish-protocol/tests/message_test.rs`

**Step 1: Write failing test for message serialization roundtrip**

```rust
// crates/omnish-protocol/tests/message_test.rs
use omnish_protocol::message::*;

#[test]
fn test_session_start_roundtrip() {
    let msg = Message::SessionStart(SessionStart {
        session_id: "abc123".to_string(),
        shell: "/bin/bash".to_string(),
        pid: 1234,
        tty: "/dev/pts/0".to_string(),
        timestamp_ms: 1707600000000,
    });
    let bytes = msg.to_bytes().unwrap();
    let decoded = Message::from_bytes(&bytes).unwrap();
    match decoded {
        Message::SessionStart(s) => {
            assert_eq!(s.session_id, "abc123");
            assert_eq!(s.pid, 1234);
        }
        _ => panic!("wrong message type"),
    }
}

#[test]
fn test_io_data_roundtrip() {
    let msg = Message::IoData(IoData {
        session_id: "abc123".to_string(),
        direction: IoDirection::Output,
        timestamp_ms: 1707600000000,
        data: b"hello world\n".to_vec(),
    });
    let bytes = msg.to_bytes().unwrap();
    let decoded = Message::from_bytes(&bytes).unwrap();
    match decoded {
        Message::IoData(io) => {
            assert_eq!(io.data, b"hello world\n");
            assert_eq!(io.direction, IoDirection::Output);
        }
        _ => panic!("wrong message type"),
    }
}

#[test]
fn test_frame_magic_validation() {
    let bad_bytes = vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    assert!(Message::from_bytes(&bad_bytes).is_err());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-protocol`
Expected: FAIL — module not found.

**Step 3: Implement message types**

```rust
// crates/omnish-protocol/src/message.rs
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

const MAGIC: [u8; 2] = [0x4F, 0x53]; // "OS" for OmniSh

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    SessionStart(SessionStart),
    SessionEnd(SessionEnd),
    IoData(IoData),
    Event(Event),
    Request(Request),
    Response(Response),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStart {
    pub session_id: String,
    pub shell: String,
    pub pid: u32,
    pub tty: String,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEnd {
    pub session_id: String,
    pub timestamp_ms: u64,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoData {
    pub session_id: String,
    pub direction: IoDirection,
    pub timestamp_ms: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IoDirection {
    Input,
    Output,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub session_id: String,
    pub timestamp_ms: u64,
    pub event_type: EventType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventType {
    NonZeroExit(i32),
    PatternMatch(String),
    CommandBoundary { command: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub request_id: String,
    pub session_id: String,
    pub query: String,
    pub scope: RequestScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequestScope {
    CurrentSession,
    AllSessions,
    Sessions(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub request_id: String,
    pub content: String,
    pub is_streaming: bool,
    pub is_final: bool,
}

impl Message {
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let payload = bincode::serialize(self)?;
        let len = payload.len() as u32;
        let mut buf = Vec::with_capacity(2 + 4 + payload.len());
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&payload);
        Ok(buf)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 6 {
            bail!("message too short");
        }
        if bytes[0..2] != MAGIC {
            bail!("invalid magic bytes");
        }
        let len = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as usize;
        if bytes.len() < 6 + len {
            bail!("message truncated");
        }
        let msg: Message = bincode::deserialize(&bytes[6..6 + len])?;
        Ok(msg)
    }
}
```

```rust
// crates/omnish-protocol/src/lib.rs
pub mod message;
```

Update `crates/omnish-protocol/Cargo.toml`:
```toml
[dependencies]
serde = { workspace = true }
bincode = { workspace = true }
anyhow = { workspace = true }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-protocol`
Expected: PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(protocol): add message types and binary serialization"
```

---

### Task 4: omnish-transport — Transport Trait and Unix Socket Implementation

**Files:**
- Create: `crates/omnish-transport/src/traits.rs`
- Create: `crates/omnish-transport/src/unix.rs`
- Modify: `crates/omnish-transport/Cargo.toml`
- Modify: `crates/omnish-transport/src/lib.rs`
- Test: `crates/omnish-transport/tests/unix_test.rs`

**Step 1: Write failing test for unix socket transport**

```rust
// crates/omnish-transport/tests/unix_test.rs
use omnish_protocol::message::*;
use omnish_transport::unix::UnixTransport;
use omnish_transport::traits::{Transport, Listener};
use std::time::Duration;
use tempfile::tempdir;

#[tokio::test]
async fn test_unix_send_recv() {
    let dir = tempdir().unwrap();
    let sock_path = dir.path().join("test.sock");
    let addr = sock_path.to_str().unwrap().to_string();

    let transport = UnixTransport;

    let mut listener = transport.listen(&addr).await.unwrap();

    let addr2 = addr.clone();
    let handle = tokio::spawn(async move {
        let conn = UnixTransport.connect(&addr2).await.unwrap();
        let msg = Message::SessionStart(SessionStart {
            session_id: "test".to_string(),
            shell: "/bin/bash".to_string(),
            pid: 42,
            tty: "/dev/pts/0".to_string(),
            timestamp_ms: 0,
        });
        conn.send(&msg).await.unwrap();
    });

    let conn = listener.accept().await.unwrap();
    let msg = conn.recv().await.unwrap();
    match msg {
        Message::SessionStart(s) => assert_eq!(s.session_id, "test"),
        _ => panic!("wrong message type"),
    }

    handle.await.unwrap();
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-transport`
Expected: FAIL — module not found.

**Step 3: Implement transport trait and unix socket**

```rust
// crates/omnish-transport/src/traits.rs
use anyhow::Result;
use async_trait::async_trait;
use omnish_protocol::message::Message;

#[async_trait]
pub trait Transport: Send + Sync {
    async fn connect(&self, addr: &str) -> Result<Box<dyn Connection>>;
    async fn listen(&self, addr: &str) -> Result<Box<dyn Listener>>;
}

#[async_trait]
pub trait Connection: Send + Sync {
    async fn send(&self, msg: &Message) -> Result<()>;
    async fn recv(&self) -> Result<Message>;
}

#[async_trait]
pub trait Listener: Send + Sync {
    async fn accept(&mut self) -> Result<Box<dyn Connection>>;
}
```

```rust
// crates/omnish-transport/src/unix.rs
use crate::traits::{Connection, Listener, Transport};
use anyhow::Result;
use async_trait::async_trait;
use omnish_protocol::message::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener as TokioUnixListener, UnixStream};
use tokio::sync::Mutex;

pub struct UnixTransport;

#[async_trait]
impl Transport for UnixTransport {
    async fn connect(&self, addr: &str) -> Result<Box<dyn Connection>> {
        let stream = UnixStream::connect(addr).await?;
        Ok(Box::new(UnixConnection {
            stream: Mutex::new(stream),
        }))
    }

    async fn listen(&self, addr: &str) -> Result<Box<dyn Listener>> {
        // Remove stale socket file if exists
        let _ = std::fs::remove_file(addr);
        let listener = TokioUnixListener::bind(addr)?;
        Ok(Box::new(UnixListener { listener }))
    }
}

struct UnixConnection {
    stream: Mutex<UnixStream>,
}

#[async_trait]
impl Connection for UnixConnection {
    async fn send(&self, msg: &Message) -> Result<()> {
        let bytes = msg.to_bytes()?;
        let mut stream = self.stream.lock().await;
        stream.write_u32(bytes.len() as u32).await?;
        stream.write_all(&bytes).await?;
        stream.flush().await?;
        Ok(())
    }

    async fn recv(&self) -> Result<Message> {
        let mut stream = self.stream.lock().await;
        let len = stream.read_u32().await? as usize;
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await?;
        Message::from_bytes(&buf)
    }
}

struct UnixListener {
    listener: TokioUnixListener,
}

#[async_trait]
impl Listener for UnixListener {
    async fn accept(&mut self) -> Result<Box<dyn Connection>> {
        let (stream, _) = self.listener.accept().await?;
        Ok(Box::new(UnixConnection {
            stream: Mutex::new(stream),
        }))
    }
}
```

```rust
// crates/omnish-transport/src/lib.rs
pub mod traits;
pub mod unix;
```

Update `crates/omnish-transport/Cargo.toml`:
```toml
[dependencies]
omnish-protocol = { path = "../omnish-protocol" }
tokio = { workspace = true }
anyhow = { workspace = true }
async-trait = "0.1"

[dev-dependencies]
tempfile = "3"
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-transport`
Expected: PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(transport): add Transport trait and Unix socket implementation"
```

---

### Task 5: omnish-pty — PTY Operations

**Files:**
- Create: `crates/omnish-pty/src/proxy.rs`
- Create: `crates/omnish-pty/src/raw_mode.rs`
- Modify: `crates/omnish-pty/Cargo.toml`
- Modify: `crates/omnish-pty/src/lib.rs`
- Test: `crates/omnish-pty/tests/pty_test.rs`

**Step 1: Write failing test for PTY spawn and I/O**

```rust
// crates/omnish-pty/tests/pty_test.rs
use omnish_pty::proxy::PtyProxy;
use std::time::Duration;

#[test]
fn test_pty_spawn_and_read_output() {
    // Spawn /bin/echo via PTY, read its output
    let mut proxy = PtyProxy::spawn("/bin/echo", &["hello_from_pty"]).unwrap();
    let mut buf = vec![0u8; 256];
    // Give the process time to produce output
    std::thread::sleep(Duration::from_millis(200));
    let n = proxy.read(&mut buf).unwrap();
    let output = String::from_utf8_lossy(&buf[..n]);
    assert!(output.contains("hello_from_pty"), "got: {}", output);
    proxy.wait().unwrap();
}

#[test]
fn test_pty_spawn_returns_child_pid() {
    let proxy = PtyProxy::spawn("/bin/true", &[]).unwrap();
    assert!(proxy.child_pid() > 0);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-pty`
Expected: FAIL — module not found.

**Step 3: Implement PTY proxy**

```rust
// crates/omnish-pty/src/proxy.rs
use anyhow::{Context, Result};
use nix::pty::{openpty, OpenptyResult};
use nix::sys::termios;
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{close, dup2, execvp, fork, read, setsid, write, ForkResult, Pid};
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub struct PtyProxy {
    master_fd: OwnedFd,
    child_pid: Pid,
}

impl PtyProxy {
    pub fn spawn(cmd: &str, args: &[&str]) -> Result<Self> {
        let OpenptyResult { master, slave } =
            openpty(None, None).context("openpty failed")?;

        // Safety: we immediately exec or _exit in child
        match unsafe { fork() }.context("fork failed")? {
            ForkResult::Child => {
                // Close master in child
                drop(master);

                // Create new session, set controlling terminal
                setsid().ok();
                unsafe {
                    libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY, 0);
                }

                // Redirect stdio to slave
                dup2(slave.as_raw_fd(), 0).ok();
                dup2(slave.as_raw_fd(), 1).ok();
                dup2(slave.as_raw_fd(), 2).ok();
                if slave.as_raw_fd() > 2 {
                    drop(slave);
                }

                // Exec
                let c_cmd = CString::new(cmd).unwrap();
                let mut c_args: Vec<CString> = vec![c_cmd.clone()];
                for a in args {
                    c_args.push(CString::new(*a).unwrap());
                }
                execvp(&c_cmd, &c_args).ok();
                unsafe { libc::_exit(127) };
            }
            ForkResult::Parent { child } => {
                drop(slave);
                Ok(PtyProxy {
                    master_fd: master,
                    child_pid: child,
                })
            }
        }
    }

    pub fn master_raw_fd(&self) -> RawFd {
        self.master_fd.as_raw_fd()
    }

    pub fn child_pid(&self) -> i32 {
        self.child_pid.as_raw() as i32
    }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let n = read(self.master_fd.as_raw_fd(), buf)
            .context("read from PTY master")?;
        Ok(n)
    }

    pub fn write_all(&self, data: &[u8]) -> Result<()> {
        let mut written = 0;
        while written < data.len() {
            let n = write(&self.master_fd, &data[written..])
                .context("write to PTY master")?;
            written += n;
        }
        Ok(())
    }

    pub fn set_window_size(&self, rows: u16, cols: u16) -> Result<()> {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let ret = unsafe {
            libc::ioctl(self.master_fd.as_raw_fd(), libc::TIOCSWINSZ, &ws)
        };
        if ret < 0 {
            anyhow::bail!("ioctl TIOCSWINSZ failed");
        }
        Ok(())
    }

    pub fn wait(&self) -> Result<i32> {
        match waitpid(self.child_pid, None)? {
            WaitStatus::Exited(_, code) => Ok(code),
            WaitStatus::Signaled(_, sig, _) => Ok(128 + sig as i32),
            _ => Ok(-1),
        }
    }
}
```

```rust
// crates/omnish-pty/src/raw_mode.rs
use anyhow::Result;
use nix::sys::termios::{self, SetArg, Termios};
use std::os::fd::RawFd;

pub struct RawModeGuard {
    fd: RawFd,
    original: Termios,
}

impl RawModeGuard {
    pub fn enter(fd: RawFd) -> Result<Self> {
        let original = termios::tcgetattr(fd)?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(fd, SetArg::TCSANOW, &raw)?;
        Ok(Self { fd, original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(self.fd, SetArg::TCSANOW, &self.original);
    }
}
```

```rust
// crates/omnish-pty/src/lib.rs
pub mod proxy;
pub mod raw_mode;
```

Update `crates/omnish-pty/Cargo.toml`:
```toml
[dependencies]
nix = { workspace = true }
anyhow = { workspace = true }
libc = "0.2"
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-pty`
Expected: PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(pty): add PTY proxy with forkpty, raw mode, and window resize"
```

---

### Task 6: omnish-store — Session Stream Storage

**Files:**
- Create: `crates/omnish-store/src/session.rs`
- Create: `crates/omnish-store/src/stream.rs`
- Modify: `crates/omnish-store/Cargo.toml`
- Modify: `crates/omnish-store/src/lib.rs`
- Test: `crates/omnish-store/tests/store_test.rs`

**Step 1: Write failing test for stream storage**

```rust
// crates/omnish-store/tests/store_test.rs
use omnish_store::session::SessionMeta;
use omnish_store::stream::StreamWriter;
use tempfile::tempdir;

#[test]
fn test_write_and_read_session_meta() {
    let dir = tempdir().unwrap();
    let meta = SessionMeta {
        session_id: "abc123".to_string(),
        shell: "/bin/bash".to_string(),
        pid: 1234,
        tty: "/dev/pts/0".to_string(),
        started_at: "2026-02-11T16:30:00Z".to_string(),
        ended_at: None,
    };
    meta.save(dir.path()).unwrap();
    let loaded = SessionMeta::load(dir.path()).unwrap();
    assert_eq!(loaded.session_id, "abc123");
    assert_eq!(loaded.pid, 1234);
}

#[test]
fn test_stream_writer_and_reader() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("stream.bin");

    {
        let mut writer = StreamWriter::create(&path).unwrap();
        writer.write_entry(1000, 0, b"ls -la\n").unwrap();   // 0 = input
        writer.write_entry(1001, 1, b"total 0\n").unwrap();  // 1 = output
    }

    let entries = omnish_store::stream::read_entries(&path).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].timestamp_ms, 1000);
    assert_eq!(entries[0].direction, 0);
    assert_eq!(entries[0].data, b"ls -la\n");
    assert_eq!(entries[1].direction, 1);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-store`
Expected: FAIL — module not found.

**Step 3: Implement session metadata and stream storage**

```rust
// crates/omnish-store/src/session.rs
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    pub shell: String,
    pub pid: u32,
    pub tty: String,
    pub started_at: String,
    pub ended_at: Option<String>,
}

impl SessionMeta {
    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join("meta.json");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("meta.json");
        let json = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&json)?)
    }
}
```

```rust
// crates/omnish-store/src/stream.rs
use anyhow::Result;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

/// Binary format per entry: timestamp_ms(8) + direction(1) + data_len(4) + data(N)
pub struct StreamWriter {
    writer: BufWriter<File>,
}

pub struct StreamEntry {
    pub timestamp_ms: u64,
    pub direction: u8,
    pub data: Vec<u8>,
}

impl StreamWriter {
    pub fn create(path: &Path) -> Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    pub fn write_entry(&mut self, timestamp_ms: u64, direction: u8, data: &[u8]) -> Result<()> {
        self.writer.write_all(&timestamp_ms.to_be_bytes())?;
        self.writer.write_all(&[direction])?;
        self.writer
            .write_all(&(data.len() as u32).to_be_bytes())?;
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }
}

pub fn read_entries(path: &Path) -> Result<Vec<StreamEntry>> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    let mut entries = Vec::new();
    let mut pos = 0;
    while pos + 13 <= data.len() {
        let timestamp_ms = u64::from_be_bytes(data[pos..pos + 8].try_into()?);
        let direction = data[pos + 8];
        let data_len =
            u32::from_be_bytes(data[pos + 9..pos + 13].try_into()?) as usize;
        if pos + 13 + data_len > data.len() {
            break;
        }
        let entry_data = data[pos + 13..pos + 13 + data_len].to_vec();
        entries.push(StreamEntry {
            timestamp_ms,
            direction,
            data: entry_data,
        });
        pos += 13 + data_len;
    }
    Ok(entries)
}
```

```rust
// crates/omnish-store/src/lib.rs
pub mod session;
pub mod stream;
```

Update `crates/omnish-store/Cargo.toml`:
```toml
[dependencies]
serde = { workspace = true }
serde_json = "1"
anyhow = { workspace = true }

[dev-dependencies]
tempfile = "3"
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-store`
Expected: PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(store): add session metadata and binary stream storage"
```

---

### Task 7: omnish-llm — LLM Backend Trait and Remote Implementations

**Files:**
- Create: `crates/omnish-llm/src/backend.rs`
- Create: `crates/omnish-llm/src/anthropic.rs`
- Create: `crates/omnish-llm/src/openai_compat.rs`
- Create: `crates/omnish-llm/src/context.rs`
- Modify: `crates/omnish-llm/Cargo.toml`
- Modify: `crates/omnish-llm/src/lib.rs`
- Test: `crates/omnish-llm/tests/llm_test.rs`

**Step 1: Write failing test for LLM request construction and context building**

```rust
// crates/omnish-llm/tests/llm_test.rs
use omnish_llm::backend::{LlmRequest, TriggerType};
use omnish_llm::context::ContextBuilder;

#[test]
fn test_context_builder_strips_escape_sequences() {
    let raw = b"\x1b[31mERROR\x1b[0m: file not found\n";
    let builder = ContextBuilder::new();
    let cleaned = builder.strip_escapes(raw);
    assert_eq!(cleaned, "ERROR: file not found\n");
}

#[test]
fn test_context_builder_truncates_to_max_tokens() {
    let builder = ContextBuilder::new().max_chars(20);
    let long_text = "a".repeat(100);
    let truncated = builder.truncate(&long_text);
    assert_eq!(truncated.len(), 20);
}

#[test]
fn test_llm_request_build() {
    let req = LlmRequest {
        context: "$ ls\nfile.txt\n$ cat file.txt\nhello".to_string(),
        query: Some("what is in file.txt?".to_string()),
        trigger: TriggerType::Manual,
        session_ids: vec!["abc".to_string()],
    };
    assert_eq!(req.session_ids.len(), 1);
    assert!(req.query.is_some());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-llm`
Expected: FAIL

**Step 3: Implement LLM backend trait, context builder, and backend stubs**

```rust
// crates/omnish-llm/src/backend.rs
use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub context: String,
    pub query: Option<String>,
    pub trigger: TriggerType,
    pub session_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum TriggerType {
    Manual,
    AutoError,
    AutoPattern,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
    pub model: String,
}

#[async_trait]
pub trait LlmBackend: Send + Sync {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse>;
    fn name(&self) -> &str;
}
```

```rust
// crates/omnish-llm/src/context.rs
pub struct ContextBuilder {
    max_chars: usize,
}

impl ContextBuilder {
    pub fn new() -> Self {
        Self { max_chars: 8000 }
    }

    pub fn max_chars(mut self, n: usize) -> Self {
        self.max_chars = n;
        self
    }

    pub fn strip_escapes(&self, raw: &[u8]) -> String {
        let s = String::from_utf8_lossy(raw);
        // Strip ANSI escape sequences: ESC[ ... m and ESC[ ... other
        let mut result = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip ESC[...X sequences
                if chars.peek() == Some(&'[') {
                    chars.next(); // consume '['
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            } else {
                result.push(c);
            }
        }
        result
    }

    pub fn truncate<'a>(&self, text: &'a str) -> &'a str {
        if text.len() <= self.max_chars {
            text
        } else {
            &text[text.len() - self.max_chars..]
        }
    }
}
```

```rust
// crates/omnish-llm/src/anthropic.rs
use crate::backend::{LlmBackend, LlmRequest, LlmResponse};
use anyhow::Result;
use async_trait::async_trait;

pub struct AnthropicBackend {
    pub model: String,
    pub api_key: String,
}

#[async_trait]
impl LlmBackend for AnthropicBackend {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let client = reqwest::Client::new();

        let mut messages = Vec::new();
        let user_content = if let Some(query) = &req.query {
            format!(
                "Here is the terminal session context:\n\n```\n{}\n```\n\nUser question: {}",
                req.context, query
            )
        } else {
            format!(
                "Analyze this terminal session output and explain any errors or issues:\n\n```\n{}\n```",
                req.context
            )
        };

        messages.push(serde_json::json!({
            "role": "user",
            "content": user_content
        }));

        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 1024,
            "messages": messages
        });

        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let json: serde_json::Value = resp.json().await?;
        let content = json["content"][0]["text"]
            .as_str()
            .unwrap_or("(no response)")
            .to_string();

        Ok(LlmResponse {
            content,
            model: self.model.clone(),
        })
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}
```

```rust
// crates/omnish-llm/src/openai_compat.rs
use crate::backend::{LlmBackend, LlmRequest, LlmResponse};
use anyhow::Result;
use async_trait::async_trait;

pub struct OpenAiCompatBackend {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
}

#[async_trait]
impl LlmBackend for OpenAiCompatBackend {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let client = reqwest::Client::new();

        let user_content = if let Some(query) = &req.query {
            format!(
                "Here is the terminal session context:\n\n```\n{}\n```\n\nUser question: {}",
                req.context, query
            )
        } else {
            format!(
                "Analyze this terminal session output and explain any errors or issues:\n\n```\n{}\n```",
                req.context
            )
        };

        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                {
                    "role": "user",
                    "content": user_content
                }
            ]
        });

        let resp = client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let json: serde_json::Value = resp.json().await?;
        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("(no response)")
            .to_string();

        Ok(LlmResponse {
            content,
            model: self.model.clone(),
        })
    }

    fn name(&self) -> &str {
        "openai_compat"
    }
}
```

```rust
// crates/omnish-llm/src/lib.rs
pub mod anthropic;
pub mod backend;
pub mod context;
pub mod openai_compat;
```

Update `crates/omnish-llm/Cargo.toml`:
```toml
[dependencies]
reqwest = { workspace = true }
serde = { workspace = true }
serde_json = "1"
anyhow = { workspace = true }
async-trait = "0.1"
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-llm`
Expected: PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(llm): add LlmBackend trait with Anthropic and OpenAI-compat implementations"
```

---

### Task 8: omnish-daemon — Daemon Process

**Files:**
- Create: `crates/omnish-daemon/src/server.rs`
- Create: `crates/omnish-daemon/src/session_mgr.rs`
- Create: `crates/omnish-daemon/src/event_detector.rs`
- Modify: `crates/omnish-daemon/Cargo.toml`
- Modify: `crates/omnish-daemon/src/main.rs`
- Test: `crates/omnish-daemon/tests/daemon_test.rs`

**Step 1: Write failing test for session manager**

```rust
// crates/omnish-daemon/tests/daemon_test.rs
use omnish_daemon::session_mgr::SessionManager;

#[tokio::test]
async fn test_session_register_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("sess1", "/bin/bash", 100, "/dev/pts/0").await.unwrap();
    mgr.register("sess2", "/bin/zsh", 101, "/dev/pts/1").await.unwrap();

    let sessions = mgr.list_active().await;
    assert_eq!(sessions.len(), 2);
}

#[tokio::test]
async fn test_session_end() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    mgr.register("sess1", "/bin/bash", 100, "/dev/pts/0").await.unwrap();
    mgr.end_session("sess1").await.unwrap();

    let sessions = mgr.list_active().await;
    assert_eq!(sessions.len(), 0);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon`
Expected: FAIL

**Step 3: Implement session manager**

```rust
// crates/omnish-daemon/src/session_mgr.rs
use anyhow::Result;
use omnish_store::session::SessionMeta;
use omnish_store::stream::StreamWriter;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::Mutex;

struct ActiveSession {
    meta: SessionMeta,
    stream_writer: StreamWriter,
    dir: PathBuf,
}

pub struct SessionManager {
    base_dir: PathBuf,
    sessions: Mutex<HashMap<String, ActiveSession>>,
}

impl SessionManager {
    pub fn new(base_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&base_dir).ok();
        Self {
            base_dir,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub async fn register(
        &self,
        session_id: &str,
        shell: &str,
        pid: u32,
        tty: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let session_dir = self.base_dir.join(format!(
            "{}_{}",
            now.replace(':', "-"),
            session_id
        ));
        std::fs::create_dir_all(&session_dir)?;

        let meta = SessionMeta {
            session_id: session_id.to_string(),
            shell: shell.to_string(),
            pid,
            tty: tty.to_string(),
            started_at: now,
            ended_at: None,
        };
        meta.save(&session_dir)?;

        let stream_writer = StreamWriter::create(&session_dir.join("stream.bin"))?;

        let mut sessions = self.sessions.lock().await;
        sessions.insert(
            session_id.to_string(),
            ActiveSession {
                meta,
                stream_writer,
                dir: session_dir,
            },
        );
        Ok(())
    }

    pub async fn write_io(
        &self,
        session_id: &str,
        timestamp_ms: u64,
        direction: u8,
        data: &[u8],
    ) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.stream_writer.write_entry(timestamp_ms, direction, data)?;
        }
        Ok(())
    }

    pub async fn end_session(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        if let Some(mut session) = sessions.remove(session_id) {
            session.meta.ended_at = Some(chrono::Utc::now().to_rfc3339());
            session.meta.save(&session.dir)?;
        }
        Ok(())
    }

    pub async fn list_active(&self) -> Vec<String> {
        let sessions = self.sessions.lock().await;
        sessions.keys().cloned().collect()
    }
}
```

```rust
// crates/omnish-daemon/src/event_detector.rs
use omnish_common::config::AutoTriggerConfig;

pub struct EventDetector {
    config: AutoTriggerConfig,
}

impl EventDetector {
    pub fn new(config: AutoTriggerConfig) -> Self {
        Self { config }
    }

    pub fn check_output(&self, data: &[u8]) -> Vec<DetectedEvent> {
        let text = String::from_utf8_lossy(data).to_lowercase();
        let mut events = Vec::new();

        for pattern in &self.config.on_stderr_patterns {
            if text.contains(&pattern.to_lowercase()) {
                events.push(DetectedEvent::PatternMatch(pattern.clone()));
            }
        }

        events
    }
}

#[derive(Debug, Clone)]
pub enum DetectedEvent {
    PatternMatch(String),
    NonZeroExit(i32),
}
```

```rust
// crates/omnish-daemon/src/server.rs
use crate::session_mgr::SessionManager;
use anyhow::Result;
use omnish_protocol::message::*;
use omnish_transport::traits::{Connection, Listener, Transport};
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
            Err(_) => break, // client disconnected
        };

        match msg {
            Message::SessionStart(s) => {
                mgr.register(&s.session_id, &s.shell, s.pid, &s.tty)
                    .await?;
            }
            Message::SessionEnd(s) => {
                mgr.end_session(&s.session_id).await?;
            }
            Message::IoData(io) => {
                let dir = match io.direction {
                    IoDirection::Input => 0,
                    IoDirection::Output => 1,
                };
                mgr.write_io(&io.session_id, io.timestamp_ms, dir, &io.data)
                    .await?;
            }
            Message::Request(req) => {
                // TODO: dispatch to LLM engine
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
```

```rust
// crates/omnish-daemon/src/main.rs
mod event_detector;
mod server;
pub mod session_mgr;

use anyhow::Result;
use omnish_common::config::OmnishConfig;
use omnish_transport::unix::UnixTransport;
use server::DaemonServer;
use session_mgr::SessionManager;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // TODO: load config from file
    let socket_path = std::env::var("OMNISH_SOCKET")
        .unwrap_or_else(|_| {
            if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
                format!("{}/omnish.sock", dir)
            } else {
                "/tmp/omnish.sock".to_string()
            }
        });

    let store_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("omnish/sessions");

    let session_mgr = Arc::new(SessionManager::new(store_dir));
    let server = DaemonServer::new(session_mgr);
    let transport = UnixTransport;

    tracing::info!("starting omnishd at {}", socket_path);
    server.run(&transport, &socket_path).await
}
```

Update `crates/omnish-daemon/Cargo.toml`:
```toml
[dependencies]
omnish-common = { path = "../omnish-common" }
omnish-protocol = { path = "../omnish-protocol" }
omnish-transport = { path = "../omnish-transport" }
omnish-store = { path = "../omnish-store" }
omnish-llm = { path = "../omnish-llm" }
tokio = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
chrono = { workspace = true }
dirs = "5"
serde_json = "1"

[dev-dependencies]
tempfile = "3"
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p omnish-daemon`
Expected: PASS

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(daemon): add omnishd with session manager, event detector, and server loop"
```

---

### Task 9: omnish-client — Client Binary with PTY Proxy and Daemon Connection

**Files:**
- Create: `crates/omnish-client/src/commands.rs`
- Modify: `crates/omnish-client/Cargo.toml`
- Modify: `crates/omnish-client/src/main.rs`

**Step 1: Implement command parser for :: prefix**

```rust
// crates/omnish-client/src/commands.rs

#[derive(Debug)]
pub enum OmnishCommand {
    Ask { flags: AskFlags, query: String },
    Sessions,
    Status,
    Pause,
    Resume,
    Config { key: String, value: String },
    Replay { session_id: String },
    Unknown(String),
}

#[derive(Debug, Default)]
pub struct AskFlags {
    pub all_sessions: bool,
    pub session_count: Option<usize>,
}

pub fn parse_command(input: &str, prefix: &str) -> Option<OmnishCommand> {
    let trimmed = input.trim();
    if !trimmed.starts_with(prefix) {
        return None;
    }
    let rest = trimmed[prefix.len()..].trim();
    let parts: Vec<&str> = rest.splitn(2, ' ').collect();
    let cmd = parts[0];
    let args = parts.get(1).copied().unwrap_or("");

    Some(match cmd {
        "ask" => {
            let mut flags = AskFlags::default();
            let mut query_parts = Vec::new();
            let mut tokens = args.split_whitespace().peekable();
            while let Some(tok) = tokens.next() {
                match tok {
                    "-a" => flags.all_sessions = true,
                    "-s" => {
                        if let Some(n) = tokens.next() {
                            flags.session_count = n.parse().ok();
                        }
                    }
                    _ => {
                        query_parts.push(tok);
                        query_parts.extend(tokens);
                        break;
                    }
                }
            }
            OmnishCommand::Ask {
                flags,
                query: query_parts.join(" "),
            }
        }
        "sessions" => OmnishCommand::Sessions,
        "status" => OmnishCommand::Status,
        "pause" => OmnishCommand::Pause,
        "resume" => OmnishCommand::Resume,
        "config" => {
            let config_parts: Vec<&str> = args.splitn(2, ' ').collect();
            OmnishCommand::Config {
                key: config_parts.get(0).unwrap_or(&"").to_string(),
                value: config_parts.get(1).unwrap_or(&"").to_string(),
            }
        }
        "replay" => OmnishCommand::Replay {
            session_id: args.to_string(),
        },
        _ => OmnishCommand::Unknown(rest.to_string()),
    })
}
```

**Step 2: Implement client main with PTY proxy loop**

```rust
// crates/omnish-client/src/main.rs
mod commands;

use anyhow::Result;
use commands::parse_command;
use omnish_protocol::message::*;
use omnish_pty::proxy::PtyProxy;
use omnish_pty::raw_mode::RawModeGuard;
use omnish_transport::traits::Connection;
use omnish_transport::unix::UnixTransport;
use omnish_transport::traits::Transport;
use std::os::fd::AsRawFd;
use uuid::Uuid;

fn timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn get_socket_path() -> String {
    std::env::var("OMNISH_SOCKET").unwrap_or_else(|_| {
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            format!("{}/omnish.sock", dir)
        } else {
            "/tmp/omnish.sock".to_string()
        }
    })
}

fn get_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    let session_id = Uuid::new_v4().to_string()[..8].to_string();
    let shell = get_shell();
    let prefix = "::";

    // Spawn PTY with shell
    let proxy = PtyProxy::spawn(&shell, &[])?;

    // Connect to daemon (graceful degradation if unavailable)
    let daemon_conn = connect_daemon(&session_id, &shell, proxy.child_pid() as u32).await;

    // Enter raw mode on stdin
    let _raw_guard = RawModeGuard::enter(std::io::stdin().as_raw_fd())?;

    // Sync initial window size
    if let Some((rows, cols)) = get_terminal_size() {
        proxy.set_window_size(rows, cols).ok();
    }

    // Install SIGWINCH handler
    let master_fd = proxy.master_raw_fd();
    setup_sigwinch(master_fd);

    // Main I/O loop using poll
    let mut input_buf = [0u8; 4096];
    let mut output_buf = [0u8; 4096];
    let mut line_buf = Vec::new();

    loop {
        let mut fds = [
            libc::pollfd {
                fd: 0, // stdin
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: master_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];

        let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, 100) };
        if ret < 0 {
            continue;
        }

        // Stdin -> PTY master
        if fds[0].revents & libc::POLLIN != 0 {
            let n = nix::unistd::read(0, &mut input_buf)?;
            if n == 0 {
                break;
            }

            // TODO: line buffer for :: command detection
            // For now, pass through directly
            proxy.write_all(&input_buf[..n])?;

            // Report to daemon
            if let Some(ref conn) = daemon_conn {
                let msg = Message::IoData(IoData {
                    session_id: session_id.clone(),
                    direction: IoDirection::Input,
                    timestamp_ms: timestamp_ms(),
                    data: input_buf[..n].to_vec(),
                });
                let _ = conn.send(&msg).await;
            }
        }

        // PTY master -> stdout
        if fds[1].revents & libc::POLLIN != 0 {
            match proxy.read(&mut output_buf) {
                Ok(0) => break,
                Ok(n) => {
                    nix::unistd::write(std::io::stdout().as_raw_fd(), &output_buf[..n])?;

                    // Report to daemon
                    if let Some(ref conn) = daemon_conn {
                        let msg = Message::IoData(IoData {
                            session_id: session_id.clone(),
                            direction: IoDirection::Output,
                            timestamp_ms: timestamp_ms(),
                            data: output_buf[..n].to_vec(),
                        });
                        let _ = conn.send(&msg).await;
                    }
                }
                Err(_) => break,
            }
        }

        // Check if PTY hung up
        if fds[1].revents & libc::POLLHUP != 0 {
            break;
        }
    }

    // Send session end
    if let Some(ref conn) = daemon_conn {
        let msg = Message::SessionEnd(SessionEnd {
            session_id: session_id.clone(),
            timestamp_ms: timestamp_ms(),
            exit_code: None,
        });
        let _ = conn.send(&msg).await;
    }

    let exit_code = proxy.wait().unwrap_or(1);
    std::process::exit(exit_code);
}

async fn connect_daemon(
    session_id: &str,
    shell: &str,
    pid: u32,
) -> Option<Box<dyn Connection>> {
    let socket_path = get_socket_path();
    let transport = UnixTransport;
    match transport.connect(&socket_path).await {
        Ok(conn) => {
            let tty = std::env::var("TTY").unwrap_or_default();
            let msg = Message::SessionStart(SessionStart {
                session_id: session_id.to_string(),
                shell: shell.to_string(),
                pid,
                tty,
                timestamp_ms: timestamp_ms(),
            });
            if conn.send(&msg).await.is_ok() {
                Some(conn)
            } else {
                None
            }
        }
        Err(_) => {
            eprintln!("[omnish] daemon not available, running in passthrough mode");
            None
        }
    }
}

fn get_terminal_size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 {
        Some((ws.ws_row, ws.ws_col))
    } else {
        None
    }
}

fn setup_sigwinch(master_fd: i32) {
    // Store master_fd for signal handler
    unsafe {
        MASTER_FD = master_fd;
    }
    unsafe {
        libc::signal(libc::SIGWINCH, sigwinch_handler as libc::sighandler_t);
    }
}

static mut MASTER_FD: i32 = -1;

extern "C" fn sigwinch_handler(_sig: libc::c_int) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) == 0 {
            libc::ioctl(MASTER_FD, libc::TIOCSWINSZ, &ws);
        }
    }
}
```

Update `crates/omnish-client/Cargo.toml`:
```toml
[dependencies]
omnish-common = { path = "../omnish-common" }
omnish-protocol = { path = "../omnish-protocol" }
omnish-transport = { path = "../omnish-transport" }
omnish-pty = { path = "../omnish-pty" }
tokio = { workspace = true }
anyhow = { workspace = true }
uuid = { workspace = true }
nix = { workspace = true }
libc = "0.2"
```

**Step 3: Write test for command parser**

```rust
// crates/omnish-client/tests/commands_test.rs
// Note: commands module is private to binary, test inline or make pub(crate)
// For now we test via integration - will verify with cargo build
```

**Step 4: Verify it compiles**

Run: `cargo build`
Expected: Compiles successfully.

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(client): add omnish client with PTY proxy loop and daemon connection"
```

---

### Task 10: Integration — End-to-End Smoke Test

**Files:**
- Create: `tests/integration_test.rs`
- Modify: root `Cargo.toml` (add integration test path if needed)

**Step 1: Write integration smoke test**

```rust
// tests/integration_test.rs
use std::process::Command;
use std::time::Duration;

#[test]
fn test_omnishd_starts_and_stops() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_omnish-daemon"))
        .env("OMNISH_SOCKET", "/tmp/omnish-test.sock")
        .spawn()
        .expect("failed to start omnishd");

    std::thread::sleep(Duration::from_millis(500));

    // Verify socket exists
    assert!(
        std::path::Path::new("/tmp/omnish-test.sock").exists(),
        "socket file should exist"
    );

    child.kill().ok();
    child.wait().ok();
    std::fs::remove_file("/tmp/omnish-test.sock").ok();
}
```

**Step 2: Run integration test**

Run: `cargo test --test integration_test`
Expected: PASS

**Step 3: Verify full workspace builds and all tests pass**

Run: `cargo build && cargo test`
Expected: All builds pass, all tests pass.

**Step 4: Commit**

```bash
git add -A
git commit -m "test: add integration smoke test for daemon startup"
```

---

## Summary

| Task | Crate | Description |
|------|-------|-------------|
| 1 | workspace | Scaffold Cargo workspace with all crates |
| 2 | omnish-common | Config types, default config |
| 3 | omnish-protocol | Message types, binary serialization |
| 4 | omnish-transport | Transport trait, Unix socket impl |
| 5 | omnish-pty | PTY proxy (forkpty, raw mode, SIGWINCH) |
| 6 | omnish-store | Session metadata, binary stream storage |
| 7 | omnish-llm | LlmBackend trait, Anthropic + OpenAI backends |
| 8 | omnish-daemon | Daemon with session mgr, event detector, server |
| 9 | omnish-client | Client with PTY loop, daemon connection, :: commands |
| 10 | integration | End-to-end smoke test |
