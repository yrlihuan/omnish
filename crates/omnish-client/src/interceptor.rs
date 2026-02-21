use std::collections::VecDeque;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// EscSeqFilter – state machine that distinguishes bare ESC from ESC sequences
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum EscSeqResult {
    /// Still accumulating bytes; caller should keep feeding.
    Pending,
    /// Bracketed-paste content collected; insert into chat buffer.
    Insert(Vec<u8>),
    /// Recognised sequence that should be silently dropped (arrow keys, Del, …).
    Ignore,
    /// Bare ESC (or ESC + non-`[`) → cancel chat/buffering mode.
    Cancel,
}

#[derive(Debug)]
enum EscState {
    /// Just received `\x1b`, waiting for next byte.
    EscGot,
    /// Inside a CSI sequence (`\x1b[`), accumulating parameter bytes.
    CsiParam(Vec<u8>),
    /// Collecting pasted content (between `\x1b[200~` and `\x1b[201~`).
    Paste(Vec<u8>),
    /// Inside paste, just saw `\x1b`.
    PasteEsc(Vec<u8>),
    /// Inside paste, saw `\x1b[`, accumulating CSI param digits.
    PasteCsi(Vec<u8>, Vec<u8>), // (paste_buf, param_buf)
}

pub struct EscSeqFilter {
    state: Option<EscState>,
}

impl EscSeqFilter {
    pub fn new() -> Self {
        Self { state: None }
    }

    /// Feed one byte. Returns `Pending` while the sequence is incomplete.
    pub fn feed(&mut self, byte: u8) -> EscSeqResult {
        let state = match self.state.take() {
            Some(s) => s,
            None => {
                if byte == 0x1b {
                    self.state = Some(EscState::EscGot);
                    return EscSeqResult::Pending;
                }
                // Not in any ESC state – shouldn't normally be called.
                return EscSeqResult::Cancel;
            }
        };

        match state {
            EscState::EscGot => {
                if byte == b'[' {
                    self.state = Some(EscState::CsiParam(Vec::new()));
                    EscSeqResult::Pending
                } else {
                    // ESC followed by non-'[' → treat as bare ESC
                    EscSeqResult::Cancel
                }
            }
            EscState::CsiParam(mut params) => {
                if byte.is_ascii_digit() || byte == b';' {
                    params.push(byte);
                    self.state = Some(EscState::CsiParam(params));
                    EscSeqResult::Pending
                } else if byte == b'~' {
                    // Check for bracketed-paste start (200) or other function keys
                    if params == b"200" {
                        self.state = Some(EscState::Paste(Vec::new()));
                        EscSeqResult::Pending
                    } else {
                        // Function key (Delete = 3~, Home = 1~, etc.) → ignore
                        EscSeqResult::Ignore
                    }
                } else if byte.is_ascii_alphabetic() {
                    // Arrow keys (A/B/C/D), other CSI sequences → ignore
                    EscSeqResult::Ignore
                } else {
                    // Unexpected byte → cancel
                    EscSeqResult::Cancel
                }
            }
            EscState::Paste(mut paste_buf) => {
                if byte == 0x1b {
                    self.state = Some(EscState::PasteEsc(paste_buf));
                    EscSeqResult::Pending
                } else {
                    paste_buf.push(byte);
                    self.state = Some(EscState::Paste(paste_buf));
                    EscSeqResult::Pending
                }
            }
            EscState::PasteEsc(paste_buf) => {
                if byte == b'[' {
                    self.state = Some(EscState::PasteCsi(paste_buf, Vec::new()));
                    EscSeqResult::Pending
                } else {
                    // Not a CSI inside paste – put ESC and this byte into paste buf
                    let mut pb = paste_buf;
                    pb.push(0x1b);
                    pb.push(byte);
                    self.state = Some(EscState::Paste(pb));
                    EscSeqResult::Pending
                }
            }
            EscState::PasteCsi(paste_buf, mut params) => {
                if byte.is_ascii_digit() || byte == b';' {
                    params.push(byte);
                    self.state = Some(EscState::PasteCsi(paste_buf, params));
                    EscSeqResult::Pending
                } else if byte == b'~' && params == b"201" {
                    // End of bracketed paste
                    EscSeqResult::Insert(paste_buf)
                } else {
                    // Not the end marker – fold the partial seq into paste buf
                    let mut pb = paste_buf;
                    pb.push(0x1b);
                    pb.push(b'[');
                    pb.extend_from_slice(&params);
                    pb.push(byte);
                    self.state = Some(EscState::Paste(pb));
                    EscSeqResult::Pending
                }
            }
        }
    }

    /// Call after processing all bytes from one `read()` batch.
    /// If we're sitting in `EscGot` state (bare `\x1b` with nothing after),
    /// that means the user pressed Esc alone → Cancel.
    pub fn finish_batch(&mut self) -> Option<EscSeqResult> {
        match self.state.take() {
            Some(EscState::EscGot) => Some(EscSeqResult::Cancel),
            other => {
                self.state = other;
                None
            }
        }
    }
}

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
    /// ESC sequence in progress — no UI update needed
    Pending,
    /// Tab pressed while in chat mode. Contains current buffer.
    /// Caller should check GhostCompleter for completion to accept.
    Tab(Vec<u8>),
}

/// Strategy for deciding whether to start intercepting at the current moment.
/// Allows swapping between time-gap heuristic, prompt detection, etc.
pub trait InterceptGuard {
    /// Record that user input was forwarded to the shell (not intercepted).
    fn note_input(&mut self);
    /// Return true if the interceptor should try to match the prefix right now.
    fn should_intercept(&self) -> bool;
}

/// Always intercept — used in tests.
#[cfg(test)]
pub struct AlwaysIntercept;

#[cfg(test)]
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
    /// Active ESC sequence filter (only while in chat/buffering mode).
    esc_filter: Option<EscSeqFilter>,
}

impl InputInterceptor {
    pub fn new(prefix: &str, guard: Box<dyn InterceptGuard>) -> Self {
        Self {
            prefix: prefix.as_bytes().to_vec(),
            buffer: VecDeque::new(),
            in_chat: false,
            suppressed: false,
            guard,
            esc_filter: None,
        }
    }

    /// Set suppression state (e.g. when alternate screen is active)
    pub fn set_suppressed(&mut self, suppressed: bool) {
        if suppressed && !self.suppressed {
            // Entering suppressed mode: discard any partial buffer
            self.buffer.clear();
            self.in_chat = false;
            self.esc_filter = None;
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

    /// Call after processing all bytes from one `read()` batch.
    /// If a bare ESC is pending (user pressed Esc alone), returns Cancel.
    pub fn finish_batch(&mut self) -> Option<InterceptAction> {
        if let Some(ref mut filter) = self.esc_filter {
            if let Some(result) = filter.finish_batch() {
                self.esc_filter = None;
                return Some(self.apply_esc_result(result));
            }
        }
        None
    }

    /// Convert an EscSeqResult into an InterceptAction.
    fn apply_esc_result(&mut self, result: EscSeqResult) -> InterceptAction {
        match result {
            EscSeqResult::Cancel => {
                self.buffer.clear();
                self.in_chat = false;
                InterceptAction::Cancel
            }
            EscSeqResult::Ignore => {
                // Arrow keys, Del, etc. — silently drop, no UI update
                InterceptAction::Pending
            }
            EscSeqResult::Insert(data) => {
                // Bracketed paste content — append to buffer
                for &b in &data {
                    self.buffer.push_back(b);
                }
                let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
                InterceptAction::Buffering(current_buf)
            }
            EscSeqResult::Pending => {
                // Shouldn't happen from finish_batch, but treat as no-op
                InterceptAction::Pending
            }
        }
    }

    /// Feed a single input byte, returns action
    pub fn feed_byte(&mut self, byte: u8) -> InterceptAction {
        // When suppressed (e.g. inside vim), forward everything directly
        if self.suppressed {
            return self.forward(vec![byte]);
        }

        // If an ESC sequence filter is active, feed bytes into it
        if self.esc_filter.is_some() {
            let result = self.esc_filter.as_mut().unwrap().feed(byte);
            match result {
                EscSeqResult::Pending => return InterceptAction::Pending,
                _ => {
                    self.esc_filter = None;
                    return self.apply_esc_result(result);
                }
            }
        }

        // Handle ESC — start filter in chat/buffering mode, forward otherwise
        if byte == 0x1b {
            if self.in_chat || !self.buffer.is_empty() {
                let mut filter = EscSeqFilter::new();
                filter.feed(byte); // transitions to EscGot
                self.esc_filter = Some(filter);
                return InterceptAction::Pending;
            } else {
                return self.forward(vec![byte]);
            }
        }

        // Handle Tab
        if byte == b'\t' {
            if self.in_chat {
                let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
                return InterceptAction::Tab(current_buf);
            } else if !self.buffer.is_empty() {
                // During prefix matching, flush buffer + tab to PTY
                let mut flushed: Vec<u8> = self.buffer.iter().copied().collect();
                flushed.push(byte);
                self.buffer.clear();
                return self.forward(flushed);
            } else {
                return self.forward(vec![byte]);
            }
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

    /// Inject bytes directly into the buffer (for accepting completions).
    pub fn inject_byte(&mut self, byte: u8) {
        self.buffer.push_back(byte);
    }

    /// Get a copy of the current buffer contents.
    pub fn current_buffer(&self) -> Vec<u8> {
        self.buffer.iter().copied().collect()
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

    // --- ESC / escape-sequence tests ---

    #[test]
    fn test_esc_cancels_chat_mode() {
        let mut interceptor = new_interceptor("::");
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b'h');

        // ESC alone enters Pending
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Pending);
        // finish_batch sees bare ESC → Cancel
        assert_eq!(interceptor.finish_batch(), Some(InterceptAction::Cancel));

        // After cancel, normal input resumes
        assert_eq!(interceptor.feed_byte(b'x'), InterceptAction::Forward(vec![b'x']));
    }

    #[test]
    fn test_esc_cancels_prefix_buffering() {
        let mut interceptor = new_interceptor("::");
        interceptor.feed_byte(b':');

        // ESC while still matching prefix → Pending
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Pending);
        // finish_batch → Cancel
        assert_eq!(interceptor.finish_batch(), Some(InterceptAction::Cancel));
        assert_eq!(interceptor.feed_byte(b'x'), InterceptAction::Forward(vec![b'x']));
    }

    #[test]
    fn test_esc_ignored_when_not_buffering() {
        let mut interceptor = new_interceptor("::");

        // ESC with empty buffer — forward to PTY (could be start of arrow key etc.)
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Forward(vec![0x1b]));
    }

    #[test]
    fn test_esc_then_non_bracket_cancels() {
        let mut interceptor = new_interceptor("::");
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b'h');

        // ESC then a non-'[' byte → Cancel immediately (not via finish_batch)
        interceptor.feed_byte(0x1b);
        assert_eq!(interceptor.feed_byte(b'x'), InterceptAction::Cancel);

        // Normal input resumes
        assert_eq!(interceptor.feed_byte(b'y'), InterceptAction::Forward(vec![b'y']));
    }

    #[test]
    fn test_arrow_keys_ignored_in_chat() {
        let mut interceptor = new_interceptor("::");
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b'h');

        // Up arrow: \x1b[A
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Pending);
        assert_eq!(interceptor.feed_byte(b'['), InterceptAction::Pending);
        // Final byte 'A' completes the CSI → Ignore → Pending (no UI update)
        assert_eq!(interceptor.feed_byte(b'A'), InterceptAction::Pending);

        // Buffer is unchanged, still in chat mode
        assert_eq!(
            interceptor.feed_byte(b'i'),
            InterceptAction::Buffering(vec![b':', b':', b'h', b'i'])
        );
    }

    #[test]
    fn test_delete_key_ignored_in_chat() {
        let mut interceptor = new_interceptor("::");
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b'h');

        // Delete key: \x1b[3~
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Pending);
        assert_eq!(interceptor.feed_byte(b'['), InterceptAction::Pending);
        assert_eq!(interceptor.feed_byte(b'3'), InterceptAction::Pending);
        assert_eq!(interceptor.feed_byte(b'~'), InterceptAction::Pending);
    }

    #[test]
    fn test_paste_in_chat_mode() {
        let mut interceptor = new_interceptor("::");
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b':');

        // Bracketed paste: \x1b[200~hello world\x1b[201~
        let paste_seq = b"\x1b[200~hello world\x1b[201~";
        let mut last_action = InterceptAction::Buffering(vec![]);
        for &byte in paste_seq.iter() {
            last_action = interceptor.feed_byte(byte);
        }
        // After paste end, buffer should contain "::hello world"
        assert_eq!(
            last_action,
            InterceptAction::Buffering(vec![
                b':', b':', b'h', b'e', b'l', b'l', b'o', b' ',
                b'w', b'o', b'r', b'l', b'd'
            ])
        );
    }

    #[test]
    fn test_paste_with_newlines() {
        let mut interceptor = new_interceptor("::");
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b':');

        // Paste content with newlines
        let paste_seq = b"\x1b[200~line1\nline2\x1b[201~";
        let mut last_action = InterceptAction::Buffering(vec![]);
        for &byte in paste_seq.iter() {
            last_action = interceptor.feed_byte(byte);
        }
        assert_eq!(
            last_action,
            InterceptAction::Buffering(vec![
                b':', b':', b'l', b'i', b'n', b'e', b'1', b'\n',
                b'l', b'i', b'n', b'e', b'2'
            ])
        );
    }

    #[test]
    fn test_finish_batch_no_op_when_idle() {
        let mut interceptor = new_interceptor("::");
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b':');
        // No ESC fed, finish_batch should return None
        assert_eq!(interceptor.finish_batch(), None);
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

    // -----------------------------------------------------------------------
    // Integration tests: simulate main-loop rendering decisions
    // -----------------------------------------------------------------------

    /// Simulate the main loop's rendering logic for a batch of input bytes.
    /// Maps each InterceptAction to a descriptive string, mirroring the
    /// match arms in main.rs.  Calls `finish_batch` at the end of the batch.
    fn simulate_main_loop(interceptor: &mut InputInterceptor, batch: &[u8]) -> Vec<String> {
        let mut actions = Vec::new();
        for &byte in batch {
            match interceptor.feed_byte(byte) {
                InterceptAction::Buffering(ref buf) if buf == b":" => {
                    actions.push("prompt".into())
                }
                InterceptAction::Buffering(ref buf) if buf.starts_with(b":") => {
                    actions.push(format!(
                        "echo:{}",
                        String::from_utf8_lossy(&buf[1..])
                    ))
                }
                InterceptAction::Buffering(_) => actions.push("buffering".into()),
                InterceptAction::Forward(_) => actions.push("forward".into()),
                InterceptAction::Cancel => actions.push("cancel".into()),
                InterceptAction::Chat(msg) => actions.push(format!("chat:{msg}")),
                InterceptAction::Backspace(ref buf) if buf.is_empty() => {
                    actions.push("dismiss".into())
                }
                InterceptAction::Backspace(ref buf) => {
                    actions.push(format!(
                        "backspace:{}",
                        String::from_utf8_lossy(buf)
                    ))
                }
                InterceptAction::Tab(_) => actions.push("tab".into()),
                InterceptAction::Pending => actions.push("pending".into()),
            }
        }
        if let Some(action) = interceptor.finish_batch() {
            match action {
                InterceptAction::Cancel => actions.push("cancel".into()),
                _ => {}
            }
        }
        actions
    }

    #[test]
    fn test_ui_type_and_submit() {
        let mut ic = new_interceptor(":");
        let actions = simulate_main_loop(&mut ic, b":hello\n");
        assert_eq!(
            actions,
            vec!["prompt", "echo:h", "echo:he", "echo:hel", "echo:hell", "echo:hello", "chat:hello"]
        );
    }

    #[test]
    fn test_ui_type_and_esc_cancel() {
        let mut ic = new_interceptor(":");
        let mut input = b":hello".to_vec();
        input.push(0x1b); // ESC at batch end
        let actions = simulate_main_loop(&mut ic, &input);
        // Last action must be cancel (from finish_batch detecting bare ESC)
        assert_eq!(actions.last().unwrap(), "cancel");
        // prompt appears exactly once
        assert_eq!(actions.iter().filter(|a| *a == "prompt").count(), 1);
    }

    #[test]
    fn test_ui_paste_no_spurious_redraws() {
        let mut ic = new_interceptor(":");
        let mut input = vec![b':'];
        // Bracketed paste: \x1b[200~ps aux\x1b[201~
        input.extend_from_slice(b"\x1b[200~ps aux\x1b[201~");
        let actions = simulate_main_loop(&mut ic, &input);

        // prompt appears exactly once (at the first ':')
        assert_eq!(actions.iter().filter(|a| *a == "prompt").count(), 1);
        // Last action is the echo after paste insert
        assert_eq!(actions.last().unwrap(), "echo:ps aux");
        // Everything between prompt and last echo must be "pending"
        for a in &actions[1..actions.len() - 1] {
            assert_eq!(a, "pending", "expected pending during paste, got {a}");
        }
    }

    #[test]
    fn test_ui_arrow_keys_no_redraws() {
        let mut ic = new_interceptor(":");
        // Type "::h", then Up arrow, then Down arrow, then 'i'
        let mut input = b"::h".to_vec();
        input.extend_from_slice(b"\x1b[A"); // Up
        input.extend_from_slice(b"\x1b[B"); // Down
        input.push(b'i');
        let actions = simulate_main_loop(&mut ic, &input);

        // First three: prompt, echo::, echo::h
        assert_eq!(actions[0], "prompt");
        assert_eq!(actions[1], "echo::");
        assert_eq!(actions[2], "echo::h");
        // Arrow bytes are all pending (6 bytes → 6 pending)
        for a in &actions[3..9] {
            assert_eq!(a, "pending", "arrow key bytes should be pending");
        }
        // Final 'i' produces normal echo
        assert_eq!(actions[9], "echo::hi");
    }

    #[test]
    fn test_ui_esc_non_bracket_cancel() {
        let mut ic = new_interceptor(":");
        // Type "::h", then ESC + 'x' in same batch (non-bracket → immediate cancel)
        let actions = simulate_main_loop(&mut ic, b"::h\x1bx");
        assert_eq!(
            actions,
            vec!["prompt", "echo::", "echo::h", "pending", "cancel"]
        );
    }

    #[test]
    fn test_ui_backspace_to_empty_dismisses() {
        let mut ic = new_interceptor(":");
        let mut input = b":he".to_vec();
        input.extend_from_slice(&[0x7f, 0x7f, 0x7f]); // 3× backspace
        let actions = simulate_main_loop(&mut ic, &input);
        assert_eq!(
            actions,
            vec!["prompt", "echo:h", "echo:he", "backspace::h", "backspace::", "dismiss"]
        );
    }

    #[test]
    fn test_ui_multiple_esc_sequences() {
        let mut ic = new_interceptor(":");
        // Type "::", then Up arrow, Down arrow, then 'x'
        let mut input = b"::".to_vec();
        input.extend_from_slice(b"\x1b[A"); // Up
        input.extend_from_slice(b"\x1b[B"); // Down
        input.push(b'x');
        let actions = simulate_main_loop(&mut ic, &input);

        assert_eq!(actions[0], "prompt");
        assert_eq!(actions[1], "echo::");
        // 6 pending bytes from two arrow sequences
        for a in &actions[2..8] {
            assert_eq!(a, "pending");
        }
        assert_eq!(actions[8], "echo::x");
    }

    #[test]
    fn test_ui_paste_then_enter() {
        let mut ic = new_interceptor(":");
        let mut input = vec![b':'];
        input.extend_from_slice(b"\x1b[200~hello\x1b[201~");
        input.push(b'\n');
        let actions = simulate_main_loop(&mut ic, &input);

        // Should end with echo of pasted text, then chat
        let len = actions.len();
        assert_eq!(actions[len - 2], "echo:hello");
        assert_eq!(actions[len - 1], "chat:hello");
    }

    #[test]
    fn test_ui_paste_with_embedded_ansi() {
        let mut ic = new_interceptor(":");
        let mut input = vec![b':'];
        // Paste content contains ANSI color codes: \x1b[32mhi\x1b[0m
        input.extend_from_slice(b"\x1b[200~\x1b[32mhi\x1b[0m\x1b[201~");
        let actions = simulate_main_loop(&mut ic, &input);

        // Last action should be an echo containing the full ANSI content
        let last = actions.last().unwrap();
        assert!(last.starts_with("echo:"), "last action should be echo, got {last}");
        let content = &last["echo:".len()..];
        assert!(content.contains("hi"), "pasted content should contain 'hi'");
        assert!(content.contains("\x1b[32m"), "pasted content should preserve ANSI SGR");
        assert!(content.contains("\x1b[0m"), "pasted content should preserve ANSI reset");
    }

    #[test]
    fn test_ui_prefix_mismatch_then_retry() {
        let mut ic = new_interceptor("::");
        // First attempt ":x" mismatches prefix "::", then retry "::hi\n"
        let actions = simulate_main_loop(&mut ic, b":x::hi\n");
        assert_eq!(
            actions,
            vec!["prompt", "forward", "prompt", "echo::", "echo::h", "echo::hi", "chat:hi"]
        );
    }

    // --- Tab tests ---

    #[test]
    fn test_tab_in_chat_mode_returns_tab_action() {
        let mut ic = new_interceptor(":");
        ic.feed_byte(b':');
        ic.feed_byte(b'h');
        assert_eq!(ic.feed_byte(b'\t'), InterceptAction::Tab(vec![b':', b'h']));
    }

    #[test]
    fn test_tab_not_in_chat_forwards() {
        let mut ic = new_interceptor(":");
        assert_eq!(ic.feed_byte(b'\t'), InterceptAction::Forward(vec![b'\t']));
    }

    #[test]
    fn test_tab_during_prefix_buffering() {
        let mut ic = new_interceptor("::");
        ic.feed_byte(b':');
        assert_eq!(ic.feed_byte(b'\t'), InterceptAction::Forward(vec![b':', b'\t']));
    }

    // --- inject_byte / current_buffer tests ---

    #[test]
    fn test_inject_byte_and_current_buffer() {
        let mut ic = new_interceptor(":");
        ic.feed_byte(b':');
        ic.feed_byte(b'h');
        ic.inject_byte(b'e');
        ic.inject_byte(b'l');
        ic.inject_byte(b'p');
        assert_eq!(ic.current_buffer(), b":help");
    }
}
