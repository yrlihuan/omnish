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

        // If current ghost is still a prefix match, keep showing it
        if let Some(ref ghost) = self.current_ghost {
            if input.starts_with(&self.ghost_input) {
                let extra_typed = input.len() - self.ghost_input.len();
                if extra_typed < ghost.len() {
                    return;
                }
            }
        }
        // Otherwise clear ghost
        self.current_ghost = None;
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

    /// Clear ghost text.
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

        c.on_input_changed("git ", 4);
        assert_eq!(c.ghost(), Some(" status"));
    }
}
