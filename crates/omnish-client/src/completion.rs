use std::time::Instant;
use std::collections::HashMap;
use omnish_protocol::message::{
    CompletionRequest, CompletionResponse, Message,
};

const DEBOUNCE_MS: u64 = 500;
/// Timeout for in-flight completion requests (5 seconds)
const IN_FLIGHT_TIMEOUT_MS: u64 = 5000;
/// Maximum number of concurrent requests allowed
const MAX_CONCURRENT_REQUESTS: usize = 5;

/// State of an active completion request
#[derive(Debug, Clone)]
struct RequestState {
    input: String,
    sent_at: Instant,
    sequence_id: u64,
}

pub struct ShellCompleter {
    /// Last time input changed.
    last_change: Option<Instant>,
    /// Sequence ID of the last input change.
    pending_seq: u64,
    /// Sequence ID of the last sent request.
    sent_seq: u64,
    /// Current ghost text suggestion (if any).
    current_ghost: Option<String>,
    /// Active completion requests (sequence_id -> request state)
    active_requests: HashMap<u64, RequestState>,
    /// The input that produced the current ghost.
    ghost_input: String,
    /// When the current ghost text was set.
    ghost_set_at: Option<Instant>,
    /// The input that was sent with the last request.
    sent_input: String,
}

impl ShellCompleter {
    pub fn new() -> Self {
        Self {
            last_change: None,
            pending_seq: 0,
            sent_seq: 0,
            current_ghost: None,
            active_requests: HashMap::new(),
            ghost_input: String::new(),
            ghost_set_at: None,
            sent_input: String::new(),
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
    ///
    /// Logic:
    /// 1. Concurrent limit — don't exceed MAX_CONCURRENT_REQUESTS.
    /// 2. Debounce — wait DEBOUNCE_MS after last input change.
    /// 3. Dedup — if an active request already has the same input, only retry
    ///    after timeout (2× for empty input to reduce spam).
    /// 4. Require new input — sequence_id must have advanced since last send.
    pub fn should_request(&self, current_sequence_id: u64, current_input: &str) -> bool {
        if self.active_requests.len() >= MAX_CONCURRENT_REQUESTS {
            return false;
        }

        let debounce_expired = match self.last_change {
            Some(t) => t.elapsed().as_millis() >= DEBOUNCE_MS as u128,
            None => false,
        };
        if !debounce_expired {
            return false;
        }

        // If an active request already covers this input, only allow retry after timeout.
        let timeout = if current_input.is_empty() {
            IN_FLIGHT_TIMEOUT_MS * 2
        } else {
            IN_FLIGHT_TIMEOUT_MS
        };
        for req in self.active_requests.values() {
            if req.input == current_input {
                return req.sent_at.elapsed().as_millis() >= timeout as u128;
            }
        }

        // Only send if there's been new input (first request or sequence advanced).
        self.sent_seq == 0 || current_sequence_id > self.sent_seq
    }

    /// Mark that a request was sent.
    pub fn mark_sent(&mut self, sequence_id: u64, input: &str) {
        // Track this request
        self.active_requests.insert(sequence_id, RequestState {
            input: input.to_string(),
            sent_at: Instant::now(),
            sequence_id,
        });

        self.sent_seq = sequence_id;
        self.sent_input = input.to_string();
    }

    /// Check if a response is relevant given the current input state.
    /// A response is relevant if the current input is compatible with the input
    /// that generated the response (either user kept typing or backspaced).
    fn is_response_relevant(&self, response: &CompletionResponse, current_input: &str) -> bool {
        // Get the input that generated this response
        if let Some(request_state) = self.active_requests.get(&response.sequence_id) {
            let original_input = &request_state.input;

            // Response is relevant if:
            // 1. Current input starts with original input (user kept typing)
            // 2. Current input is a prefix of original input (user backspaced)
            current_input.starts_with(original_input) ||
            original_input.starts_with(current_input)
        } else {
            // Request timed out or was cleaned up - response is not relevant
            false
        }
    }

    /// Get the input that was sent with a specific request.
    fn get_request_input(&self, sequence_id: &u64) -> Option<String> {
        self.active_requests.get(sequence_id).map(|state| state.input.clone())
    }

    /// Process a completion response.
    /// Returns the ghost text to display, if any.
    /// Now supports concurrent requests with intelligent filtering.
    pub fn on_response(&mut self, response: &CompletionResponse, current_input: &str) -> Option<&str> {
        // Get the request input before removing the request
        let request_input = self.get_request_input(&response.sequence_id).unwrap_or_else(|| self.sent_input.clone());

        // Check if this response is relevant to current input state
        if !self.is_response_relevant(response, current_input) {
            // Remove the request even if it's not relevant (to clean up)
            self.active_requests.remove(&response.sequence_id);
            return None;
        }

        // Discard stale response (older than current pending sequence)
        if response.sequence_id < self.pending_seq {
            // Remove the request even if it's stale (to clean up)
            self.active_requests.remove(&response.sequence_id);
            return None;
        }

        // Get request state before removing it (for timing check)
        let request_state = self.active_requests.get(&response.sequence_id).cloned();

        // Remove this completed request from active tracking (only after validation)
        self.active_requests.remove(&response.sequence_id);

        // Issue #21: Don't show ghost text if user has typed after the request was sent
        if let (Some(last_change), Some(request_state)) = (self.last_change, request_state) {
            if last_change > request_state.sent_at {
                // User typed after request was sent - discard completion
                self.current_ghost = None;
                return None;
            }
        }

        // Take best suggestion
        if let Some(best) = response
            .suggestions
            .iter()
            .max_by(|a, b| a.confidence.partial_cmp(&b.confidence).unwrap_or(std::cmp::Ordering::Equal))
        {
            if !best.text.is_empty() {
                // Compute full suggestion based on what was sent to LLM
                // LLM returns either:
                // 1. Full command (may or may not start with request_input)
                // 2. Suffix after cursor (does not include request_input)
                // Determine which case based on whether suggestion starts with request_input
                let full_suggestion = if request_input.is_empty() {
                    // No sent input means LLM returned full command
                    best.text.clone()
                } else if best.text.starts_with(&request_input) {
                    // Suggestion starts with request_input - it's a full command
                    best.text.clone()
                } else {
                    // Suggestion doesn't start with request_input - it's a suffix
                    format!("{}{}", request_input, best.text)
                };

                // Check if current input is a prefix of the full suggestion
                if full_suggestion.starts_with(current_input) {
                    let suffix = &full_suggestion[current_input.len()..];
                    if suffix.is_empty() {
                        self.current_ghost = None;
                        return None;
                    }
                    self.current_ghost = Some(suffix.to_string());
                    self.ghost_input = current_input.to_string();
                    self.ghost_set_at = Some(Instant::now());
                    return self.current_ghost.as_deref();
                } else {
                    // Current input is not a prefix of full suggestion
                    // Discard completion according to issue #6
                    self.current_ghost = None;
                    return None;
                }
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

    /// Clear ghost text and clean up any active requests.
    /// Called when prompt appears or user cancels completion.
    pub fn clear(&mut self) {
        self.current_ghost = None;
        self.ghost_input.clear();
        self.ghost_set_at = None;
        // Clear active requests when ghost is cleared
        self.active_requests.clear();
        // Reset last_change to prevent immediate requests on empty prompt
        self.last_change = None;
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

    /// Get the input that produced the current ghost text.
    pub fn ghost_input(&self) -> &str {
        &self.ghost_input
    }

    /// Get debug state for troubleshooting concurrent requests
    pub fn get_debug_state(&self) -> (usize, u64, u64, Vec<u64>) {
        let active_request_ids: Vec<u64> = self.active_requests.keys().copied().collect();
        (self.active_requests.len(), self.sent_seq, self.pending_seq, active_request_ids)
    }

    /// Clean up timed-out requests and return count of cleaned requests
    pub fn cleanup_timed_out_requests(&mut self) -> usize {
        let now = Instant::now();
        let timed_out: Vec<u64> = self.active_requests
            .iter()
            .filter(|(_, req)| now.duration_since(req.sent_at).as_millis() >= IN_FLIGHT_TIMEOUT_MS as u128)
            .map(|(seq, _)| *seq)
            .collect();

        let count = timed_out.len();
        for seq in timed_out {
            self.active_requests.remove(&seq);
        }
        count
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
        assert!(!c.should_request(1, "git"));
    }

    #[test]
    fn test_debounce_ready_short_input() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("g", 1);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(c.should_request(1, "g"));
    }

    #[test]
    fn test_debounce_ready_empty_input() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("", 1);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(c.should_request(1, ""));
    }

    #[test]
    fn test_debounce_ready_after_timeout() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(c.should_request(5, "git sta"));
    }

    #[test]
    fn test_no_duplicate_request() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1));
        c.mark_sent(5, "git sta");
        assert!(!c.should_request(5, "git sta"));
    }

    #[test]
    fn test_stale_response_discarded() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git sta", 5);
        c.mark_sent(5, "git sta");
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
        c.mark_sent(5, "git sta");

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
        c.mark_sent(5, "git sta");

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
        c.mark_sent(5, "git sta");

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
        c.mark_sent(3, "git");

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
        c.mark_sent(3, "git");

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
        c.mark_sent(1, "cargo");

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
        c.mark_sent(1, "cargo");

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
        c.mark_sent(1, "ls");

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
        c.mark_sent(1, "");

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
        for (i, _ch) in "cargo".chars().enumerate() {
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
        c.mark_sent(5, "git sta");

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
        c.mark_sent(1, "cargo");

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

    /// Test scenario: completion request sent with prefix "git st", user continues
    /// typing "at", when completion returns "git status", ghost text should be "us".
    #[test]
    fn test_ghost_calculation_when_input_changes_during_request() {
        let mut c = ShellCompleter::new();

        // Simulate request sent with input "git st", sequence_id 1
        c.on_input_changed("git st", 1);
        c.mark_sent(1, "git st");

        // User continues typing "at" - input is now "git stat"
        // In real scenario, on_input_changed would be called with new sequence_id,
        // but for this test we want to simulate that the response arrives with
        // sequence_id 1 but current input has changed, and the response hasn't
        // been marked stale (e.g., due to race condition).
        // We keep pending_seq = 1 so sequence_id check passes.
        // pending_seq is already 1 from on_input_changed above.

        // LLM returns full command "git status" (based on original input "git st")
        let resp = CompletionResponse {
            sequence_id: 1,
            suggestions: vec![CompletionSuggestion {
                text: "git status".to_string(),
                confidence: 0.9,
            }],
        };

        // Current input is "git stat", which is a prefix of "git status"
        let ghost = c.on_response(&resp, "git stat");
        // Should extract suffix "us" ("git status".len() - "git stat".len() = 2)
        assert_eq!(ghost, Some("us"));
        assert_eq!(c.ghost(), Some("us"));
    }

    /// Test scenario: completion request sent with prefix "git st", user continues
    /// typing "at", when completion returns suffix "atus", ghost text should be "us".
    #[test]
    fn test_ghost_calculation_when_suffix_returned_and_input_changes() {
        let mut c = ShellCompleter::new();

        // Simulate request sent with input "git st", sequence_id 1
        c.on_input_changed("git st", 1);
        c.mark_sent(1, "git st");

        // User continues typing "at" - input is now "git stat"
        // Response arrives before on_input_changed is called (race condition)
        // pending_seq remains 1, so response is not stale

        // LLM returns suffix "atus" (relative to original input "git st")
        let resp = CompletionResponse {
            sequence_id: 1,
            suggestions: vec![CompletionSuggestion {
                text: "atus".to_string(),
                confidence: 0.9,
            }],
        };

        // Current input is "git stat"
        let ghost = c.on_response(&resp, "git stat");
        // With sent_input tracking, we can reconstruct full suggestion:
        // sent_input = "git st", suffix = "atus" -> full = "git status"
        // Current input "git stat" is a prefix of "git status"
        // Ghost should be "us" (remaining suffix after current input)
        assert_eq!(ghost, Some("us"), "Should compute correct ghost text when input changes during request");
        assert_eq!(c.ghost(), Some("us"));
    }

    /// Test for issue 6: when current input is not a prefix of the full suggestion,
    /// the completion should be discarded.
    #[test]
    fn test_completion_discarded_when_input_not_prefix() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git ", 1);
        c.mark_sent(1, "git ");

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

    // --- Concurrent request tests ---

    #[test]
    fn test_concurrent_requests_allowed() {
        let mut c = ShellCompleter::new();
        c.on_input_changed("git", 1);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1)); // Debounce expired

        // First request
        assert!(c.should_request(1, "git"));
        c.mark_sent(1, "git");

        // Second request should be allowed (concurrent)
        c.on_input_changed("git s", 2);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1)); // Debounce expired
        assert!(c.should_request(2, "git s"));
        c.mark_sent(2, "git s");

        assert_eq!(c.active_requests.len(), 2);
    }

    #[test]
    fn test_concurrent_request_limit() {
        let mut c = ShellCompleter::new();

        // Send MAX_CONCURRENT_REQUESTS
        for i in 0..MAX_CONCURRENT_REQUESTS {
            c.on_input_changed(&format!("input{}", i), i as u64);
            c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1)); // Debounce expired
            assert!(c.should_request(i as u64, &format!("input{}", i)));
            c.mark_sent(i as u64, &format!("input{}", i));
        }

        assert_eq!(c.active_requests.len(), MAX_CONCURRENT_REQUESTS);

        // Next request should be blocked
        c.on_input_changed("too_many", MAX_CONCURRENT_REQUESTS as u64);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1)); // Debounce expired
        assert!(!c.should_request(MAX_CONCURRENT_REQUESTS as u64, "too_many"));
    }

    #[test]
    fn test_response_relevance_filtering() {
        let mut c = ShellCompleter::new();

        // Send request for "git st"
        c.on_input_changed("git st", 1);
        c.mark_sent(1, "git st");

        // Response arrives before user continues typing
        let resp = CompletionResponse {
            sequence_id: 1,
            suggestions: vec![CompletionSuggestion {
                text: "atus".to_string(),
                confidence: 0.9,
            }],
        };

        // Response should be processed successfully
        let ghost = c.on_response(&resp, "git st");
        assert_eq!(ghost, Some("atus"), "Response should be processed when input hasn't changed");

        // Now user continues typing to "git stat" - the existing ghost should be trimmed
        c.on_input_changed("git stat", 2);
        assert_eq!(c.ghost(), Some("us"), "Ghost should be trimmed when user continues typing");
    }

    #[test]
    fn test_response_irrelevance_filtering() {
        let mut c = ShellCompleter::new();

        // Send request for "git st"
        c.on_input_changed("git st", 1);
        c.mark_sent(1, "git st");

        // User completely changes input to something unrelated
        let resp = CompletionResponse {
            sequence_id: 1,
            suggestions: vec![CompletionSuggestion {
                text: "atus".to_string(),
                confidence: 0.9,
            }],
        };

        // Response should be irrelevant when input completely changed
        let ghost = c.on_response(&resp, "ls -la");
        assert_eq!(ghost, None, "Response should be irrelevant when input completely changed");
    }

    #[test]
    fn test_timed_out_request_cleanup() {
        let mut c = ShellCompleter::new();

        // Send a request
        c.on_input_changed("test", 1);
        c.mark_sent(1, "test");

        assert_eq!(c.active_requests.len(), 1);

        // Manually mark the request as timed out by backdating it
        if let Some(req) = c.active_requests.get_mut(&1) {
            req.sent_at = Instant::now() - std::time::Duration::from_secs(10);
        }

        // should_request should allow new request due to timeout
        c.on_input_changed("test new", 2);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1)); // Debounce expired
        assert!(c.should_request(2, "test new"));

        // Cleanup should remove timed out request
        let cleaned = c.cleanup_timed_out_requests();
        assert_eq!(cleaned, 1);
        assert_eq!(c.active_requests.len(), 0);
    }

    #[test]
    fn test_stale_response_discarded_with_concurrent() {
        let mut c = ShellCompleter::new();

        // Send request 1
        c.on_input_changed("git sta", 5);
        c.mark_sent(5, "git sta");

        // User continues typing, sequence advances
        c.on_input_changed("git status", 10);

        // Response for old sequence should be discarded
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
    fn test_clear_clears_active_requests() {
        let mut c = ShellCompleter::new();

        // Send multiple requests
        for i in 0..3 {
            c.on_input_changed(&format!("input{}", i), i as u64);
            c.mark_sent(i as u64, &format!("input{}", i));
        }

        assert_eq!(c.active_requests.len(), 3);

        // Clear should remove all active requests
        c.clear();
        assert_eq!(c.active_requests.len(), 0);
    }

    #[test]
    fn test_debug_state_shows_concurrent_info() {
        let mut c = ShellCompleter::new();

        // Send some requests
        c.on_input_changed("test1", 1);
        c.mark_sent(1, "test1");
        c.on_input_changed("test2", 2);
        c.mark_sent(2, "test2");

        let (active_count, sent_seq, pending_seq, active_ids) = c.get_debug_state();
        assert_eq!(active_count, 2);
        assert_eq!(sent_seq, 2);
        assert_eq!(pending_seq, 2);
        assert_eq!(active_ids.len(), 2);
        assert!(active_ids.contains(&1));
        assert!(active_ids.contains(&2));
    }

    #[test]
    fn test_concurrent_requests_with_different_inputs() {
        let mut c = ShellCompleter::new();

        // Test concurrent requests by sending multiple requests without waiting for responses
        // We need to simulate the scenario where responses arrive before the next input change
        // So we keep pending_seq at the original value for all requests

        // Set up initial state with "git s" as the current input
        c.on_input_changed("git s", 3);
        c.pending_seq = 1; // Reset to simulate race condition where responses arrive before sequence advances

        // Now simulate sending requests with different inputs but same pending_seq
        c.mark_sent(1, "git");
        c.mark_sent(2, "git ");
        c.mark_sent(3, "git s");

        assert_eq!(c.active_requests.len(), 3);

        // Now process responses in order - they should all work with current input "git s"
        let current_input = "git s";

        // Response for "git" should be relevant (current input starts with original)
        let resp1 = CompletionResponse {
            sequence_id: 1,
            suggestions: vec![CompletionSuggestion {
                text: " status".to_string(),
                confidence: 0.8,
            }],
        };
        let ghost1 = c.on_response(&resp1, current_input);
        assert_eq!(ghost1, Some("tatus"), "Response 1 should work");

        // Response for "git " should be relevant
        let resp2 = CompletionResponse {
            sequence_id: 2,
            suggestions: vec![CompletionSuggestion {
                text: "status".to_string(),
                confidence: 0.9,
            }],
        };
        let ghost2 = c.on_response(&resp2, current_input);
        assert_eq!(ghost2, Some("tatus"), "Response 2 should work");

        // Response for "git s" should be most relevant
        let resp3 = CompletionResponse {
            sequence_id: 3,
            suggestions: vec![CompletionSuggestion {
                text: "tatus".to_string(),
                confidence: 1.0,
            }],
        };
        let ghost3 = c.on_response(&resp3, current_input);
        assert_eq!(ghost3, Some("tatus"), "Response 3 should work");

        // The final ghost should be from the last response processed
        assert_eq!(c.ghost(), Some("tatus"), "Final ghost should be from last response");
    }

    #[test]
    fn test_no_duplicate_requests_after_daemon_restart() {
        let mut c = ShellCompleter::new();

        // Simulate daemon restart scenario:
        // 1. Request was sent but daemon restarted
        // 2. Request times out but stays in active_requests
        // 3. No new user input (pending_seq == sent_seq)
        // 4. should_request should return false to prevent duplicate requests

        // Initial state: user typed something, request sent
        c.on_input_changed("git", 24);
        c.mark_sent(24, "git");
        assert_eq!(c.active_requests.len(), 1);

        // Simulate time passing (daemon restart)
        // Manually make the request timed out
        if let Some(req) = c.active_requests.get_mut(&24) {
            req.sent_at = Instant::now() - std::time::Duration::from_secs(10);
        }

        // No new input, same sequence
        assert_eq!(c.pending_seq, 24);
        assert_eq!(c.sent_seq, 24);

        // should_request should return false - no new input means no new requests
        assert!(!c.should_request(24, "git"), "Should not allow new requests when there's no new input, even with timed-out requests");

        // Cleanup should remove the timed-out request
        let cleaned = c.cleanup_timed_out_requests();
        assert_eq!(cleaned, 1);
        assert_eq!(c.active_requests.len(), 0);

        // After cleanup, still should not allow requests without new input
        assert!(!c.should_request(24, "git"), "Should still not allow requests without new input");

        // Now simulate new user input
        c.on_input_changed("git s", 25);
        c.last_change = Some(Instant::now() - std::time::Duration::from_secs(1)); // Debounce expired

        // Now should_request should return true
        assert!(c.should_request(25, "git s"), "Should allow new request when there's new input");
    }
}
