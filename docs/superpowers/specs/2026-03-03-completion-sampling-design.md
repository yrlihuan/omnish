# Completion Sampling Design (Issue #101)

## Goal

Sample completion requests alongside the command the user subsequently executes, for offline analysis to iterate on LLM prompts and model parameters.

## What Gets Sampled

Each sample record contains the full context needed to reproduce and evaluate a completion interaction:

- **context**: the full LLM context (terminal session history) sent with the request
- **prompt**: the complete prompt sent to the LLM (template + context + input)
- **suggestions**: the LLM's completion suggestions (array of strings)
- **input**: the user's partial input at request time
- **accepted**: whether the user accepted a suggestion (Tab)
- **next_command**: the command the user actually executed afterward
- **similarity**: edit distance ratio between best suggestion and next_command
- **cwd**, **latency_ms**, **session_id**, **recorded_at**

## Sampling Criteria

Not every completion is sampled. A record is written when ALL of:

1. The completion was **ignored** (not accepted via Tab)
2. The next executed command has **edit distance similarity > 0.3** with the best suggestion (near miss — the LLM was close but not close enough)
3. The **global rate limit** has not been exceeded (at most 1 sample per 5 minutes across all sessions)

This captures the most valuable signal: cases where the LLM's suggestion was in the right direction but the user preferred to type manually.

## Storage

- Format: JSONL (one JSON object per line)
- Location: `~/.omnish/logs/samples/YYYY-MM-DD.jsonl`
- Daily rotation (same pattern as completion CSV)
- Async writer thread via mpsc channel (same pattern as `completion_writer`)

## Architecture

### Data Flow

```
CompletionRequest
  → handle_completion_request() builds context + prompt, gets suggestions
  → stores PendingSample { context, prompt, suggestions, input, accepted: false } in Session

CompletionSummary
  → updates pending sample's accepted field

CommandComplete
  → takes pending sample from Session
  → computes edit distance similarity between best suggestion and executed command
  → if criteria met (ignored + similarity > 0.3 + rate limit ok):
      → sends CompletionSample to writer via mpsc
      → writer appends to JSONL file

SessionEnd
  → flushes pending sample (if any) without next_command field
```

### Per-Session State

`Session` struct gets:
```rust
pending_sample: Mutex<Option<PendingSample>>
```

Only the most recent completion per session is buffered. Older pending samples are dropped when a new CompletionRequest arrives (we only care about the completion immediately before the command).

### SessionManager State

```rust
sample_writer: mpsc::Sender<CompletionSample>  // async JSONL writer
last_sample_time: Mutex<Option<Instant>>        // global rate limit
```

## Edit Distance

Levenshtein edit distance ratio:
```
similarity = 1.0 - (levenshtein(suggestion, command) / max(len(suggestion), len(command)))
```

Implemented inline (no external crate) — the strings are short (shell commands).

## Files to Modify

1. **`omnish-store/src/sample.rs`** (new): `CompletionSample` struct, `PendingSample` struct, `spawn_sample_writer()`, edit distance helper
2. **`omnish-store/src/lib.rs`**: export `sample` module
3. **`omnish-daemon/src/session_mgr.rs`**: add `pending_sample` to `Session`, `sample_writer` + `last_sample_time` to `SessionManager`, sampling logic in `receive_command()` and `end_session()`
4. **`omnish-daemon/src/server.rs`**: in `handle_completion_request()`, store `PendingSample` in session after LLM response
