# Protocol-Channel Binary Distribution (Issue #346)

## Problem

The daemon's auto-update currently distributes binaries to remote client machines via SSH (`deploy.sh`). This requires SSH access, pre-configured host lists, and external scripts. The daemon and client already have a persistent protocol channel — binary distribution should use it.

## Solution

Add a client-initiated update polling mechanism over the existing daemon-client protocol. The daemon caches full release packages and serves them to clients on request. The existing mtime-based `execvp()` trigger and SSH deploy remain untouched — this is an additional distribution channel.

## Flow

```
Client                              Daemon
  |                                   |
  |-- UpdateCheck {os, arch, ver} --> |
  |                                   |  (check cache)
  |<-- UpdateInfo {ver, available} -- |
  |                                   |
  | (if available:)                   |
  |-- UpdateRequest {os, arch} -----> |
  |                                   |  (read cached package)
  |<-- UpdateChunk {seq=0, meta} ---- |
  |<-- UpdateChunk {seq=1, data} ---- |
  |<-- UpdateChunk {seq=N, done} ---- |
  |                                   |
  | (write temp, verify, rename)      |
  |                                   |
  | ... mtime polling detects change → execvp()
```

## Protocol Messages

Four new variants added to `Message`:

```rust
/// Client → Daemon: periodic version check
UpdateCheck {
    os: String,       // e.g., "linux", "darwin"
    arch: String,     // e.g., "x86_64", "aarch64"
    current_version: String,
}

/// Daemon → Client: version info response
UpdateInfo {
    latest_version: String,
    available: bool,
}

/// Client → Daemon: request binary download
UpdateRequest {
    os: String,
    arch: String,
}

/// Daemon → Client: binary chunk stream
UpdateChunk {
    seq: u32,           // 0-based sequence number
    total_size: u64,    // total package size in bytes (repeated in every chunk for simplicity)
    checksum: String,   // SHA-256 of the full package (repeated in every chunk)
    data: Vec<u8>,      // chunk payload (empty in final chunk if done)
    done: bool,         // true on last chunk
}
```

Protocol version bumped to 10.

## Daemon Side

### Package cache

Location: `~/.omnish/updates/{os}-{arch}/omnish-{version}.tar.gz`

After the daemon's own `install.sh --upgrade` succeeds, it copies the downloaded package into the cache for its own platform. Other platforms are fetched on-demand.

Cached packages persist until replaced by a newer version.

### UpdateCheck handler

1. Look up `~/.omnish/updates/{os}-{arch}/` for the latest cached package.
2. Compare version against `current_version` from the client.
3. If newer: reply `UpdateInfo { latest_version, available: true }`.
4. If no cached package for that platform: kick off a background download from the release source (same `check_url` used by `install.sh`), reply `UpdateInfo { available: false }`.
5. If same or older: reply `UpdateInfo { available: false }`.

### Download deduplication

A `HashMap<(String, String), ()>` (or `HashSet`) tracks in-flight downloads by `(os, arch)`. When an `UpdateCheck` triggers a download:
- If already downloading for that platform → skip, reply `available: false`.
- If not → insert entry, spawn download task.
- On download complete → remove entry.

This prevents multiple clients of the same platform from triggering redundant downloads.

### UpdateRequest handler

1. Open the cached package file for the requested `(os, arch)`.
2. Stream the file in 64KB chunks as `UpdateChunk` messages.
3. If no cached package exists: send a single `UpdateChunk { done: true, total_size: 0 }` to signal "not available."

Multiple concurrent `UpdateRequest`s from different clients each get their own read stream — no conflict since the file is read-only.

## Client Side

### Polling

Simple periodic timer (e.g., every 60 seconds). Sends `UpdateCheck { os, arch, current_version }` to daemon over the existing connection. No idle/prompt/alt-screen guards — those conditions only matter for the `execvp()` trigger in the mtime check.

### Download flow

1. Receive `UpdateInfo { available: true }` → send `UpdateRequest { os, arch }`.
2. Receive `UpdateChunk` stream → write sequentially to a temp file (e.g., `~/.omnish/tmp/update-{version}.tar.gz`).
3. On final chunk (`done: true`): verify SHA-256 checksum against `checksum` field.
4. Extract the package (tar.gz containing both daemon and client binaries).
5. Copy the client binary to the correct path (overwriting current binary via atomic temp-write + rename).
6. Existing mtime polling detects the change → `execvp()`.

### Package extraction

The daemon sends the **full release package** (tar.gz with all binaries). The client extracts only the client binary for its own platform and writes it into place. The package format is the same as the one used by `install.sh`.

### Fallback

If the daemon is unreachable, doesn't support update messages (old daemon), or replies `available: false`, nothing happens. SSH deploy and mtime polling continue to work independently.

## Interaction with Existing Mechanisms

| Mechanism | Role | Changed? |
|-----------|------|----------|
| `install.sh --upgrade` (daemon cron) | Self-update daemon binary | No |
| SSH `deploy.sh` | Push binary to remote hosts | No (can be deprecated later) |
| Mtime polling (client) | Detect new binary, trigger `execvp()` | No |
| `/update` command (client) | Manual trigger | No |
| **Protocol distribution (new)** | Get binary onto client disk via daemon | **New** |

## Files to Modify

- `crates/omnish-protocol/src/message.rs` — add 4 new message variants, bump protocol version
- `crates/omnish-daemon/src/auto_update.rs` — package caching after self-update, background download for other platforms
- `crates/omnish-daemon/src/server.rs` — handle `UpdateCheck` and `UpdateRequest` messages
- `crates/omnish-client/src/main.rs` — periodic `UpdateCheck` polling, download/extract flow
- `crates/omnish-common/src/config.rs` — no changes expected (reuses existing `auto_update` config)
