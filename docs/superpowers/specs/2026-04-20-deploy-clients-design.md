# Deploy Clients via Config Menu - Design

Issue: dev/omnish#560

## Goal

Add a `General > Clients` submenu that lets the user deploy the omnish client
binary, TLS cert, auth token, and `client.toml` to a remote host over SSH,
without leaving the config TUI.

The feature is only useful when the daemon is reachable over TCP (clients on
other hosts), so the submenu is hidden when `listen_addr` is a Unix socket.

## Menu structure

```
General
  Clients                          # only visible when listen_addr is TCP
    ├─ Add client                  # one-shot manual entry
    │    ├─ Target                 # text: "user@host"
    │    └─ Deploy                 # toggle: triggers deploy, then resets
    ├─ alice.local [active]        # one entry per unique session hostname
    │    ├─ SSH user               # text, default = daemon process's $USER
    │    └─ Deploy                 # toggle: triggers deploy, then resets
    └─ bob-host
```

- `[active]` tag is appended to the entry label whenever the host currently has
  a live TCP client connection.
- Discovered hosts are derived from the in-memory session map's
  `attrs["hostname"]`, dedup'd. The list is volatile - it shrinks as sessions
  are evicted from memory.
- `Add client` is one-shot: the daemon does not persist any list of enrolled
  clients. Once the deploy succeeds, the new client will connect and appear in
  the discovered list automatically.

## Deploy execution

Reuses the existing `~/.omnish/deploy.sh` script (already shipped by
`install.sh` and used by the `auto_update` task).

1. The daemon spawns `bash $OMNISH_DIR/deploy.sh user@host` as a background
   tokio task.
2. The `ConfigUpdate` RPC that triggered the deploy returns immediately - the
   menu does not show a "started" status.
3. When the script exits, the daemon pushes a `NoticePush` message to the
   originating client. The client renders it through the existing
   `InlineNotice` widget.
   - Success: `Deployed to user@host`
   - Failure: `Deploy to user@host failed: <last stderr line>`

`deploy.sh` already handles ssh/scp, permission setting, and graceful errors
(skips on connection failure). Re-using it avoids duplicating ssh logic in
Rust and keeps manual-deploy and auto-update behavior in sync.

## Protocol additions

A new push message variant:

```rust
Message::NoticePush {
    level: NoticeLevel,    // Info | Warn | Error
    text:  String,
}
```

Routed through the existing `PushRegistry` mechanism that already carries
`ConfigClient`. `PROTOCOL_VERSION` is bumped; `MIN_COMPATIBLE_VERSION` stays at
v14 because the new variant is appended at the end of the enum.

## Config schema additions

`config_schema.toml`:

```toml
[[items]]
path = "general.clients"
label = "Clients"
kind = "submenu"

[[items]]
path = "general.clients.__add__"
label = "Add client"
kind = "submenu"
handler = "add_client"

[[items]]
path = "general.clients.__add__.target"
label = "Target (user@host)"
kind = "text"

[[items]]
path = "general.clients.__add__.deploy"
label = "Deploy"
kind = "toggle"
```

`config_schema.rs`:

- `build_config_items` skips emitting the `general.clients.*` items when
  `config.listen_addr` is a Unix socket path.
- New helper `build_client_items(session_hostnames)` is appended to the dynamic
  items section (similar to `build_plugin_items`). For each
  `(hostname, is_active)` pair it emits:
  - A submenu handler `deploy_client` registered with path
    `general.clients.<quoted_hostname>` and label `<hostname> [active]` (or
    just `<hostname>` when not active).
  - `general.clients.<quoted_hostname>.ssh_user` (text, default = `$USER`).
  - `general.clients.<quoted_hostname>.deploy` (toggle).
  - `<quoted_hostname>` follows the same convention as backend names: wrapped
    in `"..."` when the hostname contains a `.` so `config_edit::split_key_path`
    treats it as a single segment (e.g. `general.clients."server1.local"`).

Handlers in `apply_config_changes`:

- `add_client`: reads the `target` field, validates it parses as `user@host`,
  ignores the change if `deploy` is not toggled true. Spawns the deploy task.
- `deploy_client`: derives hostname from the change path
  (`general.clients.<hostname>.deploy`), reads the `ssh_user` field (defaulting
  to the daemon process's `$USER`), and spawns the deploy task with
  `<ssh_user>@<hostname>`.

Both handlers are no-ops when the deploy toggle is left false (so just editing
the SSH user without deploying does nothing). The toggle has no persistent
backing - it is rebuilt as `false` every time the menu is opened, so it
naturally resets after each use.

## SessionManager API

```rust
impl SessionManager {
    /// Returns (hostname, is_active) for each unique hostname seen in
    /// in-memory sessions. `is_active` is true when at least one session for
    /// that hostname has an open transport connection.
    pub async fn list_hostnames(&self) -> Vec<(String, bool)>;
}
```

Empty / missing `attrs["hostname"]` values are skipped. Hostnames are sorted
alphabetically for stable menu order.

## File layout

New:

- `crates/omnish-daemon/src/deploy.rs` (~80 lines): spawn `deploy.sh`, capture
  the last non-empty stderr line on failure, push the `NoticePush` to the
  client whose RPC triggered the deploy.

Modified:

- `crates/omnish-daemon/src/config_schema.toml` - new menu items.
- `crates/omnish-daemon/src/config_schema.rs` - skip when Unix socket, emit
  per-host items, dispatch `add_client` / `deploy_client` handlers.
- `crates/omnish-daemon/src/session_mgr.rs` - new `list_hostnames` method.
- `crates/omnish-daemon/src/server.rs` - wire deploy handler dispatch into
  `Message::ConfigUpdate` flow; carry the originating connection's push
  channel into the spawned task so the notice is delivered to the right
  client.
- `crates/omnish-protocol/src/message.rs` - add `NoticePush` variant + bump
  `PROTOCOL_VERSION`.
- `crates/omnish-client/src/main.rs` - handle inbound `NoticePush` and render
  via `InlineNotice`.

## Edge cases

- `attrs["hostname"]` empty or missing - the session is skipped from the list.
- Same hostname across multiple sessions - single menu entry; `[active]` shown
  if any of them has a live transport connection.
- `deploy.sh` not present at `$OMNISH_DIR/deploy.sh` - notice reports the
  missing-script error.
- SSH connection failure - `deploy.sh` already prints `WARN: Cannot connect`;
  daemon captures the last stderr line into the notice.
- `Target` field on `Add client` not parseable as `user@host` - notice with
  `Invalid target: expected user@host`.
- Daemon `listen_addr` is a Unix socket - the entire `general.clients`
  submenu and dynamic per-host items are omitted.
- The deploy task outlives the originating RPC - if the client disconnects
  before the script finishes, the notice is dropped silently.

## Out of scope

- Persisting an enrolled-clients list anywhere (no `[deploy.clients]` section
  in `daemon.toml`).
- Coupling with the `[tasks.auto_update] clients` list - that field continues
  to be edited by hand for now.
- Streaming deploy progress live to the menu.
- Pure-Rust SSH (we shell out to `ssh`/`scp` via `deploy.sh`).
