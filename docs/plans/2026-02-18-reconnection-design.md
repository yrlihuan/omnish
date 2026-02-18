# RPC Client Reconnection Design

## Overview

Add automatic reconnection to RpcClient so that when the daemon connection drops, the client transparently re-establishes the connection and re-registers the session, without interrupting the PTY shell.

## Current Behavior

When the connection drops:
- RpcClient's read/write tasks exit
- All subsequent `call()` return errors ("write task closed" / "read task closed")
- Client silently ignores errors via `let _ = rpc.call(msg).await`
- PTY continues working but daemon receives no data
- No recovery — connection stays dead for the rest of the session

## Design

### RpcClient Changes

Add `connect_unix_with_reconnect` constructor that accepts:
- `addr: &str` — socket path (stored for reconnection)
- `on_reconnect` callback — called after connection is re-established (e.g. to re-send SessionStart)

When the read or write task detects a connection break:
1. Mark connection as unavailable
2. Spawn a background reconnect task
3. Reconnect loop: try `connect_unix` with exponential backoff (1s, 2s, 4s, 8s... max 30s), infinite retries
4. On successful connect: call `on_reconnect` callback
5. If callback succeeds: replace internal read/write tasks, mark connection available
6. If callback fails: continue retry loop

**All `call()` invocations return an error immediately while connection is unavailable** — both in-flight calls at the moment of disconnect and new calls during reconnection are treated identically.

### API

```rust
impl RpcClient {
    // Existing: one-shot connection, no reconnect
    pub async fn connect_unix(addr: &str) -> Result<Self>;

    // New: connection with auto-reconnect
    pub async fn connect_unix_with_reconnect(
        addr: &str,
        on_reconnect: impl Fn(&RpcClient) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
            + Send + Sync + 'static,
    ) -> Result<Self>;
}
```

### Reconnection Flow

```
Connection drops
  → read/write tasks exit
  → all pending call() oneshots get dropped → callers receive error
  → reconnect task spawns
  → loop:
      connect_unix(addr)
      → fail: sleep(backoff), backoff = min(backoff * 2, 30s), retry
      → success:
          on_reconnect(client) — e.g. send SessionStart
          → fail: sleep(backoff), retry
          → success: replace read/write tasks, resume normal operation
  → during reconnect: new call() returns error immediately
```

### Exponential Backoff

- Initial interval: 1 second
- Multiplier: 2x
- Maximum interval: 30 seconds
- Maximum retries: unlimited

### Daemon Side: Idempotent register()

`SessionManager::register()` must handle reconnection gracefully. If session_id already exists in the HashMap:
- Skip directory creation
- Reuse existing StreamWriter and commands
- Update attrs if changed

### Client Side Usage

```rust
let rpc = RpcClient::connect_unix_with_reconnect(&socket_path, move |rpc| {
    let sid = session_id.clone();
    let psid = parent_session_id.clone();
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
}).await?;
```

Main loop code stays unchanged — `let _ = rpc.call(msg).await` already handles errors gracefully.

### Not In Scope

- Server-side heartbeat or keepalive
- Buffering messages during reconnection
- TCP transport reconnection (same mechanism will work, but not tested)
