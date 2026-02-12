use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub enum InterceptAction {
    /// Buffering input, don't send to PTY yet
    /// Contains current buffer for echo display
    Buffering(Vec<u8>),
    /// Forward these bytes to PTY
    Forward(Vec<u8>),
    /// Chat mode message completed (user pressed Enter after prefix)
    Chat(String),
    /// Backspace in buffering mode - erased one char
    /// Contains updated buffer for echo display
    Backspace(Vec<u8>),
    /// User pressed ESC to cancel chat mode
    Cancel,
}

/// Strategy for deciding whether to start intercepting at the current moment.
/// Allows swapping between time-gap heuristic, prompt detection, etc.
pub trait InterceptGuard {
    /// Record that user input was forwarded to the shell (not intercepted).
    fn note_input(&mut self);
    /// Return true if the interceptor should try to match the prefix right now.
    fn should_intercept(&self) -> bool;
}

/// Always intercept — used in tests and when no guard logic is needed.
pub struct AlwaysIntercept;

impl InterceptGuard for AlwaysIntercept {
    fn note_input(&mut self) {}
    fn should_intercept(&self) -> bool { true }
}

/// Intercept only when enough time has elapsed since the last forwarded input.
/// This heuristic assumes that if the user hasn't typed for a while, they're
/// at a fresh shell prompt rather than in the middle of a command.
pub struct TimeGapGuard {
    last_input: Option<Instant>,
    min_gap: Duration,
}

impl TimeGapGuard {
    pub fn new(min_gap: Duration) -> Self {
        Self {
            last_input: None,
            min_gap,
        }
    }
}

impl InterceptGuard for TimeGapGuard {
    fn note_input(&mut self) {
        self.last_input = Some(Instant::now());
    }

    fn should_intercept(&self) -> bool {
        match self.last_input {
            None => true, // No prior input — likely at initial prompt
            Some(t) => t.elapsed() >= self.min_gap,
        }
    }
}

pub struct InputInterceptor {
    prefix: Vec<u8>,
    buffer: VecDeque<u8>,
    in_chat: bool,
    /// When true, all input is forwarded directly (e.g. inside vim/less)
    suppressed: bool,
    guard: Box<dyn InterceptGuard>,
}

impl InputInterceptor {
    pub fn new(prefix: &str, guard: Box<dyn InterceptGuard>) -> Self {
        Self {
            prefix: prefix.as_bytes().to_vec(),
            buffer: VecDeque::new(),
            in_chat: false,
            suppressed: false,
            guard,
        }
    }

    /// Set suppression state (e.g. when alternate screen is active)
    pub fn set_suppressed(&mut self, suppressed: bool) {
        if suppressed && !self.suppressed {
            // Entering suppressed mode: discard any partial buffer
            self.buffer.clear();
            self.in_chat = false;
        }
        self.suppressed = suppressed;
    }

    /// Note output from shell (to detect prompt and reset state)
    pub fn note_output(&mut self, _data: &[u8]) {
        // On any output from shell, reset chat state
        // This handles Ctrl+C, Ctrl+D, etc. that cancel partial input
        if self.in_chat {
            self.in_chat = false;
            self.buffer.clear();
        }
    }

    /// Feed a single input byte, returns action
    pub fn feed_byte(&mut self, byte: u8) -> InterceptAction {
        // When suppressed (e.g. inside vim), forward everything directly
        if self.suppressed {
            return self.forward(vec![byte]);
        }

        // Handle ESC — cancel chat mode
        if byte == 0x1b && (self.in_chat || !self.buffer.is_empty()) {
            self.buffer.clear();
            self.in_chat = false;
            return InterceptAction::Cancel;
        }

        // Handle backspace/delete
        if byte == 0x7f || byte == 0x08 {
            // If we're buffering or in chat mode, handle backspace
            if !self.buffer.is_empty() && (self.in_chat || self.buffer.len() <= self.prefix.len()) {
                // Delete one UTF-8 character (may be multiple bytes)
                // Work backwards to find the start of the last character
                let buf_vec: Vec<u8> = self.buffer.iter().copied().collect();
                let as_str = String::from_utf8_lossy(&buf_vec);
                let mut chars = as_str.chars().collect::<Vec<_>>();

                if !chars.is_empty() {
                    chars.pop(); // Remove last character
                    let new_str: String = chars.into_iter().collect();
                    self.buffer = new_str.as_bytes().iter().copied().collect();
                }

                // Check if we dropped out of chat mode
                if self.in_chat && self.buffer.len() < self.prefix.len() {
                    self.in_chat = false;
                }

                let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
                return InterceptAction::Backspace(current_buf);
            } else {
                // Not buffering, forward to PTY
                return self.forward(vec![byte]);
            }
        }

        self.buffer.push_back(byte);

        // Check for Enter/newline
        if byte == b'\n' || byte == b'\r' {
            return self.handle_enter();
        }

        // Check if buffer matches prefix so far
        if !self.in_chat && self.buffer.len() <= self.prefix.len() {
            if self.buffer.iter().copied().collect::<Vec<_>>() == self.prefix[..self.buffer.len()] {
                // On first prefix byte, check guard
                if self.buffer.len() == 1 && !self.guard.should_intercept() {
                    let flushed: Vec<u8> = self.buffer.iter().copied().collect();
                    self.buffer.clear();
                    return self.forward(flushed);
                }

                // Still matching prefix
                if self.buffer.len() == self.prefix.len() {
                    // Complete prefix match
                    self.in_chat = true;
                }
                // Keep buffering, don't send to PTY yet, return buffer for echo
                let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
                return InterceptAction::Buffering(current_buf);
            } else {
                // Prefix mismatch, flush buffer to PTY
                let flushed: Vec<u8> = self.buffer.iter().copied().collect();
                self.buffer.clear();
                return self.forward(flushed);
            }
        }

        // In chat mode, keep buffering and return for echo
        if self.in_chat {
            let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
            return InterceptAction::Buffering(current_buf);
        }

        // Not in chat mode and buffer exceeded prefix length - flush and reset
        let flushed: Vec<u8> = self.buffer.iter().copied().collect();
        self.buffer.clear();
        self.forward(flushed)
    }

    /// Forward bytes and record input activity for the guard.
    fn forward(&mut self, bytes: Vec<u8>) -> InterceptAction {
        self.guard.note_input();
        InterceptAction::Forward(bytes)
    }

    fn handle_enter(&mut self) -> InterceptAction {
        let buffered: Vec<u8> = self.buffer.iter().copied().collect();
        self.buffer.clear();

        if !self.in_chat {
            // Not in chat mode, forward to PTY
            return self.forward(buffered);
        }

        // Extract chat message after prefix
        self.in_chat = false;
        if buffered.len() > self.prefix.len() {
            let cmd_bytes = &buffered[self.prefix.len()..buffered.len() - 1]; // exclude final \n
            if let Ok(cmd_str) = std::str::from_utf8(cmd_bytes) {
                return InterceptAction::Chat(cmd_str.to_string());
            }
        }

        // Empty message or decode error, forward to PTY
        self.forward(buffered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_interceptor(prefix: &str) -> InputInterceptor {
        InputInterceptor::new(prefix, Box::new(AlwaysIntercept))
    }

    #[test]
    fn test_passthrough_normal_input() {
        let mut interceptor = new_interceptor("::");
        // 'l' doesn't match prefix, so buffer is flushed
        assert_eq!(interceptor.feed_byte(b'l'), InterceptAction::Forward(vec![b'l']));
        assert_eq!(interceptor.feed_byte(b's'), InterceptAction::Forward(vec![b's']));
        assert_eq!(interceptor.feed_byte(b'\n'), InterceptAction::Forward(vec![b'\n']));
    }

    #[test]
    fn test_chat_detected() {
        let mut interceptor = new_interceptor("::");
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':', b':']));
        assert_eq!(interceptor.feed_byte(b'a'), InterceptAction::Buffering(vec![b':', b':', b'a']));
        assert_eq!(interceptor.feed_byte(b's'), InterceptAction::Buffering(vec![b':', b':', b'a', b's']));
        assert_eq!(interceptor.feed_byte(b'k'), InterceptAction::Buffering(vec![b':', b':', b'a', b's', b'k']));

        if let InterceptAction::Chat(cmd) = interceptor.feed_byte(b'\n') {
            assert_eq!(cmd, "ask");
        } else {
            panic!("Expected Chat action");
        }
    }

    #[test]
    fn test_chat_with_query() {
        let mut interceptor = new_interceptor("::");
        let input = b"::ask why did this fail\n";
        for (idx, &byte) in input.iter().enumerate() {
            let action = interceptor.feed_byte(byte);
            if byte == b'\n' {
                if let InterceptAction::Chat(cmd) = action {
                    assert_eq!(cmd, "ask why did this fail");
                } else {
                    panic!("Expected Chat action");
                }
            } else {
                if let InterceptAction::Buffering(buf) = action {
                    assert_eq!(buf, &input[..=idx]);
                } else {
                    panic!("Expected Buffering action");
                }
            }
        }
    }

    #[test]
    fn test_partial_prefix_then_mismatch() {
        let mut interceptor = new_interceptor("::");
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        // Mismatch - should flush ":x"
        assert_eq!(interceptor.feed_byte(b'x'), InterceptAction::Forward(vec![b':', b'x']));
        // New input after buffer cleared
        assert_eq!(interceptor.feed_byte(b'\n'), InterceptAction::Forward(vec![b'\n']));
    }

    #[test]
    fn test_note_output_resets_chat_state() {
        let mut interceptor = new_interceptor("::");
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b':');
        // Now in chat mode
        interceptor.note_output(b"some output");
        // Chat state should be reset
        assert_eq!(interceptor.in_chat, false);
        assert_eq!(interceptor.buffer.len(), 0);
    }

    #[test]
    fn test_backspace_in_chat_mode() {
        let mut interceptor = new_interceptor("::");
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':', b':']));
        assert_eq!(interceptor.feed_byte(b'a'), InterceptAction::Buffering(vec![b':', b':', b'a']));

        // Backspace should remove 'a'
        assert_eq!(interceptor.feed_byte(0x7f), InterceptAction::Backspace(vec![b':', b':']));

        // Still in chat mode
        assert_eq!(interceptor.feed_byte(b'b'), InterceptAction::Buffering(vec![b':', b':', b'b']));
    }

    #[test]
    fn test_backspace_out_of_chat_mode() {
        let mut interceptor = new_interceptor("::");
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':', b':']));

        // Backspace once
        assert_eq!(interceptor.feed_byte(0x7f), InterceptAction::Backspace(vec![b':']));

        // Backspace again - should drop out of buffering
        assert_eq!(interceptor.feed_byte(0x7f), InterceptAction::Backspace(vec![]));

        // Next char should be forwarded normally
        assert_eq!(interceptor.feed_byte(b'x'), InterceptAction::Forward(vec![b'x']));
    }

    #[test]
    fn test_backspace_when_not_buffering() {
        let mut interceptor = new_interceptor("::");
        // Type something that doesn't match prefix
        interceptor.feed_byte(b'l');

        // Backspace should be forwarded to PTY
        assert_eq!(interceptor.feed_byte(0x7f), InterceptAction::Forward(vec![0x7f]));
    }

    #[test]
    fn test_backspace_multibyte_chars() {
        let mut interceptor = new_interceptor("::");
        // Type "::ask 中文"
        for &byte in b"::ask " {
            interceptor.feed_byte(byte);
        }

        // Add Chinese characters "中文" (each is 3 bytes in UTF-8)
        let chinese = "中文";
        for &byte in chinese.as_bytes() {
            interceptor.feed_byte(byte);
        }

        // Buffer should contain "::ask 中文"
        let buf_before = interceptor.buffer.iter().copied().collect::<Vec<_>>();
        let str_before = String::from_utf8_lossy(&buf_before);
        assert_eq!(str_before, "::ask 中文");

        // Backspace should remove "文" (3 bytes)
        let action = interceptor.feed_byte(0x7f);
        if let InterceptAction::Backspace(buf) = action {
            let result = String::from_utf8_lossy(&buf);
            assert_eq!(result, "::ask 中");
        } else {
            panic!("Expected Backspace action");
        }

        // Backspace again should remove "中" (3 bytes)
        let action = interceptor.feed_byte(0x7f);
        if let InterceptAction::Backspace(buf) = action {
            let result = String::from_utf8_lossy(&buf);
            assert_eq!(result, "::ask ");
        } else {
            panic!("Expected Backspace action");
        }
    }

    #[test]
    fn test_suppressed_forwards_everything() {
        let mut interceptor = new_interceptor("::");
        interceptor.set_suppressed(true);

        // "::" prefix should NOT trigger buffering when suppressed
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Forward(vec![b':']));
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Forward(vec![b':']));
        assert_eq!(interceptor.feed_byte(b'a'), InterceptAction::Forward(vec![b'a']));
        assert_eq!(interceptor.feed_byte(b'\n'), InterceptAction::Forward(vec![b'\n']));
    }

    #[test]
    fn test_suppressed_then_resumed() {
        let mut interceptor = new_interceptor("::");
        interceptor.set_suppressed(true);

        // Should forward while suppressed
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Forward(vec![b':']));

        // Unsuppress
        interceptor.set_suppressed(false);

        // Should intercept again
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':', b':']));
    }

    #[test]
    fn test_suppressed_discards_partial_buffer() {
        let mut interceptor = new_interceptor("::");

        // Start typing a chat message
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':', b':']));
        assert_eq!(interceptor.feed_byte(b'a'), InterceptAction::Buffering(vec![b':', b':', b'a']));

        // Enter suppressed mode (e.g. vim opened) - should discard buffer
        interceptor.set_suppressed(true);

        // Everything forwards
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Forward(vec![b':']));

        // Exit suppressed mode
        interceptor.set_suppressed(false);

        // Should start fresh, no leftover buffer
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
    }

    // --- ESC cancel tests ---

    #[test]
    fn test_esc_cancels_chat_mode() {
        let mut interceptor = new_interceptor("::");
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b'h');

        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Cancel);

        // After cancel, normal input resumes
        assert_eq!(interceptor.feed_byte(b'x'), InterceptAction::Forward(vec![b'x']));
    }

    #[test]
    fn test_esc_cancels_prefix_buffering() {
        let mut interceptor = new_interceptor("::");
        interceptor.feed_byte(b':');

        // ESC while still matching prefix
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Cancel);
        assert_eq!(interceptor.feed_byte(b'x'), InterceptAction::Forward(vec![b'x']));
    }

    #[test]
    fn test_esc_ignored_when_not_buffering() {
        let mut interceptor = new_interceptor("::");

        // ESC with empty buffer — forward to PTY (could be start of arrow key etc.)
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Forward(vec![0x1b]));
    }

    // --- Guard-specific tests ---

    #[test]
    fn test_time_gap_guard_no_prior_input() {
        let guard = TimeGapGuard::new(Duration::from_secs(1));
        // No prior input: should allow interception
        assert!(guard.should_intercept());
    }

    #[test]
    fn test_time_gap_guard_recent_input_blocks() {
        let mut guard = TimeGapGuard::new(Duration::from_secs(1));
        guard.note_input();
        // Just noted input: gap is ~0, should NOT intercept
        assert!(!guard.should_intercept());
    }

    #[test]
    fn test_time_gap_guard_stale_input_allows() {
        let mut guard = TimeGapGuard::new(Duration::from_millis(10));
        guard.note_input();
        std::thread::sleep(Duration::from_millis(15));
        // Enough time has passed: should intercept
        assert!(guard.should_intercept());
    }

    #[test]
    fn test_guard_blocks_prefix_mid_input() {
        // Simulate rapid typing: guard says "don't intercept"
        let mut guard = TimeGapGuard::new(Duration::from_secs(1));
        guard.note_input(); // just typed something

        let mut interceptor = InputInterceptor::new(":", Box::new(guard));

        // ":" right after other input → guard blocks → Forward
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Forward(vec![b':']));
    }

    #[test]
    fn test_guard_allows_prefix_after_gap() {
        // No prior input → guard allows interception
        let guard = TimeGapGuard::new(Duration::from_secs(1));
        let mut interceptor = InputInterceptor::new(":", Box::new(guard));

        // ":" with no prior input → guard allows → Buffering (enters chat mode)
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        assert_eq!(interceptor.feed_byte(b'h'), InterceptAction::Buffering(vec![b':', b'h']));
    }

    #[test]
    fn test_guard_forward_updates_timestamp() {
        let guard = TimeGapGuard::new(Duration::from_secs(1));
        let mut interceptor = InputInterceptor::new(":", Box::new(guard));

        // 'x' doesn't match prefix → forwarded → guard.note_input() called
        assert_eq!(interceptor.feed_byte(b'x'), InterceptAction::Forward(vec![b'x']));

        // Immediately type ':' → guard says no (just forwarded 'x')
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Forward(vec![b':']));
    }
}
