# Client Config Push Design

## Problem

`/config` writes all settings to `daemon.toml`, but client-side settings (command_prefix, completion_enabled, etc.) are loaded from `client.toml` at startup only. Changes made via `/config` don't take effect until the client restarts, and even then only if the user manually edits `client.toml`.

## Solution

Daemon owns client config in `daemon.toml` under a `[client]` section. ConfigWatcher detects changes and pushes them to all connected clients via a new `ConfigClient` message. Clients apply changes at runtime (hot-reload) and write back to `client.toml` as a local cache.

## Protocol

New message variant:

```rust
Message::ConfigClient { changes: Vec<ConfigChange> }
```

Reuses existing `ConfigChange { path: String, value: String }`. New enum variant requires protocol version bump (v13 → v14) since bincode can't deserialize unknown variants. Unknown paths within the changes list are silently ignored (forward-compatible for adding new config keys).

Direction: daemon → client only.

## daemon.toml Structure

New `[client]` section:

```toml
[client]
command_prefix = ":"
resume_prefix = "::"
completion_enabled = true
ghost_timeout_ms = 10000
intercept_gap_ms = 1000
developer_mode = false
```

`config_schema.toml` toml_keys updated from `shell.*` to `client.*`.

## ConfigWatcher Integration

`ConfigSection` enum gains a `Client` variant. ConfigWatcher diffs the `[client]` section on file change and publishes to subscribers, same mechanism as Llm/Plugins/Tasks hot-reload.

Daemon server subscribes to `ConfigSection::Client`. On change, pushes `ConfigClient` to all connected clients.

## Transport - Per-Connection Push

Current RPC layer is request-response only. Add push capability:

1. **Connection registry** in daemon: `Arc<Mutex<HashMap<ConnectionId, mpsc::Sender<Message>>>>`.
2. `spawn_connection()` creates an additional `mpsc::channel` (push channel). The connection's read loop `select!`s on both normal request processing and the push channel receiver.
3. Push messages are written with `request_id = 0` (sentinel for unsolicited messages).
4. On connection close, entry is removed from the registry.

Client-side `RpcClient` read loop routes `request_id = 0` frames to a `push_rx: mpsc::Receiver<Message>` channel for the main loop to consume.

## Push Timing

1. **Auth success** - daemon immediately pushes full `[client]` config.
2. **Config change** - after ConfigWatcher detects `[client]` section change, pushes changed entries to all connected clients.

## Client Hot-Reload

On receiving `ConfigClient`, the client:

1. Updates runtime components by matching on path:
   - `client.command_prefix` → `InputInterceptor::update_prefix()`
   - `client.resume_prefix` → `InputInterceptor::update_resume_prefix()`
   - `client.developer_mode` → `InputInterceptor::set_developer_mode()`
   - `client.intercept_gap_ms` → `TimeGapGuard::update_min_gap()`
   - `client.completion_enabled` → mutable local variable
   - `client.ghost_timeout_ms` → mutable local variable
2. Writes back to `client.toml` if any value differs from current.

New setter methods needed on `InputInterceptor` and `TimeGapGuard`.

## client.toml Role

`client.toml` serves as a local cache of daemon-pushed config. Fields split into two categories:

| Field | Source | Overwritten by daemon push? |
|-------|--------|-----------------------------|
| `daemon_addr` | local only | No |
| `shell.command` | local only | No |
| `onboarded` | local only | No |
| `command_prefix` | daemon push | Yes |
| `resume_prefix` | daemon push | Yes |
| `completion_enabled` | daemon push | Yes |
| `ghost_timeout_ms` | daemon push | Yes |
| `intercept_gap_ms` | daemon push | Yes |
| `developer_mode` | daemon push | Yes |

### Startup flow

```
Client starts
  → Load client.toml (includes previously cached daemon values)
  → Connect to daemon, Auth
  → Daemon pushes ConfigClient (current [client] section)
  → Client diffs, updates components + writes client.toml if changed

No daemon available
  → client.toml has last-known values, client works normally
```
