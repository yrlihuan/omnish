# Nested omnish Detection Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Detect nested omnish instances via `OMNISH_SESSION_ID` env var, record parent-child relationships as first-class fields, and deduplicate commands at query time.

**Architecture:** Client sets `OMNISH_SESSION_ID` env var before spawning shell. Inner omnish reads it as `parent_session_id`. Protocol and storage gain a first-class `parent_session_id` field. `omnish-commands` defaults to leaf-session-only display.

**Tech Stack:** Rust, nix (env var in child process), serde (backward compat with `#[serde(default)]`), bincode (protocol serialization)

---

### Task 1: Add `parent_session_id` to protocol `SessionStart`

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs:17-23`
- Test: `crates/omnish-protocol/tests/message_test.rs`

**Step 1: Write the failing test**

Add to `message_test.rs`:

```rust
#[test]
fn test_session_start_with_parent() {
    let msg = Message::SessionStart(SessionStart {
        session_id: "child1".to_string(),
        parent_session_id: Some("parent1".to_string()),
        timestamp_ms: 1707600000000,
        attrs: HashMap::new(),
    });
    let bytes = msg.to_bytes().unwrap();
    let decoded = Message::from_bytes(&bytes).unwrap();
    match decoded {
        Message::SessionStart(s) => {
            assert_eq!(s.session_id, "child1");
            assert_eq!(s.parent_session_id, Some("parent1".to_string()));
        }
        _ => panic!("wrong message type"),
    }
}

#[test]
fn test_session_start_without_parent() {
    let msg = Message::SessionStart(SessionStart {
        session_id: "root1".to_string(),
        parent_session_id: None,
        timestamp_ms: 1707600000000,
        attrs: HashMap::new(),
    });
    let bytes = msg.to_bytes().unwrap();
    let decoded = Message::from_bytes(&bytes).unwrap();
    match decoded {
        Message::SessionStart(s) => {
            assert_eq!(s.session_id, "root1");
            assert_eq!(s.parent_session_id, None);
        }
        _ => panic!("wrong message type"),
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-protocol`
Expected: FAIL — `parent_session_id` field does not exist

**Step 3: Add field to `SessionStart`**

In `message.rs`, add to the `SessionStart` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStart {
    pub session_id: String,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    pub timestamp_ms: u64,
    #[serde(default)]
    pub attrs: HashMap<String, String>,
}
```

**Step 4: Fix existing test**

Update the existing `test_session_start_roundtrip` to include `parent_session_id: None`.

**Step 5: Run tests to verify they pass**

Run: `cargo test -p omnish-protocol`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/omnish-protocol/
git commit -m "feat(protocol): add parent_session_id to SessionStart"
```

---

### Task 2: Add `parent_session_id` to `SessionMeta` storage

**Files:**
- Modify: `crates/omnish-store/src/session.rs:7-13`
- Test: `crates/omnish-store/tests/store_test.rs`

**Step 1: Write the failing test**

Add to `store_test.rs`:

```rust
#[test]
fn test_session_meta_with_parent() {
    let dir = tempfile::tempdir().unwrap();
    let meta = SessionMeta {
        session_id: "child1".into(),
        parent_session_id: Some("parent1".into()),
        started_at: "2026-02-14T10:00:00Z".into(),
        ended_at: None,
        attrs: HashMap::new(),
    };
    meta.save(dir.path()).unwrap();
    let loaded = SessionMeta::load(dir.path()).unwrap();
    assert_eq!(loaded.parent_session_id, Some("parent1".into()));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-store`
Expected: FAIL — `parent_session_id` field does not exist

**Step 3: Add field to `SessionMeta`**

In `session.rs`:

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    #[serde(default)]
    pub attrs: HashMap<String, String>,
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p omnish-store`
Expected: PASS (existing tests still pass via `#[serde(default)]`)

**Step 5: Commit**

```bash
git add crates/omnish-store/
git commit -m "feat(store): add parent_session_id to SessionMeta"
```

---

### Task 3: Pass `parent_session_id` through daemon registration

**Files:**
- Modify: `crates/omnish-daemon/src/session_mgr.rs:34-72`
- Modify: `crates/omnish-daemon/src/server.rs:56-58`
- Test: `crates/omnish-daemon/tests/daemon_test.rs`

**Step 1: Write the failing test**

Add to `daemon_test.rs`:

```rust
#[tokio::test]
async fn test_session_register_with_parent() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());
    mgr.register("child1", Some("parent1".to_string()), HashMap::new()).await.unwrap();
    // Verify meta was saved with parent
    let active = mgr.list_active().await;
    assert!(active.contains(&"child1".to_string()));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-daemon`
Expected: FAIL — `register` doesn't accept `parent_session_id`

**Step 3: Update `register` signature and implementation**

In `session_mgr.rs`, change `register`:

```rust
pub async fn register(
    &self,
    session_id: &str,
    parent_session_id: Option<String>,
    attrs: std::collections::HashMap<String, String>,
) -> Result<()> {
    // ... existing code ...
    let meta = SessionMeta {
        session_id: session_id.to_string(),
        parent_session_id,
        started_at: now,
        ended_at: None,
        attrs,
    };
    // ... rest unchanged ...
}
```

In `server.rs`, update the `SessionStart` handler:

```rust
Message::SessionStart(s) => {
    mgr.register(&s.session_id, s.parent_session_id, s.attrs).await?;
}
```

**Step 4: Fix all existing `register` call sites in tests**

Update existing test calls to pass `None` as `parent_session_id`.

**Step 5: Run tests to verify they pass**

Run: `cargo test -p omnish-daemon`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/omnish-daemon/
git commit -m "feat(daemon): pass parent_session_id through registration"
```

---

### Task 4: Set `OMNISH_SESSION_ID` env var in child process

**Files:**
- Modify: `crates/omnish-pty/src/proxy.rs:14-49`
- Test: `crates/omnish-pty/tests/pty_test.rs`

**Step 1: Write the failing test**

Add to `pty_test.rs`:

```rust
#[test]
fn test_pty_env_var_propagated() {
    use std::collections::HashMap;
    let env = HashMap::from([("OMNISH_SESSION_ID".to_string(), "test123".to_string())]);
    let proxy = PtyProxy::spawn_with_env("/bin/sh", &["-c", "echo $OMNISH_SESSION_ID"], env).unwrap();
    let mut buf = [0u8; 256];
    std::thread::sleep(std::time::Duration::from_millis(200));
    let n = proxy.read(&mut buf).unwrap_or(0);
    let output = String::from_utf8_lossy(&buf[..n]);
    assert!(output.contains("test123"), "env var should be propagated, got: {}", output);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-pty`
Expected: FAIL — `spawn_with_env` doesn't exist

**Step 3: Add `spawn_with_env` to `PtyProxy`**

In `proxy.rs`, add a new method that sets env vars in the child process before `execvp`:

```rust
pub fn spawn_with_env(cmd: &str, args: &[&str], env: HashMap<String, String>) -> Result<Self> {
    let OpenptyResult { master, slave } =
        openpty(None, None).context("openpty failed")?;

    match unsafe { fork() }.context("fork failed")? {
        ForkResult::Child => {
            drop(master);
            setsid().ok();
            unsafe {
                libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY, 0);
            }
            dup2(slave.as_raw_fd(), 0).ok();
            dup2(slave.as_raw_fd(), 1).ok();
            dup2(slave.as_raw_fd(), 2).ok();
            if slave.as_raw_fd() > 2 {
                drop(slave);
            }

            // Set environment variables
            for (key, value) in &env {
                std::env::set_var(key, value);
            }

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
```

Refactor `spawn` to call `spawn_with_env` with an empty HashMap.

**Step 4: Run tests to verify they pass**

Run: `cargo test -p omnish-pty`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/omnish-pty/
git commit -m "feat(pty): add spawn_with_env for env var propagation to child"
```

---

### Task 5: Client reads and sets `OMNISH_SESSION_ID`

**Files:**
- Modify: `crates/omnish-client/src/main.rs:37-46, 238-267`

**Step 1: Read `OMNISH_SESSION_ID` and pass to `SessionStart`**

In `main()`, before spawning the PTY:

```rust
let parent_session_id = std::env::var("OMNISH_SESSION_ID").ok();
```

When spawning PTY, pass the env var:

```rust
let env = HashMap::from([("OMNISH_SESSION_ID".to_string(), session_id.clone())]);
let proxy = PtyProxy::spawn_with_env(&shell, &[], env)?;
```

In `connect_daemon`, accept and forward `parent_session_id`:

```rust
let msg = Message::SessionStart(SessionStart {
    session_id: session_id.to_string(),
    parent_session_id: parent_session_id.clone(),
    timestamp_ms: timestamp_ms(),
    attrs,
});
```

**Step 2: Run full workspace tests**

Run: `cargo test --workspace`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/omnish-client/
git commit -m "feat(client): detect nesting via OMNISH_SESSION_ID env var"
```

---

### Task 6: Leaf-session filtering in `omnish-commands`

**Files:**
- Modify: `crates/omnish-daemon/src/bin/commands.rs`

**Step 1: Add `--all` flag and leaf filtering**

In `load_all_commands`, also collect `parent_session_id` from each session's meta. After loading all commands, if `--all` is not set, filter out commands from sessions that have children (i.e., sessions whose `session_id` appears as another session's `parent_session_id`).

Add `--all` flag to arg parsing:

```rust
"--all" | "-a" => {
    show_all = true;
}
```

Filter logic:

```rust
if !show_all {
    // Collect all parent_session_ids
    let child_parents: HashSet<String> = all.iter()
        .filter_map(|c| c.parent_session_id.clone())
        .collect();
    // Keep only commands from sessions that are NOT parents of other sessions
    all.retain(|c| !child_parents.contains(&c.record.session_id));
}
```

**Step 2: Add `[nested]` indicator in display**

When `--all` is used, show `[N]` marker next to session ID for nested sessions.

**Step 3: Run full workspace tests**

Run: `cargo test --workspace`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/omnish-daemon/
git commit -m "feat(commands): default to leaf sessions, add --all flag"
```

---

### Task 7: Final integration test

**Files:**
- Test: `crates/omnish-daemon/tests/daemon_test.rs`

**Step 1: Write end-to-end nesting test**

```rust
#[tokio::test]
async fn test_nested_session_parent_child_relationship() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    // Register parent session
    mgr.register("parent1", None, HashMap::new()).await.unwrap();

    // Register child session with parent
    mgr.register("child1", Some("parent1".to_string()), HashMap::new()).await.unwrap();

    // Both should be active
    let active = mgr.list_active().await;
    assert!(active.contains(&"parent1".to_string()));
    assert!(active.contains(&"child1".to_string()));

    // End both
    mgr.end_session("child1").await.unwrap();
    mgr.end_session("parent1").await.unwrap();

    // Verify parent_session_id persisted in meta.json
    let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
    for entry in &entries {
        let meta = SessionMeta::load(&entry.path()).unwrap();
        if meta.session_id == "child1" {
            assert_eq!(meta.parent_session_id, Some("parent1".to_string()));
        } else {
            assert_eq!(meta.parent_session_id, None);
        }
    }
}
```

**Step 2: Run all tests**

Run: `cargo test --workspace`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/omnish-daemon/tests/
git commit -m "test(daemon): add nested session parent-child test"
```
