use std::time::Instant;
use omnish_protocol::message::{
    CompletionRequest, CompletionResponse, Message,
};

const DEBOUNCE_MS: u64 = 500;

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
    /// When the current ghost text was set.
    ghost_set_at: Option<Instant>,
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
            ghost_set_at: None,
        }
    }

    /// Notify that input changed. Resets debounce timer and clears stale ghost.
    /// Returns `true` if ghost text was cleared (caller should erase it from screen).
    pub fn on_input_changed(&mut self, input: &str, sequence_id: u64) -> bool {
        self.pending_seq = sequence_id;
        self.last_change = Some(Instant::now());

        // If current ghost is still a prefix match, keep showing it
        if let Some(ref ghost) = self.current_ghost {
            if input.starts_with(&self.ghost_input) {
                let extra_typed = input.len() - self.ghost_input.len();
                // Verify the extra characters actually match the ghost text
                if extra_typed < ghost.len()
                    && ghost[..extra_typed] == input[self.ghost_input.len()..]
                {
                    // Trim consumed portion so accept() returns only the remaining suffix
                    self.current_ghost = Some(ghost[extra_typed..].to_string());
                    self.ghost_input = input.to_string();
                    return false;
                }
            }
        }
        // Otherwise clear ghost
        let had_ghost = self.current_ghost.is_some();
        self.current_ghost = None;
        had_ghost
    }

    /// Check if debounce timer has expired and we should send a request.
    pub fn should_request(&self, _current_input: &str) -> bool {
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
                // Strip the already-typed input prefix so ghost is only the suffix
                // If the suggestion doesn't start with current input, we need to determine
                // whether it's a suffix (e.g., "tus", " stash") or a full command that doesn't match.
                let suffix = if best.text.starts_with(current_input) {
                    &best.text[current_input.len()..]
                } else {
                    // Suggestion doesn't start with current input
                    // It could be:
                    // 1. A suffix (e.g., "tus" for "git sta", " stash" for "git")
                    // 2. A full command that doesn't match (e.g., "git status" for "ls")
                    //
                    // Heuristic: if suggestion is shorter or equal length, or starts with space,
                    // treat it as a suffix. Otherwise discard.
                    if best.text.len() <= current_input.len() || best.text.starts_with(' ') {
                        &best.text
                    } else {
                        // Likely a full command that doesn't match current input - discard
                        self.current_ghost = None;
                        return None;
                    }
                };
                if suffix.is_empty() {
                    self.current_ghost = None;
                    return None;
                }
                self.current_ghost = Some(suffix.to_string());
                self.ghost_input = current_input.to_string();
                self.ghost_set_at = Some(Instant::now());
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
        self.ghost_set_at = None;
        Some(ghost)
    }

    /// Clear ghost text.
    pub fn clear(&mut self) {
        self.current_ghost = None;
        self.ghost_input.clear();
        self.ghost_set_at = None;
    }

    /// Check if the current ghost text has expired.
    pub fn is_ghost_expired(&self, timeout_ms: u64) -> bool {
        match (self.current_ghost.as_ref(), self.ghost_set_at) {
            (Some(_), Some(t)) => t.elapsed().as_millis() >= timeout_ms as u128,
            _ => false,
        }
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
            cursor_pos: input.len(),
            sequence_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnish_protocol::message::CompletionSuggestion;

    #[test]
    fn test_debounce_not_ready_immediately() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git", 1);
        assert!(!c.should_request("git"));
    }

    #[test]
    fn test_debounce_ready_short_input() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("g", 1);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(c.should_request("g"));
    }

    #[test]
    fn test_debounce_ready_empty_input() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("", 1);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(c.should_request(""));
    }

    #[test]
    fn test_debounce_ready_after_timeout() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(c.should_request("git sta"));
    }

    #[test]
    fn test_no_duplicate_request() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1));
        c.mark_sent(5);
        assert!(!c.should_request("git sta"));
    }

    #[test]
    fn test_stale_response_discarded() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        c.mark_sent(5);
        c.on_input_changed("git status", 10);

        let resp = CompletionResponse {
            sequence_id: 5,
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
        assert_eq!(ghost, Some(" stash"));
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

        // User types "git " — ghost trimmed to "status" (consumed " ")
        assert!(!c.on_input_changed("git ", 4));
        assert_eq!(c.ghost(), Some("status"));

        // User types "git s" — ghost trimmed to "tatus"
        assert!(!c.on_input_changed("git s", 5));
        assert_eq!(c.ghost(), Some("tatus"));
    }

    #[test]
    fn test_on_input_changed_returns_true_when_ghost_cleared() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("cargo", 1);
        c.mark_sent(1);

        let resp = CompletionResponse {
            sequence_id: 1,
            suggestions: vec![CompletionSuggestion {
                text: " run".to_string(),
                confidence: 0.9,
            }],
        };
        c.on_response(&resp, "cargo");
        assert_eq!(c.ghost(), Some(" run"));

        // Typing something that diverges from the ghost should clear it
        assert!(c.on_input_changed("cargo test", 2));
        assert!(c.ghost().is_none());
    }

    #[test]
    fn test_on_input_changed_returns_false_when_no_ghost() {
        let mut c = ShellCompleter::new();
        // No ghost set — clearing nothing should return false
        assert!(!c.on_input_changed("ls", 1));
    }

    /// Regression: LLM returns full command "cargo run" but user already typed
    /// "cargo" — ghost should be " run", not "cargo run".
    #[test]
    fn test_response_strips_input_prefix() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("cargo", 1);
        c.mark_sent(1);

        let resp = CompletionResponse {
            sequence_id: 1,
            suggestions: vec![CompletionSuggestion {
                text: "cargo run".to_string(),
                confidence: 0.9,
            }],
        };
        let ghost = c.on_response(&resp, "cargo");
        assert_eq!(ghost, Some(" run"));
        assert_eq!(c.ghost(), Some(" run"));
    }

    /// When LLM returns exactly what user typed, ghost should be None.
    #[test]
    fn test_response_exact_match_no_ghost() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("ls", 1);
        c.mark_sent(1);

        let resp = CompletionResponse {
            sequence_id: 1,
            suggestions: vec![CompletionSuggestion {
                text: "ls".to_string(),
                confidence: 0.9,
            }],
        };
        assert!(c.on_response(&resp, "ls").is_none());
        assert!(c.ghost().is_none());
    }

    /// Regression: after typing matching prefix, Tab should inject only the
    /// remaining suffix, not the full original ghost.
    #[test]
    fn test_accept_after_typing_returns_suffix_only() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("", 1);
        c.mark_sent(1);

        let resp = CompletionResponse {
            sequence_id: 1,
            suggestions: vec![CompletionSuggestion {
                text: "cargo run".to_string(),
                confidence: 0.9,
            }],
        };
        c.on_response(&resp, "");
        assert_eq!(c.ghost(), Some("cargo run"));

        // User types "cargo" — ghost trimmed to " run"
        for (i, ch) in "cargo".chars().enumerate() {
            let typed: String = "cargo"[..=i].to_string();
            assert!(!c.on_input_changed(&typed, 2 + i as u64));
        }
        assert_eq!(c.ghost(), Some(" run"));

        // Tab accept should return only " run"
        assert_eq!(c.accept(), Some(" run".to_string()));
    }

    #[test]
    fn test_ghost_expired_after_timeout() {
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

        // Not expired immediately
        assert!(!c.is_ghost_expired(10_000));

        // Simulate expiry by backdating ghost_set_at
        c.ghost_set_at = Some(Instant::now() - std::time::Duration::from_secs(11));
        assert!(c.is_ghost_expired(10_000));

        // Clear resets expiry
        c.clear();
        assert!(!c.is_ghost_expired(10_000));
    }

    #[test]
    fn test_ghost_not_expired_without_ghost() {
        let c = ShellCompleter::new();
        assert!(!c.is_ghost_expired(10_000));
    }

    /// Regression: ghost " run" must be cleared when user types "cargo t"
    /// (diverges at the first extra character after the original input).
    #[test]
    fn test_ghost_cleared_on_divergent_extra_chars() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("cargo", 1);
        c.mark_sent(1);

        let resp = CompletionResponse {
            sequence_id: 1,
            suggestions: vec![CompletionSuggestion {
                text: " run".to_string(),
                confidence: 0.9,
            }],
        };
        c.on_response(&resp, "cargo");
        assert_eq!(c.ghost(), Some(" run"));

        // User types "cargo " — matches ghost[0..1] = " ", trimmed to "run"
        assert!(!c.on_input_changed("cargo ", 2));
        assert_eq!(c.ghost(), Some("run"));

        // User types "cargo t" — "t" != ghost[0..1] = "r", must clear
        assert!(c.on_input_changed("cargo t", 3));
        assert!(c.ghost().is_none());
    }

    /// Test for issue 6: when current input is not a prefix of the full suggestion,
    /// the completion should be discarded.
    #[test]
    fn test_completion_discarded_when_input_not_prefix() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git ", 1);
        c.mark_sent(1);

        // LLM returns full command "git status", but user has changed input to "ls"
        let resp = CompletionResponse {
            sequence_id: 1,
            suggestions: vec![CompletionSuggestion {
                text: "git status".to_string(),
                confidence: 0.9,
            }],
        };
        // Current input is "ls", which is not a prefix of "git status"
        let ghost = c.on_response(&resp, "ls");
        // According to issue 6, should discard completion (return None)
        // Current implementation returns the full "git status" as ghost (incorrect)
        assert_eq!(ghost, None, "Completion should be discarded when current input is not a prefix of full suggestion");
        assert_eq!(c.ghost(), None);
    }
}
