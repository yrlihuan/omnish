# LLM Shell Completion Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add LLM-driven ghost-text completion for shell commands, triggered by input pause, accepted with Tab.

**Architecture:** Extend protocol with `CompletionRequest`/`CompletionResponse` messages. Client tracks shell input state via forwarded bytes and OSC 133 markers, debounces at 500ms, sends async completion requests to daemon. Daemon reuses existing context pipeline + LLM backend with a completion-specific prompt. Client renders top suggestion as ghost text, Tab writes it to PTY.

**Tech Stack:** Rust, bincode/serde (protocol), tokio (async), existing omnish-llm backends (Anthropic/OpenAI-compat).

---

### Task 1: Add Protocol Messages

**Files:**
- Modify: `crates/omnish-protocol/src/message.rs`

**Step 1: Add CompletionSuggestion, CompletionRequest, CompletionResponse structs and Message variants**

After the existing `CommandComplete` struct (line 91), add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionSuggestion {
    pub text: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub session_id: String,
    pub input: String,
    pub cursor_pos: usize,
    pub sequence_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub sequence_id: u64,
    pub suggestions: Vec<CompletionSuggestion>,
}
```

Add to the `Message` enum:

```rust
CompletionRequest(CompletionRequest),
CompletionResponse(CompletionResponse),
```

**Step 2: Write test for round-trip serialization**

Add to the existing `mod tests` block:

```rust
#[test]
fn test_frame_with_completion_request() {
    let frame = Frame {
        request_id: 10,
        payload: Message::CompletionRequest(CompletionRequest {
            session_id: "abc".to_string(),
            input: "git sta".to_string(),
            cursor_pos: 7,
            sequence_id: 42,
        }),
    };
    let bytes = frame.to_bytes().unwrap();
    let decoded = Frame::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.request_id, 10);
    if let Message::CompletionRequest(req) = decoded.payload {
        assert_eq!(req.input, "git sta");
        assert_eq!(req.sequence_id, 42);
    } else {
        panic!("expected CompletionRequest");
    }
}

#[test]
fn test_frame_with_completion_response() {
    let frame = Frame {
        request_id: 11,
        payload: Message::CompletionResponse(CompletionResponse {
            sequence_id: 42,
            suggestions: vec![
                CompletionSuggestion {
                    text: "tus".to_string(),
                    confidence: 0.95,
                },
            ],
        }),
    };
    let bytes = frame.to_bytes().unwrap();
    let decoded = Frame::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.request_id, 11);
    if let Message::CompletionResponse(resp) = decoded.payload {
        assert_eq!(resp.sequence_id, 42);
        assert_eq!(resp.suggestions.len(), 1);
        assert_eq!(resp.suggestions[0].text, "tus");
    } else {
        panic!("expected CompletionResponse");
    }
}
```

**Step 3: Run tests**

Run: `cargo test -p omnish-protocol`
Expected: All tests pass including the two new ones.

**Step 4: Commit**

```bash
git add crates/omnish-protocol/src/message.rs
git commit -m "feat(protocol): add CompletionRequest/CompletionResponse messages"
```

---

### Task 2: Add Completion Prompt Template

**Files:**
- Modify: `crates/omnish-llm/src/template.rs`

**Step 1: Add `build_completion_content` function**

```rust
/// Build the user-content prompt for shell command completion.
pub fn build_completion_content(context: &str, input: &str, cursor_pos: usize) -> String {
    format!(
        "Here is the terminal session context:\n\n\
         ```\n{}\n```\n\n\
         The user is typing a shell command. Current input: `{}`\n\
         Cursor position: {}\n\n\
         Suggest completions for this command. Reply with a JSON array:\n\
         [{{\"text\": \"<text after cursor>\", \"confidence\": <0.0-1.0>}}]\n\
         Return at most 3 suggestions sorted by confidence descending.\n\
         Return [] if no good completion exists.\n\
         Do not include any other text outside the JSON array.",
        context, input, cursor_pos
    )
}
```

**Step 2: Write test**

```rust
#[test]
fn test_build_completion_content() {
    let result = build_completion_content("$ ls\nfoo bar", "git sta", 7);
    assert!(result.contains("$ ls\nfoo bar"));
    assert!(result.contains("Current input: `git sta`"));
    assert!(result.contains("Cursor position: 7"));
    assert!(result.contains("JSON array"));
}
```

**Step 3: Run tests**

Run: `cargo test -p omnish-llm`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/omnish-llm/src/template.rs
git commit -m "feat(llm): add completion-specific prompt template"
```

---

### Task 3: Handle CompletionRequest in Daemon

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs`

**Step 1: Add `handle_completion_request` function**

After the existing `handle_llm_request` function (line 162), add:

```rust
async fn handle_completion_request(
    req: &omnish_protocol::message::CompletionRequest,
    mgr: &SessionManager,
    backend: &Arc<dyn LlmBackend>,
) -> Result<Vec<omnish_protocol::message::CompletionSuggestion>> {
    let context_req = Request {
        request_id: String::new(),
        session_id: req.session_id.clone(),
        query: String::new(),
        scope: RequestScope::AllSessions,
    };
    let context = resolve_context(&context_req, mgr).await?;

    let prompt = omnish_llm::template::build_completion_content(
        &context, &req.input, req.cursor_pos,
    );

    let llm_req = LlmRequest {
        context: String::new(),
        query: Some(prompt),
        trigger: TriggerType::Manual,
        session_ids: vec![req.session_id.clone()],
    };

    let response = backend.complete(&llm_req).await?;
    parse_completion_suggestions(&response.content)
}

fn parse_completion_suggestions(
    content: &str,
) -> Result<Vec<omnish_protocol::message::CompletionSuggestion>> {
    // Find JSON array in response (LLM may include surrounding text)
    let trimmed = content.trim();
    let start = trimmed.find('[').unwrap_or(0);
    let end = trimmed.rfind(']').map(|i| i + 1).unwrap_or(trimmed.len());
    let json_str = &trimmed[start..end];

    #[derive(serde::Deserialize)]
    struct RawSuggestion {
        text: String,
        confidence: f32,
    }

    let raw: Vec<RawSuggestion> = serde_json::from_str(json_str).unwrap_or_default();
    Ok(raw
        .into_iter()
        .map(|r| omnish_protocol::message::CompletionSuggestion {
            text: r.text,
            confidence: r.confidence.clamp(0.0, 1.0),
        })
        .collect())
}
```

**Step 2: Add CompletionRequest match arm in `handle_message`**

In the `match msg` block (before the `_ => Message::Ack` arm), add:

```rust
Message::CompletionRequest(req) => {
    if let Some(ref backend) = llm {
        match handle_completion_request(&req, mgr, backend).await {
            Ok(suggestions) => Message::CompletionResponse(
                omnish_protocol::message::CompletionResponse {
                    sequence_id: req.sequence_id,
                    suggestions,
                },
            ),
            Err(e) => {
                tracing::error!("Completion request failed: {}", e);
                Message::CompletionResponse(
                    omnish_protocol::message::CompletionResponse {
                        sequence_id: req.sequence_id,
                        suggestions: vec![],
                    },
                )
            }
        }
    } else {
        Message::CompletionResponse(
            omnish_protocol::message::CompletionResponse {
                sequence_id: req.sequence_id,
                suggestions: vec![],
            },
        )
    }
}
```

**Step 3: Write unit test for `parse_completion_suggestions`**

Add at the bottom of server.rs:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_completion_suggestions_valid() {
        let input = r#"[{"text": "tus", "confidence": 0.95}, {"text": "sh", "confidence": 0.7}]"#;
        let result = parse_completion_suggestions(input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "tus");
        assert!((result[0].confidence - 0.95).abs() < 0.01);
    }

    #[test]
    fn test_parse_completion_suggestions_with_surrounding_text() {
        let input = "Here are my suggestions:\n[{\"text\": \"tus\", \"confidence\": 0.9}]\nHope this helps!";
        let result = parse_completion_suggestions(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "tus");
    }

    #[test]
    fn test_parse_completion_suggestions_empty() {
        let result = parse_completion_suggestions("[]").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_completion_suggestions_invalid_json() {
        let result = parse_completion_suggestions("not json at all").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_completion_suggestions_clamps_confidence() {
        let input = r#"[{"text": "x", "confidence": 1.5}]"#;
        let result = parse_completion_suggestions(input).unwrap();
        assert!((result[0].confidence - 1.0).abs() < 0.01);
    }
}
```

**Step 4: Run tests**

Run: `cargo test -p omnish-daemon`
Expected: All tests pass.

**Step 5: Commit**

```bash
git add crates/omnish-daemon/src/server.rs
git commit -m "feat(daemon): handle CompletionRequest with LLM backend"
```

---

### Task 4: Add Shell Input Tracker to Client

**Files:**
- Create: `crates/omnish-client/src/shell_input.rs`
- Modify: `crates/omnish-client/src/main.rs` (add `mod shell_input;`)

This component tracks what the user is typing at the shell prompt by observing forwarded input bytes and OSC 133 markers.

**Step 1: Write tests first**

Create `crates/omnish-client/src/shell_input.rs`:

```rust
/// Tracks the current shell command-line input by observing forwarded bytes
/// and OSC 133 state transitions.
///
/// Lifecycle:
/// 1. OSC 133;A (PromptStart) or 133;D (CommandEnd) → at_prompt = true, clear input
/// 2. OSC 133;B (CommandStart) → at_prompt = false (user pressed Enter, command executing)
/// 3. While at_prompt, forwarded printable bytes are appended to `input`
/// 4. Backspace (0x7f / 0x08) removes the last character
/// 5. Ctrl+C (0x03) / Ctrl+U (0x15) clears input
/// 6. Enter (0x0d) clears input (command submitted)
pub struct ShellInputTracker {
    input: String,
    at_prompt: bool,
    /// Monotonically increasing sequence ID, bumped on every input change.
    sequence_id: u64,
    /// Whether input changed since last `take_change()`.
    changed: bool,
}

impl ShellInputTracker {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            at_prompt: true,  // assume we start at a prompt
            sequence_id: 0,
            changed: false,
        }
    }

    /// Call when OSC 133;A (PromptStart) or 133;D (CommandEnd) is detected.
    pub fn on_prompt(&mut self) {
        self.at_prompt = true;
        if !self.input.is_empty() {
            self.input.clear();
            self.bump();
        }
    }

    /// Call when OSC 133;B (CommandStart) is detected.
    pub fn on_command_start(&mut self) {
        self.at_prompt = false;
        self.input.clear();
        self.bump();
    }

    /// Feed bytes that were forwarded to the PTY (user's raw input).
    /// Only processes input while at the prompt.
    pub fn feed_forwarded(&mut self, bytes: &[u8]) {
        if !self.at_prompt {
            return;
        }
        for &b in bytes {
            match b {
                // Enter → command submitted, will be followed by CommandStart
                0x0d | 0x0a => {
                    self.input.clear();
                    self.bump();
                }
                // Ctrl+C → cancel current input
                0x03 => {
                    if !self.input.is_empty() {
                        self.input.clear();
                        self.bump();
                    }
                }
                // Ctrl+U → clear line
                0x15 => {
                    if !self.input.is_empty() {
                        self.input.clear();
                        self.bump();
                    }
                }
                // Backspace / DEL → remove last char
                0x7f | 0x08 => {
                    if self.input.pop().is_some() {
                        self.bump();
                    }
                }
                // Tab → don't append (it's a completion trigger)
                0x09 => {}
                // Printable ASCII
                0x20..=0x7e => {
                    self.input.push(b as char);
                    self.bump();
                }
                // Ignore control chars and escape sequences for now
                // (arrow keys, etc. would need full readline emulation)
                _ => {}
            }
        }
    }

    /// Append text to the input (e.g., after Tab acceptance writes to PTY).
    pub fn inject(&mut self, text: &str) {
        self.input.push_str(text);
        self.bump();
    }

    /// Current input text.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Current sequence ID.
    pub fn sequence_id(&self) -> u64 {
        self.sequence_id
    }

    /// Whether the user is at the prompt.
    pub fn at_prompt(&self) -> bool {
        self.at_prompt
    }

    /// Check if input changed since last call, and return current state.
    /// Returns `Some((input, sequence_id))` if changed, None otherwise.
    pub fn take_change(&mut self) -> Option<(&str, u64)> {
        if self.changed {
            self.changed = false;
            Some((&self.input, self.sequence_id))
        } else {
            None
        }
    }

    fn bump(&mut self) {
        self.sequence_id += 1;
        self.changed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_typing() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"ls -la");
        assert_eq!(t.input(), "ls -la");
        assert_eq!(t.sequence_id(), 6); // one bump per char
    }

    #[test]
    fn test_backspace() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"lss");
        t.feed_forwarded(&[0x7f]); // backspace
        assert_eq!(t.input(), "ls");
    }

    #[test]
    fn test_enter_clears() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"ls");
        t.feed_forwarded(&[0x0d]); // Enter
        assert_eq!(t.input(), "");
    }

    #[test]
    fn test_ctrl_c_clears() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"some cmd");
        t.feed_forwarded(&[0x03]); // Ctrl+C
        assert_eq!(t.input(), "");
    }

    #[test]
    fn test_ctrl_u_clears() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"some cmd");
        t.feed_forwarded(&[0x15]); // Ctrl+U
        assert_eq!(t.input(), "");
    }

    #[test]
    fn test_osc133_prompt_cycle() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"ls");
        assert_eq!(t.input(), "ls");

        // Command executes
        t.on_command_start();
        assert_eq!(t.input(), "");
        assert!(!t.at_prompt());

        // Back to prompt
        t.on_prompt();
        assert!(t.at_prompt());
        assert_eq!(t.input(), "");
    }

    #[test]
    fn test_ignores_input_during_command_execution() {
        let mut t = ShellInputTracker::new();
        t.on_command_start();
        t.feed_forwarded(b"output bytes");
        assert_eq!(t.input(), ""); // ignored
    }

    #[test]
    fn test_take_change() {
        let mut t = ShellInputTracker::new();
        assert!(t.take_change().is_none());

        t.feed_forwarded(b"g");
        let (input, seq) = t.take_change().unwrap();
        assert_eq!(input, "g");
        assert_eq!(seq, 1);

        // No change since last take
        assert!(t.take_change().is_none());

        t.feed_forwarded(b"it");
        let (input, seq) = t.take_change().unwrap();
        assert_eq!(input, "git");
        assert_eq!(seq, 3);
    }

    #[test]
    fn test_tab_not_appended() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"git\t");
        assert_eq!(t.input(), "git"); // Tab not in input
    }

    #[test]
    fn test_inject() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"git");
        t.inject(" status");
        assert_eq!(t.input(), "git status");
    }
}
```

**Step 2: Add module declaration**

In `crates/omnish-client/src/main.rs`, add at top with other mod declarations:

```rust
mod shell_input;
```

**Step 3: Run tests**

Run: `cargo test -p omnish-client -- shell_input`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add crates/omnish-client/src/shell_input.rs crates/omnish-client/src/main.rs
git commit -m "feat(client): add ShellInputTracker for shell command input observation"
```

---

### Task 5: Add Completion Debouncer

**Files:**
- Create: `crates/omnish-client/src/completion.rs`
- Modify: `crates/omnish-client/src/main.rs` (add `mod completion;`)

This component manages debounce timing, async request dispatch, and ghost text state for shell completion.

**Step 1: Create completion.rs with debounce + state management**

```rust
use std::time::Instant;
use omnish_protocol::message::{
    CompletionRequest, CompletionResponse, CompletionSuggestion, Message,
};
use omnish_transport::rpc_client::RpcClient;

const DEBOUNCE_MS: u64 = 500;
const MIN_INPUT_LEN: usize = 2;

pub struct ShellCompleter {
    /// Last time input changed.
    last_change: Option<Instant>,
    /// Sequence ID of the last input change.
    pending_seq: u64,
    /// Sequence ID of the last sent request.
    sent_seq: u64,
    /// Current ghost text suggestion (if any).
    current_ghost: Option<String>,
    /// Whether a request is currently in flight.
    in_flight: bool,
    /// The input that produced the current ghost.
    ghost_input: String,
}

impl ShellCompleter {
    pub fn new() -> Self {
        Self {
            last_change: None,
            pending_seq: 0,
            sent_seq: 0,
            current_ghost: None,
            in_flight: false,
            ghost_input: String::new(),
        }
    }

    /// Notify that input changed. Resets debounce timer and clears stale ghost.
    pub fn on_input_changed(&mut self, input: &str, sequence_id: u64) {
        self.pending_seq = sequence_id;
        self.last_change = Some(Instant::now());

        // If current ghost is still a prefix match, keep showing it truncated
        if let Some(ref ghost) = self.current_ghost {
            if input.starts_with(&self.ghost_input) {
                let extra_typed = input.len() - self.ghost_input.len();
                if extra_typed < ghost.len() {
                    // Ghost is still valid, just shorter
                    return;
                }
            }
        }
        // Otherwise clear ghost
        self.current_ghost = None;
    }

    /// Check if debounce timer has expired and we should send a request.
    /// Returns Some((input_to_send, sequence_id)) if ready.
    pub fn should_request(&self, current_input: &str) -> bool {
        if current_input.len() < MIN_INPUT_LEN {
            return false;
        }
        if self.in_flight {
            return false;
        }
        if self.sent_seq >= self.pending_seq {
            return false;
        }
        match self.last_change {
            Some(t) => t.elapsed().as_millis() >= DEBOUNCE_MS as u128,
            None => false,
        }
    }

    /// Mark that a request was sent.
    pub fn mark_sent(&mut self, sequence_id: u64) {
        self.sent_seq = sequence_id;
        self.in_flight = true;
    }

    /// Process a completion response.
    /// Returns the ghost text to display, if any.
    pub fn on_response(&mut self, response: &CompletionResponse, current_input: &str) -> Option<&str> {
        self.in_flight = false;

        // Discard stale response
        if response.sequence_id < self.pending_seq {
            return None;
        }

        // Take best suggestion
        if let Some(best) = response
            .suggestions
            .iter()
            .max_by(|a, b| a.confidence.partial_cmp(&b.confidence).unwrap_or(std::cmp::Ordering::Equal))
        {
            if !best.text.is_empty() {
                self.current_ghost = Some(best.text.clone());
                self.ghost_input = current_input.to_string();
                return self.current_ghost.as_deref();
            }
        }

        self.current_ghost = None;
        None
    }

    /// Accept the current ghost text. Returns text to inject into PTY.
    pub fn accept(&mut self) -> Option<String> {
        let ghost = self.current_ghost.take()?;
        self.ghost_input.clear();
        Some(ghost)
    }

    /// Clear ghost text (e.g., user pressed Esc, or entered :: mode).
    pub fn clear(&mut self) {
        self.current_ghost = None;
        self.ghost_input.clear();
    }

    /// Current ghost text suffix to display.
    pub fn ghost(&self) -> Option<&str> {
        self.current_ghost.as_deref()
    }

    /// Build a CompletionRequest message.
    pub fn build_request(
        session_id: &str,
        input: &str,
        sequence_id: u64,
    ) -> Message {
        Message::CompletionRequest(CompletionRequest {
            session_id: session_id.to_string(),
            input: input.to_string(),
            cursor_pos: input.len(), // cursor at end for v1
            sequence_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debounce_not_ready_immediately() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git", 1);
        assert!(!c.should_request("git"));
    }

    #[test]
    fn test_debounce_not_ready_short_input() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("g", 1);
        // Even after waiting, input too short
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(!c.should_request("g"));
    }

    #[test]
    fn test_debounce_ready_after_timeout() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        // Simulate time passing
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(c.should_request("git sta"));
    }

    #[test]
    fn test_no_duplicate_request() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1));
        c.mark_sent(5);
        assert!(!c.should_request("git sta")); // in flight
    }

    #[test]
    fn test_stale_response_discarded() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        c.mark_sent(5);
        // User types more before response arrives
        c.on_input_changed("git status", 10);

        let resp = CompletionResponse {
            sequence_id: 5, // stale
            suggestions: vec![CompletionSuggestion {
                text: "tus".to_string(),
                confidence: 0.9,
            }],
        };
        assert!(c.on_response(&resp, "git status").is_none());
    }

    #[test]
    fn test_valid_response_sets_ghost() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        c.mark_sent(5);

        let resp = CompletionResponse {
            sequence_id: 5,
            suggestions: vec![CompletionSuggestion {
                text: "tus".to_string(),
                confidence: 0.9,
            }],
        };
        let ghost = c.on_response(&resp, "git sta");
        assert_eq!(ghost, Some("tus"));
        assert_eq!(c.ghost(), Some("tus"));
    }

    #[test]
    fn test_accept_returns_and_clears() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        c.mark_sent(5);

        let resp = CompletionResponse {
            sequence_id: 5,
            suggestions: vec![CompletionSuggestion {
                text: "tus".to_string(),
                confidence: 0.9,
            }],
        };
        c.on_response(&resp, "git sta");

        assert_eq!(c.accept(), Some("tus".to_string()));
        assert!(c.ghost().is_none());
        assert!(c.accept().is_none());
    }

    #[test]
    fn test_clear_removes_ghost() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        c.mark_sent(5);

        let resp = CompletionResponse {
            sequence_id: 5,
            suggestions: vec![CompletionSuggestion {
                text: "tus".to_string(),
                confidence: 0.9,
            }],
        };
        c.on_response(&resp, "git sta");
        c.clear();
        assert!(c.ghost().is_none());
    }

    #[test]
    fn test_best_suggestion_selected() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git", 3);
        c.mark_sent(3);

        let resp = CompletionResponse {
            sequence_id: 3,
            suggestions: vec![
                CompletionSuggestion { text: " status".to_string(), confidence: 0.7 },
                CompletionSuggestion { text: " stash".to_string(), confidence: 0.9 },
                CompletionSuggestion { text: " stage".to_string(), confidence: 0.5 },
            ],
        };
        let ghost = c.on_response(&resp, "git");
        assert_eq!(ghost, Some(" stash")); // highest confidence
    }

    #[test]
    fn test_ghost_survives_prefix_typing() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git", 3);
        c.mark_sent(3);

        let resp = CompletionResponse {
            sequence_id: 3,
            suggestions: vec![CompletionSuggestion {
                text: " status".to_string(),
                confidence: 0.9,
            }],
        };
        c.on_response(&resp, "git");
        assert_eq!(c.ghost(), Some(" status"));

        // User types more — "git " matches "git" + ghost " status"
        c.on_input_changed("git ", 4);
        // Ghost should still be present (shortened conceptually)
        assert_eq!(c.ghost(), Some(" status"));
    }
}
```

**Step 2: Add module declaration**

In `crates/omnish-client/src/main.rs`, add:

```rust
mod completion;
```

**Step 3: Run tests**

Run: `cargo test -p omnish-client -- completion`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add crates/omnish-client/src/completion.rs crates/omnish-client/src/main.rs
git commit -m "feat(client): add ShellCompleter with debounce and ghost text state"
```

---

### Task 6: Integrate Shell Completion into Client Main Loop

**Files:**
- Modify: `crates/omnish-client/src/main.rs`
- Modify: `crates/omnish-client/src/display.rs` (add `render_shell_ghost` if needed)

This is the integration task — wire ShellInputTracker + ShellCompleter into the main poll loop.

**Step 1: Add state variables in main()**

After the `completer` initialization (line 108-110), add:

```rust
let mut shell_input = shell_input::ShellInputTracker::new();
let mut shell_completer = completion::ShellCompleter::new();
```

**Step 2: Feed forwarded bytes to ShellInputTracker**

In the `InterceptAction::Forward(bytes)` arm (around line 184-200), after `proxy.write_all(&bytes)?;`, add:

```rust
// Track shell input for LLM completion
shell_input.feed_forwarded(&bytes);
if let Some((input, seq)) = shell_input.take_change() {
    shell_completer.on_input_changed(input, seq);
    // Clear ghost text display if input changed
    if shell_completer.ghost().is_none() {
        // TODO: clear ghost from terminal if it was visible
    }
}
```

**Step 3: Feed OSC 133 events to ShellInputTracker**

In the PTY output section (around line 324-328), inside the `for event in osc_events` loop, add:

```rust
use omnish_tracker::osc133_detector::Osc133EventKind;
match event.kind {
    Osc133EventKind::PromptStart | Osc133EventKind::CommandEnd { .. } => {
        shell_input.on_prompt();
        shell_completer.clear();
    }
    Osc133EventKind::CommandStart => {
        shell_input.on_command_start();
        shell_completer.clear();
    }
    _ => {}
}
```

**Step 4: Check debounce timer and send completion request**

At the end of the main loop (after the PTY output section, before the POLLHUP check), add:

```rust
// Check if we should send a completion request
if !interceptor_in_chat && shell_input.at_prompt() {
    let current = shell_input.input();
    if shell_completer.should_request(current) {
        let seq = shell_input.sequence_id();
        if let Some(ref rpc) = daemon_conn {
            let msg = completion::ShellCompleter::build_request(
                &session_id, current, seq,
            );
            shell_completer.mark_sent(seq);
            // Send async, don't await response here
            send_or_buffer(rpc, msg, &pending_buffer).await;
        }
    }
}
```

Note: The completion response arrives as a `Message::CompletionResponse` via the RPC client. However, the current `rpc.call()` model is request-response, so the completion request will block until a response arrives. For the MVP, we'll use `rpc.call()` in a tokio::spawn to avoid blocking the main loop:

```rust
// In the debounce check section:
if shell_completer.should_request(current) {
    let seq = shell_input.sequence_id();
    if let Some(ref rpc) = daemon_conn {
        let msg = completion::ShellCompleter::build_request(
            &session_id, current, seq,
        );
        shell_completer.mark_sent(seq);
        let rpc_clone = rpc.clone();
        let completion_tx = completion_tx.clone();
        tokio::spawn(async move {
            if let Ok(Message::CompletionResponse(resp)) = rpc_clone.call(msg).await {
                let _ = completion_tx.send(resp).await;
            }
        });
    }
}
```

Add a `tokio::sync::mpsc` channel for completion responses near the top of main():

```rust
let (completion_tx, mut completion_rx) = tokio::sync::mpsc::channel::<
    omnish_protocol::message::CompletionResponse
>(4);
```

**Step 5: Receive completion responses in poll loop**

After the `fds` poll and before POLLHUP check, add:

```rust
// Check for completion responses (non-blocking)
while let Ok(resp) = completion_rx.try_recv() {
    let current = shell_input.input();
    if let Some(ghost) = shell_completer.on_response(&resp, current) {
        // Render ghost text after current cursor position
        let ghost_render = display::render_ghost_text(ghost);
        nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
    }
}
```

**Step 6: Handle Tab for shell completion acceptance**

In the `InterceptAction::Forward` arm, intercept Tab when ghost is visible. This requires checking *before* forwarding. Modify the Forward arm:

For the normal (non-interceptor) Tab handling, we need to check in the raw forwarded bytes. Since the interceptor only handles Tab in chat mode, in normal mode Tab is forwarded. We need to intercept Tab in the forward path when shell ghost is active.

Add a check before `proxy.write_all(&bytes)?` in the Forward arm:

```rust
InterceptAction::Forward(bytes) => {
    // Check if Tab should be intercepted for shell completion
    if bytes == [b'\t'] && shell_completer.ghost().is_some() {
        if let Some(suffix) = shell_completer.accept() {
            // Write the completion text to PTY (as if user typed it)
            proxy.write_all(suffix.as_bytes())?;
            shell_input.inject(&suffix);
        }
    } else {
        // Normal forward to PTY
        proxy.write_all(&bytes)?;

        // Track shell input for LLM completion
        shell_input.feed_forwarded(&bytes);
        if let Some((input, seq)) = shell_input.take_change() {
            shell_completer.on_input_changed(input, seq);
        }
    }

    // (keep existing code for command_tracker.feed_input, IoData send, etc.)
}
```

**Step 7: Clear shell ghost on entering :: chat mode**

In the `InterceptAction::Buffering` arm where the prompt is first drawn (the `if buf == prefix_bytes` check), add:

```rust
shell_completer.clear();
```

**Step 8: Build and manual test**

Run: `cargo build`
Expected: Compiles without errors.

Run: `cargo test -p omnish-client`
Expected: All existing tests still pass.

**Step 9: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat(client): integrate LLM shell completion into main loop"
```

---

### Task 7: End-to-End Manual Testing

**Steps:**
1. Start daemon: `cargo run -p omnish-daemon`
2. Start client: `cargo run -p omnish-client`
3. Type `git sta` and wait 500ms — should see ghost text suggestion appear
4. Press Tab — suggestion should be accepted and sent to shell
5. Type a short input (1 char) and wait — no ghost should appear
6. Enter vim (`vi`) — ghost completion should be disabled
7. Exit vim — ghost completion should re-enable
8. Type `::why` — should enter chat mode, not trigger shell completion
9. Test with daemon disconnected — should work normally without errors

**Step 10: Final commit with any fixes**

```bash
git add -A
git commit -m "feat: LLM-driven shell command completion (MVP)"
```
