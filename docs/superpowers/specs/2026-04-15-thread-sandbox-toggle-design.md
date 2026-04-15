# `/thread sandbox on|off` — Per-Thread Sandbox Toggle

- Issue: #535
- Date: 2026-04-15

## Problem

Tool calls dispatched from a chat thread are sandboxed by default: the daemon
computes `ChatToolCall.sandboxed` per call by matching the tool input against
`permit_rules`. Users who trust a particular thread (e.g., an interactive
workflow where they're reviewing every tool call) currently have no way to
disable sandboxing short of adding coarse permit rules globally.

## Goal

Add `/thread sandbox on|off` so the user can toggle sandbox enforcement for a
single thread. State persists with the thread (survives resume), is authored in
the daemon, and the client surfaces the off state where the user will re-encounter
it.

Non-goals:
- Does not replace `/test lock on|off`, which toggles Landlock for the *shell
  process*, not tool calls.
- Does not add per-tool granularity; off means off for every `ChatToolCall`
  dispatched by the thread.

## Command Surface

```
/thread sandbox          — print current state for active thread
/thread sandbox on       — enable sandbox (default; clears override)
/thread sandbox off      — disable sandbox for the current thread
```

- If no thread exists yet, `on`/`off` are buffered client-side and applied as
  soon as the thread is lazily created. `/thread sandbox` (no arg) prints the
  buffered pending state.
- `on` clears the override to `None` rather than setting `Some(false)`.

## Data Model

Extend `ThreadMeta` in `crates/omnish-daemon/src/conversation_mgr.rs`:

```rust
/// Per-thread sandbox override. When Some(true), force sandboxed=false
/// for all ChatToolCall dispatched from this thread, bypassing permit_rules.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub sandbox_disabled: Option<bool>,
```

`Some(false)` is unused. Threads created before the feature landed deserialize
as `None` via `#[serde(default)]`.

## Protocol

No new top-level message type. Reuse the existing `Request`/`Response` RPC,
matching the `__cmd:thread del` / `__cmd:context chat` pattern:

```
__cmd:thread sandbox:<tid>        → "sandbox: on" | "sandbox: off"
__cmd:thread sandbox on:<tid>     → "sandbox enabled for thread <tid>"
__cmd:thread sandbox off:<tid>    → "sandbox disabled for thread <tid>"
```

Extend `ChatReady` with an optional field so the client can render a resume
warning without a follow-up round trip:

```rust
#[serde(default)]
pub sandbox_disabled: Option<bool>,
```

Bump `PROTOCOL_VERSION` from v17 to v18; `MIN_COMPATIBLE_VERSION` stays v14
(additive optional field).

## Daemon Wiring

**Sandbox decision** — `server.rs` around line 1678, in the
`ChatToolCall`-dispatch branch:

```rust
let thread_sandbox_off = conv_mgr
    .meta(&state.cm.thread_id)
    .and_then(|m| m.sandbox_disabled)
    .unwrap_or(false);

// ...
sandboxed: matched_rule.is_none() && !thread_sandbox_off,
```

The meta lookup is hot-path; cache once per agent-loop iteration (alongside
`state.cm` usage) rather than per tool call.

**RPC dispatcher** — add three handlers alongside `__cmd:thread del` and
`__cmd:thread stats`. `on`/`off` update `ThreadMeta` via
`ConversationManager::update_meta`, persist to `<thread>.meta.json`, and return
a confirmation string. Missing tid → `Response` with `error`.

**`/thread stats`** — existing stats handler prints an extra line
`sandbox: off` when `sandbox_disabled == Some(true)`.

## Client Wiring

**Handler** in `chat_session.rs` (alongside `handle_thread_del` /
`handle_thread_list`), dispatched before the generic `/` fallthrough:

```rust
if trimmed == "/thread sandbox"
    || trimmed == "/thread sandbox on"
    || trimmed == "/thread sandbox off"
{
    self.handle_thread_sandbox(trimmed, session_id, rpc).await;
    continue;
}
```

Logic:
- Parse sub-arg (`""`, `"on"`, `"off"`).
- If `current_thread_id.is_some()` → send `__cmd:thread sandbox[ on|off]:<tid>`
  and print the response.
- If `current_thread_id.is_none()`:
  - `on`/`off` → set `self.pending_sandbox_off = Some(off)` and print
    "will apply when a thread is created".
  - `""` → print the pending state, or "no active thread".

**Pending application** — in the path that handles `ChatReady` for a freshly
created thread (new_thread=true), immediately after recording `thread_id` and
before sending the first `ChatMessage`:

```rust
if let Some(off) = self.pending_sandbox_off.take() {
    let arg = if off { "off" } else { "on" };
    let query = format!("__cmd:thread sandbox {}:{}", arg, thread_id);
    let _ = rpc.call(/* Request with query */).await;
}
```

Holding the first `ChatMessage` until this RPC completes ensures no tool call
escapes the override.

**Resume warning** — in the resume path, after `ChatReady` arrives for an
existing thread, if `reply.sandbox_disabled == Some(true)` print a yellow
warning line (reuse existing `YELLOW` style) before handing control to the
chat loop.

## Edge Cases

- Thread resumed on a different client host: daemon is authoritative, so the
  override is still respected; warning is still rendered from `ChatReady`.
- Pre-feature threads: `sandbox_disabled` deserializes as `None` (sandbox on).
- `/thread del` on a disabled thread: meta is deleted with the thread; no
  special handling needed.
- Daemon-side tools (e.g., `command_query`) do not use the `sandboxed` flag, so
  the override only affects `ChatToolCall` dispatch for client-side tools. This
  matches the user's intent ("对 thread 关闭 sandbox" = Landlock bypass).

## Testing

**Unit**:
- `ThreadMeta` serde roundtrip with `sandbox_disabled = Some(true)` and absent.
- Daemon tool-dispatch: given a `ThreadMeta` with `sandbox_disabled=true` and
  no matching permit rule, the emitted `ChatToolCall` has `sandboxed=false`.

**Integration** (`tools/integration_tests/test_thread_sandbox.sh`, wired into
`.gitlab-ci.yml` alongside `test_config_backend.sh`):
1. Fresh thread → `/thread sandbox off` → trigger a client-side tool → assert
   the tool ran without Landlock (via event log line).
2. Buffered path: `/thread sandbox off` before any message → send first
   message → same assertion.
3. `/thread sandbox on` restores default; subsequent tool calls are sandboxed
   again.
4. `/thread sandbox` prints the current state; resumed thread shows warning.

## Rollout

- Single PR: protocol bump, daemon change, client change, integration test,
  changelog entry.
- No migration: optional field defaults to `None` on existing threads.
- Close #535 with the merge commit.
