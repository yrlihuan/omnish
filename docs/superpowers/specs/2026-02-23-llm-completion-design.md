# LLM-Driven Shell Command Completion

## Overview

Add intelligent command-line completion powered by LLM. When the user pauses
while typing a shell command, omnish sends the current input + session context
to the daemon's LLM backend and displays the suggestion as ghost text. The user
presses Tab to accept.

## Trigger

- User is at the shell prompt (detected via OSC 133 markers).
- Input pauses for **500ms** (debounce).
- Minimum **2 characters** typed before triggering.
- Disabled during: alt-screen programs (vim, htop), `::` chat mode, suppressed
  state, or when daemon is disconnected.

## Protocol Messages

```rust
// Client → Daemon
CompletionRequest {
    session_id: String,
    input: String,        // current command-line text
    cursor_pos: usize,    // cursor position in input
    sequence_id: u64,     // monotonically increasing; older responses are discarded
}

// Daemon → Client
CompletionResponse {
    sequence_id: u64,
    suggestions: Vec<CompletionSuggestion>,
}

CompletionSuggestion {
    text: String,         // completion text (part after cursor)
    confidence: f32,      // 0.0 - 1.0
}
```

## Daemon Side

1. Receive `CompletionRequest`.
2. Call `resolve_context()` — same context pipeline as chat (last 10 commands
   with output, grouped by session).
3. Build a completion-specific prompt:
   ```
   Here is the terminal session context:
   ```
   {context}
   ```

   The user is typing a shell command. Current input: `{input}`
   Cursor position: {cursor_pos}

   Suggest completions for this command. Reply with a JSON array:
   [{"text": "<text after cursor>", "confidence": <0.0-1.0>}]
   Return at most 3 suggestions sorted by confidence descending.
   Return [] if no good completion exists.
   ```
4. Call `LlmBackend::complete()`, parse JSON response.
5. Return `CompletionResponse`.

## Client Side

### Input Tracking

Extend the main loop to track the current command-line input:

- When OSC 133;B fires (command start after prompt), clear the input buffer.
- Track printable characters, backspace, and cursor movements to maintain a
  `current_input: String` reflecting what the user has typed so far.
- The `CursorColTracker` already exists and can help determine cursor position.

### Debounce + Cancel

- On each input change: increment `sequence_id`, reset 500ms timer.
- When timer fires: send `CompletionRequest` with current `sequence_id`.
- On response: if `response.sequence_id < current_sequence_id`, discard.

### Rendering

- Take the highest-confidence suggestion from the response.
- Render as ghost text using existing `render_ghost_text()` infrastructure.
- Ghost text appears dimmed after the cursor position.

### Acceptance

- **Tab**: If ghost text is visible, write the suggestion text to PTY master
  (sends characters to shell). Clear ghost text. If no ghost text, forward Tab
  to shell (preserving native shell completion).
- **Any other key**: Clear ghost text, process key normally.
- **Esc**: Clear ghost text.

## Edge Cases

- **Shell native completion conflict**: Tab is intercepted only when ghost text
  is visible. Otherwise Tab is forwarded to the shell as normal.
- **Stale responses**: `sequence_id` comparison ensures only the latest response
  is rendered.
- **Daemon unavailable**: Completion silently disabled; no error shown.
- **Empty/short input**: No request sent for input < 2 chars.
- **Fast typing**: Debounce ensures at most one request per 500ms pause.

## MVP Scope (v1)

- Only display the top suggestion (highest confidence).
- Multi-suggestion selection UI deferred to v2.
- No client-side caching (simple first).
- Debounce timer is hardcoded at 500ms (configurable later).

## Future Enhancements

- Tab-cycle through multiple suggestions.
- Client-side prefix caching (if input extends a previous suggestion, reuse it).
- Configurable debounce interval.
- Streaming completion for lower perceived latency.
