# RPC Transport Layer Redesign

## Overview

Redesign omnish-transport to provide RPC-style request/response multiplexing over a single connection, replacing the current raw send/recv model. All messages carry a `request_id` and receive a response, enabling safe concurrent usage from multiple tasks.

## Current Problems

1. Single `Mutex<UnixStream>` — send and recv are mutually exclusive, blocking concurrent operations
2. No request/response correlation — if two tasks send requests concurrently, recv can return the wrong response
3. No separation of read/write paths — a slow recv blocks all sends
4. fire-and-forget sends silently discard errors with `let _ =`

## Design

### Frame Protocol

Every message on the wire is wrapped in a `Frame`:

```rust
struct Frame {
    request_id: u64,
    payload: Message,  // existing omnish-protocol Message enum
}
```

Wire format: `[request_id: 8 bytes] [payload_len: 4 bytes] [payload: bincode bytes]`

### RpcClient (client side)

```rust
pub struct RpcClient {
    tx: mpsc::Sender<(Frame, oneshot::Sender<Message>)>,
    _read_task: JoinHandle<()>,
    _write_task: JoinHandle<()>,
}

impl RpcClient {
    /// Connect and spawn background read/write loops
    pub async fn connect(addr: &str) -> Result<Self>;

    /// Send a request, wait for the response with matching request_id
    pub async fn call(&self, msg: Message) -> Result<Message>;
}
```

Internals:
- `connect()` opens stream, calls `into_split()` to get independent read/write halves
- Spawns a **write task**: reads from `mpsc::Receiver`, assigns `request_id`, writes Frame, registers `oneshot::Sender` in pending map
- Spawns a **read task**: reads Frames from read half, looks up `request_id` in pending map, sends response via `oneshot::Sender`
- `call()` creates a `oneshot` channel pair, sends `(Frame, sender)` to write task via mpsc, awaits receiver
- `next_id` is `AtomicU64` for lock-free ID generation

### RpcServer (daemon side)

```rust
pub struct RpcServer {
    listener: TokioUnixListener,  // or TcpListener
}

impl RpcServer {
    pub async fn bind(addr: &str) -> Result<Self>;

    /// Accept connections, spawn a tokio task per connection
    pub async fn serve<F>(&mut self, handler: F) -> Result<()>
    where
        F: Fn(Message) -> Pin<Box<dyn Future<Output = Message> + Send>> + Send + Sync + 'static;
}
```

Per-connection task:
1. Read Frame from stream
2. Call `handler(frame.payload)` to get response Message
3. Write Frame with same `request_id` and response payload
4. Loop until connection closes

### Message Changes

Add an `Ack` variant to Message for empty responses:

```rust
enum Message {
    // existing variants...
    SessionStart(SessionStart),
    IoData(IoData),
    CommandComplete(CommandComplete),
    Request(Request),
    Response(Response),
    SessionEnd(SessionEnd),
    // new
    Ack,
}
```

All messages that previously had no response now return `Ack`.

### Message Flow

```
Client                              Daemon
  |                                   |
  |-- Frame(id=1, SessionStart) ----> |
  |<-- Frame(id=1, Ack) ------------- |
  |                                   |
  |-- Frame(id=2, IoData) ----------> |
  |-- Frame(id=3, IoData) ----------> |  (concurrent, no need to wait)
  |<-- Frame(id=2, Ack) ------------- |
  |<-- Frame(id=3, Ack) ------------- |
  |                                   |
  |-- Frame(id=4, Request) ---------> |
  |-- Frame(id=5, IoData) ----------> |  (doesn't block on id=4)
  |<-- Frame(id=5, Ack) ------------- |
  |<-- Frame(id=4, Response) -------- |  (responses can arrive out of order)
  |                                   |
```

### Crate Changes

- **omnish-protocol**: Add `Frame` struct, add `Message::Ack` variant
- **omnish-transport**: Replace `Connection`/`Listener`/`Transport` traits with `RpcClient`/`RpcServer`
- **omnish-client**: Replace `conn.send(&msg)` with `rpc.call(msg).await`, remove raw recv calls
- **omnish-daemon/server.rs**: Replace `handle_connection` loop with RpcServer handler

### Not In Scope

- Reconnection (separate follow-up)
- TCP transport (separate follow-up, but RpcClient/RpcServer work with any AsyncRead+AsyncWrite)
- Streaming responses (LLM streaming — future consideration)
