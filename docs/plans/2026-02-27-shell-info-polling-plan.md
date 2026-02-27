# Shell Info Periodic Polling Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Periodically collect the shell's CWD and child process info from `/proc`, and send updates to the daemon when values change.

**Architecture:** Extend the existing Probe system with two new probes (`ShellCwdProbe`, `ChildProcessProbe`) that read from `/proc/{pid}`. A tokio task in the client polls every 5 seconds, diffs against previous state, and sends a new `SessionUpdate` message to the daemon. The daemon merges changed attrs into `SessionMeta`.

**Tech Stack:** Rust, `procfs` crate, tokio, existing omnish probe/protocol/session infrastructure.

---

### Task 1: Add `procfs` dependency to omnish-client

**Files:**
- Modify: `crates/omnish-client/Cargo.toml`

**Step 1: Add dependency**

Add `procfs = "0.17"` to `[dependencies]` in `crates/omnish-client/Cargo.toml`:

```toml
procfs = "0.17"
```

**Step 2: Verify it compiles**

Run: `cargo check -p omnish-client`
Expected: success

**Step 3: Commit**

```bash
git add crates/omnish-client/Cargo.toml Cargo.lock
git commit -m "chore: add procfs dependency to omnish-client"
```

---

### Task 2: Add `ShellCwdProbe` and `ChildProcessProbe`

**Files:**
- Modify: `crates/omnish-client/src/probe.rs`

**Step 1: Write tests for the new probes**

Add to the existing `#[cfg(test)] mod tests` block in `probe.rs`:

```rust
#[test]
fn test_shell_cwd_probe_returns_path_for_self() {
    // Use our own PID — /proc/self/cwd should be readable
    let pid = std::process::id();
    let probe = ShellCwdProbe(pid);
    assert_eq!(probe.key(), "shell_cwd");
    let cwd = probe.collect();
    assert!(cwd.is_some(), "should read own cwd from /proc");
    // Should match std::env::current_dir()
    let expected = std::env::current_dir().unwrap().to_string_lossy().to_string();
    assert_eq!(cwd.unwrap(), expected);
}

#[test]
fn test_shell_cwd_probe_returns_none_for_bad_pid() {
    let probe = ShellCwdProbe(999999999);
    assert_eq!(probe.collect(), None);
}

#[test]
fn test_child_process_probe_key() {
    let probe = ChildProcessProbe(std::process::id());
    assert_eq!(probe.key(), "child_process");
}

#[test]
fn test_child_process_probe_returns_string_or_empty() {
    // Our test process likely has no children, so expect empty or some value
    let probe = ChildProcessProbe(std::process::id());
    let result = probe.collect();
    // Should always return Some (possibly empty string)
    assert!(result.is_some());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p omnish-client -- probe`
Expected: FAIL — `ShellCwdProbe` and `ChildProcessProbe` not defined

**Step 3: Implement `ShellCwdProbe`**

Add to `probe.rs` (before `default_session_probes`):

```rust
pub struct ShellCwdProbe(pub u32);
impl Probe for ShellCwdProbe {
    fn key(&self) -> &str { "shell_cwd" }
    fn collect(&self) -> Option<String> {
        std::fs::read_link(format!("/proc/{}/cwd", self.0))
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    }
}
```

**Step 4: Implement `ChildProcessProbe`**

Add to `probe.rs`:

```rust
pub struct ChildProcessProbe(pub u32);
impl Probe for ChildProcessProbe {
    fn key(&self) -> &str { "child_process" }
    fn collect(&self) -> Option<String> {
        let proc = procfs::process::Process::new(self.0 as i32).ok()?;
        // Find direct children by scanning /proc/{pid}/task/{pid}/children
        let children_path = format!("/proc/{}/task/{}/children", self.0, self.0);
        let children_str = std::fs::read_to_string(&children_path).unwrap_or_default();
        let child_pid: Option<i32> = children_str
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .last(); // last child is typically the foreground process
        match child_pid {
            Some(pid) => {
                let name = procfs::process::Process::new(pid)
                    .ok()
                    .and_then(|p| p.comm().ok())
                    .unwrap_or_default();
                Some(format!("{}:{}", name, pid))
            }
            None => Some(String::new()),
        }
    }
}
```

**Step 5: Add `default_polling_probes` helper**

Add after `default_session_probes`:

```rust
pub fn default_polling_probes(child_pid: u32) -> ProbeSet {
    let mut set = ProbeSet::new();
    set.add(Box::new(ShellCwdProbe(child_pid)));
    set.add(Box::new(ChildProcessProbe(child_pid)));
    set
}
```

**Step 6: Run tests**

Run: `cargo test -p omnish-client -- probe`
Expected: PASS

**Step 7: Commit**

```bash
git add crates/omnish-client/src/probe.rs
git commit -m "feat: add ShellCwdProbe and ChildProcessProbe for periodic polling"
```

---

### Task 3: Add `SessionUpdate` message to protocol

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs`

**Step 1: Write test for SessionUpdate round-trip**

Add to the existing `#[cfg(test)] mod tests` block in `message.rs`:

```rust
#[test]
fn test_frame_with_session_update() {
    let mut attrs = HashMap::new();
    attrs.insert("shell_cwd".to_string(), "/home/user/project".to_string());
    let frame = Frame {
        request_id: 20,
        payload: Message::SessionUpdate(SessionUpdate {
            session_id: "abc".to_string(),
            timestamp_ms: 2000,
            attrs,
        }),
    };
    let bytes = frame.to_bytes().unwrap();
    let decoded = Frame::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.request_id, 20);
    if let Message::SessionUpdate(su) = decoded.payload {
        assert_eq!(su.session_id, "abc");
        assert_eq!(su.attrs.get("shell_cwd").unwrap(), "/home/user/project");
    } else {
        panic!("expected SessionUpdate");
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-protocol -- test_frame_with_session_update`
Expected: FAIL — `SessionUpdate` not defined

**Step 3: Add SessionUpdate struct and message variant**

In `message.rs`, add the struct (after `SessionEnd`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUpdate {
    pub session_id: String,
    pub timestamp_ms: u64,
    pub attrs: HashMap<String, String>,
}
```

Add the variant to the `Message` enum (after `SessionEnd`):

```rust
SessionUpdate(SessionUpdate),
```

**Step 4: Run test**

Run: `cargo test -p omnish-protocol -- test_frame_with_session_update`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/omnish-protocol/src/message.rs
git commit -m "feat: add SessionUpdate message type for periodic attr updates"
```

---

### Task 4: Add `update_attrs` to SessionManager

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs`

**Step 1: Implement `update_attrs` method**

Add to the `impl SessionManager` block:

```rust
pub async fn update_attrs(
    &self,
    session_id: &str,
    attrs: HashMap<String, String>,
) -> Result<()> {
    let sessions = self.sessions.read().await;
    let session = sessions
        .get(session_id)
        .ok_or_else(|| anyhow!("session {} not found", session_id))?;
    let mut meta = session.meta.write().await;
    for (k, v) in attrs {
        meta.attrs.insert(k, v);
    }
    meta.save(&session.dir)?;
    Ok(())
}
```

**Step 2: Verify it compiles**

Run: `cargo check -p omnish-daemon`
Expected: success

**Step 3: Commit**

```bash
git add crates/omnish-daemon/src/session_mgr.rs
git commit -m "feat: add update_attrs to SessionManager for merging attr updates"
```

---

### Task 5: Handle `SessionUpdate` in daemon server

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

**Step 1: Add match arm for SessionUpdate**

In the `handle_message` function's `match msg` block (after the `Message::SessionEnd` arm), add:

```rust
Message::SessionUpdate(su) => {
    if let Err(e) = mgr.update_attrs(&su.session_id, su.attrs).await {
        tracing::error!("update_attrs error: {}", e);
    }
    Message::Ack
}
```

**Step 2: Verify it compiles**

Run: `cargo check -p omnish-daemon`
Expected: success

**Step 3: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "feat: handle SessionUpdate messages in daemon server"
```

---

### Task 6: Add polling loop to client main

**Files:**
- Modify: `crates/omnish-client/src/main.rs`

**Step 1: Add the polling task spawn**

After line 118 (`let daemon_conn = connect_daemon(...)`) and before `let _raw_guard = RawModeGuard::enter(...)`, spawn the polling task:

```rust
// Spawn shell info polling task (5s interval, diff-based updates)
if let Some(ref rpc) = daemon_conn {
    let rpc_poll = rpc.clone();
    let sid_poll = session_id.clone();
    let child_pid_poll = proxy.child_pid() as u32;
    tokio::spawn(async move {
        let probes = probe::default_polling_probes(child_pid_poll);
        let mut last_attrs: HashMap<String, String> = HashMap::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let current = probes.collect_all();
            // Diff: find changed keys
            let changed: HashMap<String, String> = current.iter()
                .filter(|(k, v)| last_attrs.get(*k) != Some(v))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !changed.is_empty() {
                let msg = Message::SessionUpdate(SessionUpdate {
                    session_id: sid_poll.clone(),
                    timestamp_ms: timestamp_ms(),
                    attrs: changed,
                });
                let _ = rpc_poll.call(msg).await;
            }
            last_attrs = current;
        }
    });
}
```

**Step 2: Verify it compiles**

Run: `cargo check -p omnish-client`
Expected: success

**Step 3: Manual integration test**

1. Start daemon: `cargo run -p omnish-daemon`
2. Start client: `cargo run -p omnish-client`
3. In the spawned shell, run `cd /tmp` and wait 5 seconds
4. Check daemon logs for `update_attrs` or inspect session `meta.json` for `shell_cwd: /tmp`

**Step 4: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat: spawn shell info polling task in client (5s interval)"
```

---

### Task 7: Add `SessionUpdate` to `should_buffer`

**Files:**
- Modify: `crates/omnish-client/src/main.rs:29-31`

**Step 1: Include SessionUpdate in bufferable messages**

Update `should_buffer` to include `SessionUpdate` so updates survive reconnections:

```rust
fn should_buffer(msg: &Message) -> bool {
    matches!(msg, Message::IoData(_) | Message::CommandComplete(_) | Message::SessionUpdate(_))
}
```

**Step 2: Verify it compiles**

Run: `cargo check -p omnish-client`
Expected: success

**Step 3: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat: buffer SessionUpdate messages for reconnection replay"
```
