# `/resume ?` semantic thread search (issue #651)

## Goal

Add `/resume ?` to the chat client. It prompts the user for a free-text
description, then uses the LLM to search across all conversation threads
(all hosts) and returns every relevant thread, sorted by confidence, in the
existing resume picker with a per-entry relevance reason.

## Current behavior (baseline)

`handle_resume` in `crates/omnish-client/src/chat_session.rs`:

- `/resume` -> picker of recent threads (current host)
- `/resume all` -> picker across all hosts
- `/resume N` -> resume the Nth cached thread

Thread data available on the daemon (`ConversationManager`,
`crates/omnish-daemon/src/conversation_mgr.rs`):

- `ThreadMeta.summary` (auto-generated title)
- `list_conversations()` -> `(thread_id, modified, exchange_count, last_question)`
- `get_all_exchanges(thread_id)` -> `Vec<(user, assistant)>`

The picker (`widgets::picker::pick_one_with_disabled`) is a blocking stdin
loop in raw mode. `LineEditor` (`widgets/line_editor.rs`) manages line-edit
state but has no standalone blocking driver yet.

## User-facing flow

1. User types `/resume ?` (optionally `/resume ? <description>` inline).
2. If no inline description, the client shows a blocking text prompt
   ("Describe the conversation to find:"), driven by `LineEditor`. ESC or an
   empty submission cancels (returns to the chat prompt, no request sent).
3. The client sends the description to the daemon and receives a ranked list
   of matching threads with reasons.
4. The client shows all matches in the resume picker, in confidence order,
   each annotated with its short relevance reason. Empty result -> a
   "no matching conversation" message, no picker.
5. Selecting an entry resumes it via the existing lock-aware
   `handle_resume_tid` path.

## Components

### 1. Client: description prompt helper

New blocking helper in the widgets layer, e.g.
`widgets::line_prompt::prompt_line(title: &str) -> Option<String>`:

- Renders `title`, then drives `LineEditor` from a `nix::unistd::read` stdin
  loop (mirroring `picker`'s loop): printable bytes and editing keys feed the
  editor; Enter returns the trimmed content; ESC / empty returns `None`.
- Returns `None` on cancel or empty input.

Rationale: keeps interactive input consistent with the picker's raw-mode
model instead of introducing async stdin.

### 2. Client: `handle_resume` branch

In `handle_resume`, detect `rest == "?"` or `rest` starting with `? `:

- If a description follows `?` inline, use it directly; otherwise call
  `prompt_line`.
- Cancel/empty -> return `false` (no-op), same as an aborted picker.
- Otherwise call a new `show_resume_search_picker(description, session_id, rpc)`.

`show_resume_search_picker`:

- Sends `Message::Request` with query `__cmd:conversations search <description>`,
  scope `AllSessions` (same request shape as `fetch_thread_ids`).
- Parses the response JSON: `thread_ids` (ordered), `reasons` (parallel array),
  and reuses `locked_threads` for disabled icons.
- Builds picker item strings from the daemon `display` lines, appending each
  `reason`. Populates `self.cached_thread_ids` with the returned ordered IDs so
  selection maps back correctly.
- Runs `pick_one_with_disabled`; selection resumes via `handle_resume_tid`.

### 3. Daemon: `conversations search` subcommand

Extend the `__cmd:conversations` handler in `crates/omnish-daemon/src/server.rs`
to recognize a `search <description>` form.

Handler steps:

1. Enumerate all threads across all hosts (same source as the existing
   all-hosts `conversations` listing).
2. For each thread, load `get_all_exchanges` and collect the user questions
   (the user side of each exchange). Pair with `thread_id` and `summary`.
3. Build a single LLM request (`UseCase::Analysis`):
   - System/prompt instructs: given the user's description and a numbered list
     of threads (id + summary + that thread's user questions), return a JSON
     array of every relevant thread as
     `[{"thread_id": "...", "confidence": 0.0-1.0, "reason": "<short>"}]`.
     Omit irrelevant threads. Return `[]` if none match.
4. Parse the JSON robustly (reuse the `extract_first_json_array` approach from
   the completion parser). Drop entries whose `thread_id` is unknown or whose
   `confidence` is out of range.
5. Sort by `confidence` descending.
6. Return `{thread_ids: [...], reasons: [...], locked_threads: [...], display: ...}`
   in the same envelope the existing `conversations` command produces, so the
   client picker rendering path is reused.

### 4. LLM prompt template

Add a template in `crates/omnish-llm/src/template.rs` (English base, with
`append_language_instruction` support) describing the ranking task and the
strict JSON output contract above.

## Data flow

```
/resume ?  ->  prompt_line (client)  ->  description
   -> __cmd:conversations search <description>  (client -> daemon)
   -> gather user questions per thread  ->  LLM ranking call
   -> parse + sort by confidence  ->  {thread_ids, reasons, locked_threads, display}
   -> picker with reasons (client)  ->  handle_resume_tid
```

## Error handling

- Empty/cancelled prompt: no request, return to chat prompt.
- LLM error or unparseable output: treat as empty result -> "no matching
  conversation" message (mirrors `parse_completion_suggestions` returning
  `Vec::new()` on malformed output).
- Unknown `thread_id` in LLM output: skipped.
- No threads exist: existing "no conversations to resume" message.

## Testing

- Daemon: unit-test the search-response parser/sorter with a mocked LLM
  backend (from_single): relevant subset returned, confidence sorting,
  malformed JSON -> empty, unknown thread_id dropped.
- Client: unit-test `prompt_line` byte handling (printable, backspace, Enter,
  ESC) following the `line_editor` test style; test the `handle_resume`
  argument parsing for `?` and `? <inline>`.
- Existing `/resume`, `/resume all`, `/resume N` tests must remain green.

## Scope / YAGNI

- No keyword prefilter or embeddings; single LLM call over all threads (user
  chose precision over cost).
- No fixed result cap; the LLM decides relevance, daemon sorts by confidence.
- No changes to `/resume`, `/resume all`, `/resume N`, or the `::` shortcut.
