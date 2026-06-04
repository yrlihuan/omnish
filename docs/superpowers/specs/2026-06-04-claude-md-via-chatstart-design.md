# Move CLAUDE.md Injection to Client via ChatStart Design

**Issue:** #637 - CLAUDE.md 注入在 client/daemon 跨机部署下失效

## Overview

The current `<project_instructions>` injection (PR #626) reads `<cwd>/CLAUDE.md`
inside the daemon process. When client and daemon run on different machines (or
different containers / mount namespaces), the daemon's filesystem view does not
contain the file at the path reported by the client, and the read silently
returns `NotFound`. The feature is dead in any cross-machine deployment.

This design moves the read to the **client side** and ships the prepared
content with `ChatStart`. The daemon caches it per-thread in memory and
appends it to the system prompt during `handle_chat_message`, with no
filesystem I/O of its own.

### Goals

- Eliminate daemon-side reading of client-provided paths.
- Refresh CLAUDE.md content on every chat entry (new or resume), including
  cross-machine thread resume (B-machine resume picks up B's CLAUDE.md).
- Minimize protocol surface and daemon state.
- Fail loudly on protocol mismatch (auth reject) rather than silently dropping
  CLAUDE.md, matching the existing project convention.

### Non-Goals

- Parent-directory walk (a-la Claude Code). Stay with `<cwd>/CLAUDE.md` only,
  matching #626 behavior.
- Refreshing CLAUDE.md mid-chat without re-entering chat. Users who edit
  CLAUDE.md mid-thread must `/exit` and re-enter to pick up changes.
- Backward compatibility with old peers via graceful degradation. We bump
  `MIN_COMPATIBLE_VERSION` and force coordinated upgrade.

## Architecture

CLAUDE.md flows as follows:

```
client.enter_chat:
    block = load_for_cwd(shell_cwd)         # Option<String>, pre-wrapped
    send ChatStart { project_instructions: block, ... }
      ↓ RPC
daemon.handle_chat_start:
    conv_mgr.set_project_instructions(thread_id, cs.project_instructions)
    reply ChatReady
      ↓
client.send ChatMessage { thread_id, query, ... }
      ↓ RPC
daemon.handle_chat_message:
    pi = conv_mgr.get_project_instructions(thread_id)   # Option<String>
    system = base_prompt + "\n\n" + reminder + (pi ? "\n\n" + pi : "")
    call LLM
      ↓ ... ↓
client.exit chat:
    send ChatEnd
      ↓
daemon.handle_chat_end:
    conv_mgr.clear_project_instructions(thread_id)
```

## Components

### 1. omnish-protocol

**`ChatStart` gains one field:**

```rust
pub struct ChatStart {
    pub request_id: String,
    pub session_id: String,
    pub new_thread: bool,
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Pre-formatted `<project_instructions>` block read from
    /// `<shell_cwd>/CLAUDE.md` on the client. `None` when absent or
    /// unreadable. Already truncated and wrapped; daemon appends as-is.
    #[serde(default)]
    pub project_instructions: Option<String>,
}
```

**Version bump:**

```rust
pub const PROTOCOL_VERSION: u32 = 24;
pub const MIN_COMPATIBLE_VERSION: u32 = 24;
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

**Callsites:** all three `Message::ChatStart(...)` constructions in
`chat_session.rs` (lines 1611, 2661, 2683) read `shell_cwd` from the existing
client-side state and pass `load_for_cwd(&shell_cwd)` into the new field.

Other `Message::*` constructions are not touched.

### 3. omnish-daemon: in-memory cache on ConversationManager

Add to `ConversationManager`:

```rust
project_instructions: Mutex<HashMap<String, String>>,
```

Methods:

```rust
pub fn set_project_instructions(&self, thread_id: &str, content: Option<String>) {
    let mut map = self.project_instructions.lock().unwrap();
    match content {
        Some(s) => { map.insert(thread_id.to_string(), s); }
        None => { map.remove(thread_id); }
    }
}

pub fn get_project_instructions(&self, thread_id: &str) -> Option<String> {
    self.project_instructions.lock().unwrap().get(thread_id).cloned()
}

pub fn clear_project_instructions(&self, thread_id: &str) {
    self.project_instructions.lock().unwrap().remove(thread_id);
}
```

**Callsites:**

- `handle_chat_start` (after the thread is successfully created or resumed -
  skip on `thread_locked` and any other error path that returns `ChatReady`
  with `error` set): `conv_mgr.set_project_instructions(&thread_id, cs.project_instructions.clone())`.
- `handle_chat_message` (`server.rs:1525-1532`): replace the
  `session_attrs.get("shell_cwd").and_then(load_project_instructions)` chain
  with `conv_mgr.get_project_instructions(&cm.thread_id)`.
- `handle_chat_end` (`server.rs:848`): `conv_mgr.clear_project_instructions(&ce.thread_id)`.

**Removed:**

- `MAX_PROJECT_INSTRUCTIONS_BYTES` constant (`server.rs:1420`).
- `load_project_instructions` function (`server.rs:1426-1463`).

## Data Flow & Concurrency

### Paths

**A. New thread:**
```
client: load_for_cwd -> Some(block_A)
        ChatStart{new_thread=true, thread_id=None, project_instructions=Some(block_A)}
daemon: create_thread -> T1
        set_project_instructions(T1, Some(block_A))
        reply ChatReady{thread_id=T1}
client: ChatMessage{thread_id=T1, ...}
daemon: get_project_instructions(T1) -> Some(block_A)
        system = base + "\n\n" + reminder + "\n\n" + block_A
        call LLM
```

**B. Resume thread (same or cross machine):**
```
client: load_for_cwd -> Some(block_B)   # fresh read from current shell_cwd
        ChatStart{new_thread=false, thread_id=Some(T1), project_instructions=Some(block_B)}
daemon: set_project_instructions(T1, Some(block_B))   # overwrites prior
        reply ChatReady
```

Resume always re-reads CLAUDE.md on the client, so:
- Daemon restart followed by client resume repopulates the cache.
- Cross-machine resume picks up B's CLAUDE.md (semantically: project
  instructions follow the current working directory, not the thread's birth
  host).

**C. No CLAUDE.md in cwd:**
```
client: load_for_cwd -> None
        ChatStart{..., project_instructions=None}
daemon: set_project_instructions(T, None)   # removes any prior entry
client: ChatMessage
daemon: get_project_instructions(T) -> None
        system = base + "\n\n" + reminder   # no <project_instructions>
```

### Concurrency

`Mutex<HashMap>` over `RwLock` because the critical sections are sub-microsecond
and writes are common (every ChatStart).

Potential races:
- **Concurrent ChatStart for the same thread** (two clients race to resume the
  same thread). The daemon's existing `thread_locked` mechanism prevents this
  in normal flows. If it ever happens, last-writer-wins is acceptable.
- **ChatMessage arriving before ChatStart.** Cannot happen: client awaits
  `ChatReady` before sending the first `ChatMessage`. Even if it did, `get`
  returns `None` and chat degrades to "no CLAUDE.md", no crash.
- **ChatEnd followed by stray ChatMessage.** `get` returns `None`, system
  prompt omits the block. Acceptable.

### Memory Footprint

Each entry is at most 128KB. Active threads are bounded (typically tens to
hundreds). Worst case ~120MB. No eviction hook for now; if thread counts grow
in the future a hook into thread eviction can be added later.

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
| `cs.project_instructions == None` | `set(thread_id, None)` removes any prior entry |
| map has no entry for thread | `get` returns `None`, system prompt omits block |
| daemon restart | cache empty; first new ChatStart (incl. resume) repopulates |

### Protocol Compatibility

| Combination | Outcome |
|---|---|
| new client + new daemon | works |
| new client + old daemon (PV 23) | auth rejected (`MIN_COMPATIBLE_VERSION=24`); explicit upgrade prompt |
| old client + new daemon | auth rejected; explicit upgrade prompt |
| cross-machine deployment | no longer reads daemon-side files; bug fixed |

## Testing

### omnish-protocol

- `ChatStart` bincode round-trip with `project_instructions = Some("...")` and
  `None`.
- Length sanity check: a `ChatStart` with `Some(non-empty)` serializes longer
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

- `ConversationManager::{set,get,clear}_project_instructions` basic
  behaviors, including `set(None)` overwriting `Some` to `None`.
- `handle_chat_start` integration: mock RPC, feed `ChatStart{project_instructions=Some("BLOCK")}`,
  assert `get` returns `Some("BLOCK")`.
- `handle_chat_message` integration: pre-seed cache, capture system prompt
  passed to the LLM mock, assert it contains `"BLOCK"`. With no pre-seed,
  assert it does not contain `<project_instructions>`.
- `handle_chat_end` integration: confirm `get` returns `None` afterward.

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

- Order of work: protocol → client module → daemon plumbing → integration
  tests. Keep removal of `load_project_instructions` in the same patch as the
  new daemon code so there is no transient state where both code paths run.
- The build is always release per project convention (`CLAUDE.md`).
- Do not run `omnish-daemon` from automation; ask the user to start it.
