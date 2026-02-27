# Shell Info Periodic Polling Design

**Issue:** #14 — Let client periodically read shell info (shell name, PID, CWD, child process name/PID)

## Overview

Extend the existing Probe system to support periodic re-collection of dynamic shell properties. The client polls every 5 seconds, diffs against previous state, and sends updates to the daemon only when values change.

## Components

### 1. New Probes

- **`ShellCwdProbe(pid: u32)`** — reads `/proc/{pid}/cwd` symlink to get the shell's actual working directory. Separate from the existing `CwdProbe` (which reads the client process's own CWD via `std::env::current_dir()`).
- **`ChildProcessProbe(pid: u32)`** — uses the `procfs` crate to find direct children of the shell PID via `/proc/{pid}/task/{pid}/children`, then reads `/proc/{child_pid}/comm` for the process name. Returns a serialized string like `"vim:12345"` or empty string if no child foreground process.

Existing probes (`ShellProbe`, `PidProbe`, `TtyProbe`, `CwdProbe`, `HostnameProbe`) remain unchanged.

### 2. Polling ProbeSet

New helper function:

```rust
pub fn default_polling_probes(child_pid: u32) -> ProbeSet {
    let mut set = ProbeSet::new();
    set.add(Box::new(ShellCwdProbe(child_pid)));
    set.add(Box::new(ChildProcessProbe(child_pid)));
    set
}
```

### 3. Client Polling Loop

After PTY spawn and session start, the client spawns a tokio task:

1. Create `default_polling_probes(child_pid)`
2. Every 5 seconds, call `collect_all()`
3. Diff against last known `HashMap<String, String>`
4. If any values changed, send `Message::SessionUpdate` to daemon via `RpcClient`
5. Exit when cancellation token is triggered (shell exits)

### 4. Protocol: SessionUpdate Message

```rust
Message::SessionUpdate(SessionUpdate)

pub struct SessionUpdate {
    pub session_id: String,
    pub timestamp_ms: u64,
    pub attrs: HashMap<String, String>,  // only changed keys
}
```

### 5. Daemon Handling

- `server.rs`: handle `Message::SessionUpdate` by calling `SessionManager::update_attrs()`
- `session_mgr.rs`: new `update_attrs(session_id, attrs)` method that merges incoming attrs into the session's `SessionMeta.attrs`

## Dependencies

- `procfs` crate added to `omnish-client/Cargo.toml`

## Data Flow

```
Client (every 5s)              Daemon
──────────────────────────────────────
ShellCwdProbe.collect()
ChildProcessProbe.collect()
    │
    ├─ diff vs last_attrs
    │
    └─ if changed ──────→ SessionUpdate(changed_attrs)
                              │
                          update_attrs()
                              │
                          SessionMeta.attrs merged
```
