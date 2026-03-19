# Multi-Session Command Association Design

## Problem

omnish aggregates terminal I/O from multiple sessions, but currently treats each session as a flat stream. Users working across 10+ terminals need to query related work across sessions. The system should automatically identify relevant commands and let the LLM decide which details to pull.

## Key Decisions

- **Association unit is the command, not the session.** Instead of linking entire sessions, individual commands are the granular unit of relevance.
- **Automatic inference of relevance** — no manual tagging or grouping required.
- **Two-phase LLM flow:** send a lightweight catalog of command summaries, then LLM uses tool calls to fetch full output for commands it deems relevant.
- **Relevance scoring via extensible Trait abstraction** (like the existing Probe trait), not hardcoded rules.
- **Scope includes recently ended sessions** (not just active ones).

## Architecture

### 1. Command Segmentation

Terminal I/O streams are segmented into individual commands by detecting shell prompt patterns in the output stream.

**Mechanism:** The daemon's `EventDetector` monitors the Output stream for shell prompt appearances. When a prompt is detected, everything between the previous prompt and the current one is packaged as a `CommandRecord`.

**Prompt detection strategy:** Pattern matching against common prompt endings (`$`, `#`, `%`, `❯`) with configurable patterns. Initial implementation can use heuristics; shell hook integration (preexec/precmd) is a future enhancement.

### 2. Data Model

```rust
struct CommandRecord {
    command_id: String,           // Unique ID: "{session_id}:{seq}"
    session_id: String,           // Owning session
    command_line: Option<String>, // User input text (extracted from Input stream)
    cwd: Option<String>,          // Working directory at execution time
    started_at: u64,              // Millisecond timestamp
    ended_at: Option<u64>,        // When next prompt appeared
    output_summary: String,       // First N + last N lines of output (for catalog)
    stream_offset: u64,           // Byte offset in stream.bin for full I/O
    stream_length: u64,           // Byte length of full I/O in stream.bin
}
```

Each command record is ~100-200 tokens in the catalog, making it feasible to send 50+ command summaries to the LLM in a single request.

Full I/O data stays in `stream.bin` and is fetched on-demand via `stream_offset` + `stream_length`.

### 3. Relevance Scoring — RelevanceSignal Trait

```rust
/// A signal that contributes to relevance scoring between a query context
/// and a candidate command.
trait RelevanceSignal: Send + Sync {
    /// Unique name for this signal (e.g., "cwd", "time_proximity").
    fn name(&self) -> &str;

    /// Compute a relevance score in [0.0, 1.0] for a candidate command
    /// relative to the query context.
    fn score(&self, query_ctx: &QueryContext, candidate: &CommandRecord) -> f64;
}
```

**Initial signal implementations:**

| Signal | Logic |
|--------|-------|
| `CwdSignal` | 1.0 if same cwd, 0.5 if same parent dir, 0.0 otherwise |
| `TimeProximitySignal` | Decays from 1.0 based on time distance (e.g., exponential decay) |
| `HostnameSignal` | 1.0 if same hostname, 0.0 otherwise |
| `CommandPatternSignal` | Score based on shared toolchain (e.g., both running cargo/npm/git) |

**Aggregation:** Total score = average (or weighted sum) of all signal scores. Commands above a threshold are included in the catalog sent to the LLM.

Adding a new relevance dimension = implement one new struct with the trait.

### 4. Query Flow

```
User types ":ask why did the build fail"
         │
         ▼
┌─────────────────────────────┐
│ 1. Build QueryContext       │  current session, cwd, recent commands
│    from current session     │
└─────────────┬───────────────┘
              │
              ▼
┌─────────────────────────────┐
│ 2. Score all commands       │  RelevanceSignal trait impls
│    (active + recent sessions)│  filter by threshold
└─────────────┬───────────────┘
              │
              ▼
┌─────────────────────────────┐
│ 3. Send to LLM:            │
│    - User query             │
│    - Current session context│
│    - Command catalog        │  command_id, command_line, cwd,
│      (scored candidates)    │  time, session_id, output_summary
│    - Tool definitions       │
└─────────────┬───────────────┘
              │
              ▼
┌─────────────────────────────┐
│ 4. LLM calls tool:         │
│    get_commands([ids])      │  batch fetch full I/O by command_id
└─────────────┬───────────────┘
              │
              ▼
┌─────────────────────────────┐
│ 5. LLM synthesizes answer   │
│    with full context        │
└─────────────────────────────┘
```

### 5. LLM Tool Definition

```rust
/// Tool exposed to the LLM for fetching command details.
/// Batch interface: accepts multiple command IDs in one call.
struct GetCommandsTool;

// Input:  { "command_ids": ["abc123:5", "def456:12"] }
// Output: [
//   { "command_id": "abc123:5", "session_id": "abc123",
//     "command_line": "cargo build", "cwd": "/home/user/project",
//     "full_output": "... complete terminal output ..." },
//   ...
// ]
```

### 6. Session Scope — Including Recently Ended Sessions

`SessionManager` changes:
- Active sessions: currently tracked in `HashMap<String, ActiveSession>`
- Add: scan session directories for sessions ended within a configurable window (default: 2 hours)
- `CommandRecord` storage must persist across session end (already the case since stream.bin survives)
- Command index (list of `CommandRecord` metadata) stored alongside `meta.json` per session

### 7. Storage Layout (Updated)

```
~/.local/share/omnish/sessions/
├── 2026-02-13T10-30-00_abc12345/
│   ├── meta.json         # SessionMeta (unchanged)
│   ├── stream.bin         # Raw I/O stream (unchanged)
│   └── commands.json      # Vec<CommandRecord> index
└── 2026-02-13T10-35-00_def67890/
    ├── meta.json
    ├── stream.bin
    └── commands.json
```

## Open Questions / Future Work

- **Shell prompt detection accuracy:** Heuristic-based detection will have edge cases (multi-line prompts, custom prompts). Shell hook integration (preexec/precmd) would be more reliable but requires user setup.
- **cwd tracking per command:** Currently cwd is captured once at session start via Probe. Per-command cwd tracking may require shell hooks or `/proc/{pid}/cwd` polling.
- **Output summary strategy:** How many lines to keep in head/tail summary. Configurable, default TBD.
- **Relevance threshold tuning:** What score threshold to use for including commands in the catalog. May need experimentation.
- **Context window budget:** How to balance current session context vs. catalog size vs. fetched command details within LLM token limits.
