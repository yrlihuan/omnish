/// Tracks the current shell command-line input by observing forwarded bytes
/// and OSC 133 state transitions.
///
/// Lifecycle (caller in main.rs maps OSC 133 events to these methods):
/// 1. OSC 133;A/D (PromptStart/CommandEnd) -> on_prompt(): at_prompt = true
/// 2. Enter key (0x0d) in feed_forwarded -> at_prompt = false
///    (OSC 133;B/C are NOT used for at_prompt because the bash DEBUG trap
///     fires during PS1 command substitution, not just on user Enter)
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
    /// ESC sequence state: 0=normal, 1=saw ESC, 2=in CSI params
    esc_state: u8,
    /// When true, a readline report is expected; `take_change()` returns None
    /// to suppress stale completions until the real readline content arrives.
    pending_rl_report: bool,
}

impl ShellInputTracker {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            at_prompt: true,  // assume we start at a prompt
            sequence_id: 0,
            changed: false,
            esc_state: 0,
            pending_rl_report: false,
        }
    }

    /// Call when OSC 133;A (PromptStart) or 133;D (CommandEnd) is detected.
    pub fn on_prompt(&mut self) {
        self.at_prompt = true;
        self.input.clear();
        self.esc_state = 0;
        self.bump(); // always bump so completion can fire on empty prompt
    }

    /// Feed bytes that were forwarded to the PTY (user's raw input).
    /// Only processes input while at the prompt.
    pub fn feed_forwarded(&mut self, bytes: &[u8]) {
        if !self.at_prompt {
            return;
        }
        for &b in bytes {
            // ESC sequence state machine: skip CSI and single-char escapes
            match self.esc_state {
                1 => {
                    if b == b'[' {
                        self.esc_state = 2;
                    } else {
                        self.esc_state = 0;
                    }
                    continue;
                }
                2 => {
                    if (0x40..=0x7e).contains(&b) {
                        self.esc_state = 0;
                    }
                    continue;
                }
                _ => {}
            }
            if b == 0x1b {
                self.esc_state = 1;
                continue;
            }
            match b {
                // Enter -> command submitted, no longer at prompt
                0x0d | 0x0a => {
                    self.at_prompt = false;
                    self.input.clear();
                    self.bump();
                }
                // Ctrl+C -> cancel current input
                0x03 => {
                    if !self.input.is_empty() {
                        self.input.clear();
                        self.bump();
                    }
                }
                // Ctrl+U -> clear line
                0x15 => {
                    if !self.input.is_empty() {
                        self.input.clear();
                        self.bump();
                    }
                }
                // Backspace / DEL -> remove last char
                0x7f | 0x08 => {
                    if self.input.pop().is_some() {
                        self.bump();
                    }
                }
                // Tab -> don't append (it's a completion trigger)
                0x09 => {}
                // Printable ASCII
                0x20..=0x7e => {
                    self.input.push(b as char);
                    self.bump();
                }
                // Ignore control chars and escape sequences
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

    /// Whether a readline report is pending (e.g., after Tab/Up/Down/Ctrl+R).
    pub fn pending_rl_report(&self) -> bool {
        self.pending_rl_report
    }

    /// ESC sequence processing state for debugging.
    /// Returns: 0=normal, 1=saw ESC, 2=in CSI params
    pub fn esc_state(&self) -> u8 {
        self.esc_state
    }

    /// Get detailed debug information about current input state.
    pub fn get_debug_info(&self) -> (String, u64, bool, bool, u8) {
        (
            self.input.clone(),
            self.sequence_id,
            self.at_prompt,
            self.pending_rl_report,
            self.esc_state,
        )
    }

    /// Replace the tracked input with the real readline content reported by bash.
    /// Only effective while at the prompt.
    /// Only bumps sequence_id if content actually changed, to avoid spurious
    /// completion requests from readline triggers sent for response processing.
    pub fn set_readline(&mut self, content: &str) {
        if !self.at_prompt {
            return;
        }
        self.pending_rl_report = false;
        if self.input != content {
            self.input = content.to_string();
            self.bump();
        }
    }

    /// Mark that a readline report is expected (e.g. after sending a trigger
    /// sequence for Tab/Up/Down). While pending, `take_change()` returns None
    /// to avoid triggering completions based on stale input.
    pub fn mark_pending_report(&mut self) {
        self.pending_rl_report = true;
    }

    /// Check if input changed since last call, and return current state.
    /// Returns None if a readline report is pending (to suppress stale completions).
    pub fn take_change(&mut self) -> Option<(&str, u64)> {
        if self.pending_rl_report {
            self.changed = false;
            return None;
        }
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
        assert_eq!(t.sequence_id(), 6);
    }

    #[test]
    fn test_backspace() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"lss");
        t.feed_forwarded(&[0x7f]);
        assert_eq!(t.input(), "ls");
    }

    #[test]
    fn test_enter_clears() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"ls");
        t.feed_forwarded(&[0x0d]);
        assert_eq!(t.input(), "");
    }

    #[test]
    fn test_ctrl_c_clears() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"some cmd");
        t.feed_forwarded(&[0x03]);
        assert_eq!(t.input(), "");
    }

    #[test]
    fn test_ctrl_u_clears() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"some cmd");
        t.feed_forwarded(&[0x15]);
        assert_eq!(t.input(), "");
    }

    #[test]
    fn test_prompt_cycle_with_enter() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"ls");
        assert_eq!(t.input(), "ls");
        // Enter sets at_prompt=false and clears input
        t.feed_forwarded(&[0x0d]);
        assert_eq!(t.input(), "");
        assert!(!t.at_prompt());
        // OSC 133;A/D restores at_prompt
        t.on_prompt();
        assert!(t.at_prompt());
        assert_eq!(t.input(), "");
    }

    #[test]
    fn test_on_prompt_bumps_even_when_empty() {
        let mut t = ShellInputTracker::new();
        let seq_before = t.sequence_id();
        t.on_prompt();
        assert!(t.sequence_id() > seq_before);
        assert!(t.take_change().is_some());
    }

    #[test]
    fn test_ignores_input_after_enter() {
        let mut t = ShellInputTracker::new();
        // Enter sets at_prompt=false
        t.feed_forwarded(&[0x0d]);
        t.feed_forwarded(b"output bytes");
        assert_eq!(t.input(), "");
    }

    #[test]
    fn test_take_change() {
        let mut t = ShellInputTracker::new();
        assert!(t.take_change().is_none());
        t.feed_forwarded(b"g");
        let (input, seq) = t.take_change().unwrap();
        assert_eq!(input, "g");
        assert_eq!(seq, 1);
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
        assert_eq!(t.input(), "git");
    }

    #[test]
    fn test_esc_sequence_skipped() {
        let mut t = ShellInputTracker::new();
        // Arrow up sends ESC [ A — should not appear in input
        t.feed_forwarded(b"\x1b[A");
        assert_eq!(t.input(), "", "ESC sequence should be skipped");
    }

    #[test]
    fn test_esc_sequence_then_typing() {
        let mut t = ShellInputTracker::new();
        // Arrow up (ESC [ A), then type "ls"
        t.feed_forwarded(b"\x1b[Als");
        assert_eq!(t.input(), "ls", "real input after ESC sequence should be kept");
    }

    #[test]
    fn test_inject() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"git");
        t.inject(" status");
        assert_eq!(t.input(), "git status");
    }

    #[test]
    fn test_set_readline_updates_input() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"gi");
        let _ = t.take_change(); // consume
        t.set_readline("git");
        assert_eq!(t.input(), "git");
        let (input, _seq) = t.take_change().unwrap();
        assert_eq!(input, "git");
    }

    #[test]
    fn test_pending_report_suppresses_change() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"gi");
        let _ = t.take_change(); // consume
        t.mark_pending_report();
        t.feed_forwarded(b"t");
        // take_change returns None while pending
        assert!(t.take_change().is_none());
    }

    #[test]
    fn test_set_readline_clears_pending() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"gi");
        let _ = t.take_change();
        t.mark_pending_report();
        assert!(t.take_change().is_none());
        t.set_readline("git");
        // After set_readline, take_change works again
        let (input, _seq) = t.take_change().unwrap();
        assert_eq!(input, "git");
    }

    /// Regression: set_readline with unchanged content should NOT bump sequence_id.
    /// This prevents the completion loop where readline triggers (sent to get
    /// current input for processing a completion response) cause spurious
    /// sequence bumps that make should_request think there's new user input.
    #[test]
    fn test_set_readline_same_content_no_bump() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"git");
        let seq_after_typing = t.sequence_id();
        let _ = t.take_change(); // consume

        // Simulate readline trigger for completion: content unchanged
        t.mark_pending_report();
        t.set_readline("git"); // same content

        // Should clear pending_rl_report
        assert!(!t.pending_rl_report());
        // But should NOT bump sequence_id
        assert_eq!(t.sequence_id(), seq_after_typing,
            "set_readline with unchanged content should not bump sequence_id");
        // take_change should return None (no actual change)
        assert!(t.take_change().is_none(),
            "No change should be reported when readline content is the same");
    }

    #[test]
    fn test_set_readline_ignored_when_not_at_prompt() {
        let mut t = ShellInputTracker::new();
        t.feed_forwarded(b"ls");
        t.feed_forwarded(&[0x0d]); // Enter — not at prompt
        assert!(!t.at_prompt());
        t.set_readline("should be ignored");
        assert_eq!(t.input(), "");
    }
}
