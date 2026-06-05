# Move CLAUDE.md Injection from Daemon to Client Design

**Issue:** #637 - CLAUDE.md 注入在 client/daemon 跨机部署下失效

> **Revision 2 (post-review):** The original design (sections below tagged R1)
> attached `project_instructions` to `ChatStart` with a per-thread cache on the
> daemon. Code review flagged that `shell_cwd` can change mid-chat via two
> paths (`ResumeMismatchAction::CdToOld` after thread resume, and tool-driven
> cwd changes during the agent loop), which the cache would silently ignore.
> The design now attaches `project_instructions` to **`ChatMessage`** instead.
> Daemon reads `cm.project_instructions` directly each turn; no per-thread
> cache. This eliminates the stale-content failure mode at the cost of
> re-sending CLAUDE.md (typically a few KB) with each user message, which
> is acceptable given chat turn rate.

## Overview

The current `<project_instructions>` injection (PR #626) reads `<cwd>/CLAUDE.md`
inside the daemon process. When client and daemon run on different machines (or
different containers / mount namespaces), the daemon's filesystem view does not
contain the file at the path reported by the client, and the read silently
returns `NotFound`. The feature is dead in any cross-machine deployment.

This design moves the read to the **client side** and ships the prepared
content with each `ChatMessage`. The daemon reads it directly from the message
and appends it to the system prompt for that turn, with no filesystem I/O of
its own and no per-thread state.

### Goals

- Eliminate daemon-side reading of client-provided paths.
- Reflect current client cwd on every chat turn, including mid-chat changes
  driven by resume cwd negotiation or tool execution.
- Minimize protocol surface and daemon state.
- Fail loudly on protocol mismatch (auth reject) rather than silently dropping
  CLAUDE.md, matching the existing project convention.

### Non-Goals

- Parent-directory walk (a-la Claude Code). Stay with `<cwd>/CLAUDE.md` only,
  matching #626 behavior.
- Backward compatibility with old peers via graceful degradation. We bump
  `MIN_COMPATIBLE_VERSION` and force coordinated upgrade.

## Architecture

CLAUDE.md flows as follows:

```
client.send_chat_message:
    block = load_for_cwd(self.shell_cwd)    # Option<String>, pre-wrapped
    send ChatMessage { query, project_instructions: block, ... }
      ↓ RPC
daemon.handle_chat_message:
    pi = cm.project_instructions
    system = base_prompt + "\n\n" + reminder + (pi ? "\n\n" + pi : "")
    call LLM
```

`ChatStart`/`ChatEnd` no longer carry any project-instructions state. Each
`ChatMessage` is self-contained: the client reads `<shell_cwd>/CLAUDE.md` at
send time, so any mid-chat cwd change (via `ResumeMismatchAction::CdToOld` or
tool-driven cwd updates) is reflected on the next turn without any refresh
protocol path.

## Components

### 1. omnish-protocol

**`ChatMessage` gains one field:**

```rust
pub struct ChatMessage {
    pub request_id: String,
    pub session_id: String,
    pub thread_id: String,
    pub query: String,
    pub model: Option<String>,
    /// Pre-formatted `<project_instructions>` block read from
    /// `<shell_cwd>/CLAUDE.md` on the client at message-send time. `None`
    /// when absent or unreadable. Already truncated and wrapped; daemon
    /// appends as-is.
    #[serde(default)]
    pub project_instructions: Option<String>,
}
```

`ChatStart` is unchanged from its #626 shape (no new field).

**Version bump:**

```rust
pub const PROTOCOL_VERSION: u32 = 25;
pub const MIN_COMPATIBLE_VERSION: u32 = 25;
```

Rationale: bincode 1.x is positional. Adding a field to an existing variant is
a breaking change for `old_client -> new_daemon` (`UnexpectedEof` on the new
trailing field). The project convention (`message.rs:11-15`) prescribes
bumping both for modified existing variants. The cost is one auth-time
rejection; the benefit is that mismatched peers fail immediately and visibly
rather than silently dropping `project_instructions`.

Bincode 2.x migration and tagged-encoding alternatives (protobuf/CBOR) were
considered and rejected as out-of-scope for this feature; they may be
revisited as an independent protocol-layer cleanup.

### 2. omnish-client: new `project_instructions` module

`crates/omnish-client/src/project_instructions.rs`:

```rust
const MAX_BYTES: usize = 128 * 1024;

/// Read `<cwd>/CLAUDE.md`, truncate at a char boundary if necessary,
/// and wrap in a `<project_instructions>` block. Returns `None` when
/// `cwd` is empty, the file is absent, or it is unreadable.
pub fn load_for_cwd(cwd: &str) -> Option<String> {
    if cwd.is_empty() { return None; }
    let path = Path::new(cwd).join("CLAUDE.md");
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::NotFound => return None,
        Err(e) => {
            crate::event_log::push(
                format!("project_instructions: read failed at {}: {}", path.display(), e)
            );
            return None;
        }
    };
    let (body, truncated) = if content.len() > MAX_BYTES {
        let mut end = MAX_BYTES;
        while !content.is_char_boundary(end) { end -= 1; }
        (&content[..end], true)
    } else {
        (content.as_str(), false)
    };
    let tail = if truncated {
        "\n[... truncated: file exceeded 128KB ...]\n"
    } else {
        "\n"
    };
    Some(format!(
        "<project_instructions>\nSource: {}\n\n{}{}</project_instructions>",
        path.display(), body, tail
    ))
}
```

**Callsites:** the primary `Message::ChatMessage(...)` construction in
`chat_session.rs` (the user-message send path) reads `self.shell_cwd` from
existing client-side state and passes `load_for_cwd(&shell_cwd)` into the new
field. The secondary `ChatMessage` site that sends an empty-query model-change
ack passes `project_instructions: None` because the daemon short-circuits on
empty query before building any system prompt.

`Message::ChatStart(...)` constructions are not modified.

### 3. omnish-daemon: direct read from `ChatMessage`

No new fields or methods on `ConversationManager`. In `handle_chat_message`,
replace the old filesystem read:

```rust
let project_instructions = session_attrs
    .get("shell_cwd")
    .and_then(|cwd| load_project_instructions(cwd));
```

with:

```rust
let project_instructions = cm.project_instructions.clone();
```

The existing concatenation block (`format!("{}\n\n{}\n\n{}", ...)`) is
unchanged.

**Removed:**

- `MAX_PROJECT_INSTRUCTIONS_BYTES` constant (`server.rs`).
- `load_project_instructions` function (`server.rs`).

## Data Flow

### Paths

**A. Standard user turn:**
```
client: load_for_cwd(self.shell_cwd) -> Some(block)
        ChatMessage{ query, project_instructions: Some(block), ... }
daemon: pi = cm.project_instructions
        system = base + "\n\n" + reminder + "\n\n" + block
        call LLM
```

**B. Mid-chat cwd change (CdToOld after resume, or tool-driven cd):**
```
client: previously at /projectB; user resumes T1 (born in /projectA), picks CdToOld
        self.shell_cwd updated to /projectA
        user sends next message
        load_for_cwd(/projectA) -> Some(block_from_A)
        ChatMessage{ ..., project_instructions: Some(block_from_A) }
daemon: uses block_from_A for this turn
```

**C. No CLAUDE.md in cwd:**
```
client: load_for_cwd -> None
        ChatMessage{ ..., project_instructions: None }
daemon: system = base + "\n\n" + reminder   # no <project_instructions>
```

### State

The daemon holds no project-instructions state across messages. Each turn is
self-contained. There is no race window, no cache invalidation, no eviction
concern.

## Error Handling

### Client

| Condition | Behavior |
|---|---|
| `cwd.is_empty()` | return `None` (defensive) |
| file does not exist | return `None` (silent, normal case) |
| permission denied / I/O error | return `None`, log to `event_log` |
| file > 128KB | truncate at char boundary, append marker, wrap, return `Some(...)` |
| invalid UTF-8 | `read_to_string` errors, log + return `None` |

### Daemon

| Condition | Behavior |
|---|---|
| `cm.project_instructions == None` | system prompt omits the `<project_instructions>` block |
| daemon restart | no state to recover; next ChatMessage carries fresh content |

### Protocol Compatibility

| Combination | Outcome |
|---|---|
| new client + new daemon | works |
| new client + old daemon (PV < 25) | auth rejected (`MIN_COMPATIBLE_VERSION=25`); explicit upgrade prompt |
| old client + new daemon | auth rejected; explicit upgrade prompt |
| cross-machine deployment | no longer reads daemon-side files; bug fixed |

## Testing

### omnish-protocol

- `ChatMessage` bincode round-trip with `project_instructions = Some("...")` and
  `None`.
- Length sanity check: a `ChatMessage` with `Some(non-empty)` serializes longer
  than the same with `None`, guarding against accidental field removal.

### omnish-client (`project_instructions` module)

Pure-function unit tests against `tempdir`:
- Happy path: write `CLAUDE.md`, assert `Some(wrapped)`.
- Missing file: `None`.
- Empty cwd: `None`.
- Oversize file (200KB): result around 128KB plus wrapper + truncation marker;
  truncation occurs at a char boundary.
- Multibyte char boundary: write content where 128KB falls inside a CJK
  character; assert `is_char_boundary` of the cut.
- Invalid UTF-8: `None`.

### omnish-daemon

No daemon-side unit tests are added in R2 (the cache is gone). The
ChatMessage round-trip in `omnish-protocol` covers the wire format; the
manual smoke regression covers the end-to-end injection path. If
mock-RPC handler tests are added to the codebase later, a focused test
on `handle_chat_message` reading `cm.project_instructions` should be
included then.

### Cross-machine manual regression

Not in CI (needs two hosts). Document in PR description:

```
# On client host A:
cd /some/project; echo "TEST_SENTINEL_$(date +%s)" >> CLAUDE.md
omnish    # connects to remote daemon on host B
# Enter chat. Ask the LLM: "what's in the project_instructions you see?"
# Expect: the LLM repeats the sentinel string.
```

### Removed Tests

Any unit test for the deleted `load_project_instructions` function in `server.rs`.

## Implementation Notes

- Order of work: protocol -> client module -> daemon plumbing. Keep removal
  of `load_project_instructions` in the same patch as the new daemon code so
  there is no transient state where both code paths run.
- The build is always release per project convention (`CLAUDE.md`).
- Do not run `omnish-daemon` from automation; ask the user to start it.
