# Nested omnish Detection & Deduplication

## Problem

Running omnish inside omnish (directly or via tmux) causes duplicate I/O recording. Both layers connect to the same daemon and record overlapping data independently.

## Approach

**Environment variable marking + query-time deduplication.**

Both layers record normally. The parent-child relationship is tracked via a first-class `parent_session_id` field. Deduplication happens at query time, not recording time.

## Design

### 1. Environment Variable Propagation

omnish-client on startup:
- Reads `OMNISH_SESSION_ID` env var
  - Present -> use as `parent_session_id`
  - Absent -> top-level session (parent_session_id = None)
- Sets `OMNISH_SESSION_ID=<own session_id>` in the child shell's environment

```
omnish (session=abc123, parent=None)
  export OMNISH_SESSION_ID=abc123
  -> bash
     -> tmux
        |- omnish (session=def456, parent=abc123)
        |    export OMNISH_SESSION_ID=def456
        |    -> zsh
        |- omnish (session=ghi789, parent=abc123)
             export OMNISH_SESSION_ID=ghi789
             -> bash
```

### 2. Protocol Change

`SessionStart` message gains a first-class field:

```rust
SessionStart {
    session_id: String,
    parent_session_id: Option<String>,  // NEW
    attrs: HashMap<String, String>,
}
```

### 3. Storage Change

`SessionMeta` gains a first-class field:

```rust
pub struct SessionMeta {
    pub session_id: String,
    pub parent_session_id: Option<String>,  // NEW
    pub started_at: String,
    pub ended_at: Option<String>,
    pub attrs: HashMap<String, String>,
}
```

Backward compatible via `#[serde(default)]`.

### 4. Query-Time Deduplication

- `omnish-commands`: default shows only leaf sessions (no children). `--all` flag shows everything.
- LLM catalog: marks commands with `is_nested: bool` so the LLM knows which may be duplicates.
- Session tree can be reconstructed from `parent_session_id` links.

### 5. Unchanged Behavior

- Both layers record stream.bin, commands.json normally
- Daemon does no filtering or pausing
- `::` command interception works at all levels
- Graceful degradation unaffected
