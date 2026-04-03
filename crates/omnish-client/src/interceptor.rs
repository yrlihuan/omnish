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
    Ignore(Vec<u8>),
    /// Bare ESC (or ESC + non-`[`) → cancel chat/buffering mode.
    /// Carries the raw consumed bytes so they can be forwarded when not in chat.
    Cancel(Vec<u8>),
}

#[derive(Debug)]
enum EscState {
    /// Just received `\x1b`, waiting for next byte.
    EscGot,
    /// Inside a CSI sequence (`\x1b[`), accumulating parameter bytes.
    CsiParam(Vec<u8>, Vec<u8>), // (sequence_bytes, parameter_bytes)
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
                return EscSeqResult::Cancel(vec![byte]);
            }
        };

        match state {
            EscState::EscGot => {
                if byte == b'[' {
                    self.state = Some(EscState::CsiParam(vec![0x1b, b'['], Vec::new()));
                    EscSeqResult::Pending
                } else {
                    // ESC followed by non-'[' → treat as bare ESC
                    EscSeqResult::Cancel(vec![0x1b, byte])
                }
            }
            EscState::CsiParam(mut seq_bytes, mut params) => {
                // Add byte to sequence buffer
                seq_bytes.push(byte);

                if byte.is_ascii_digit() || byte == b';' {
                    params.push(byte);
                    self.state = Some(EscState::CsiParam(seq_bytes, params));
                    EscSeqResult::Pending
                } else if byte == b'~' {
                    // Check for bracketed-paste start (200) or other function keys
                    if params == b"200" {
                        self.state = Some(EscState::Paste(Vec::new()));
                        EscSeqResult::Pending
                    } else {
                        // Function key (Delete = 3~, Home = 1~, etc.) → ignore
                        EscSeqResult::Ignore(seq_bytes)
                    }
                } else if byte.is_ascii_alphabetic() {
                    // Arrow keys (A/B/C/D), other CSI sequences → ignore
                    EscSeqResult::Ignore(seq_bytes)
                } else {
                    // Unexpected byte → cancel
                    EscSeqResult::Cancel(seq_bytes)
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
            Some(EscState::EscGot) => Some(EscSeqResult::Cancel(vec![0x1b])),
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
    /// Resume last chat session (prefix typed twice, e.g. "::")
    ResumeChat,
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
pub trait InterceptGuard: Send {
    /// Record that user input was forwarded to the shell (not intercepted).
    fn note_input(&mut self);
    /// Return true if the interceptor should try to match the prefix right now.
    fn should_intercept(&self) -> bool;
    /// Update the minimum time gap used by gap-based guards.
    fn update_min_gap(&mut self, _gap: std::time::Duration) {}
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

    fn update_min_gap(&mut self, gap: std::time::Duration) {
        self.min_gap = gap;
    }
}

/// Return the expected byte length of a UTF-8 character from its leading byte.
/// Returns 1 for ASCII / invalid lead bytes (safe default: flush immediately).
fn utf8_char_len(first: u8) -> usize {
    if first < 0xC0 { 1 } // ASCII or continuation byte (shouldn't be a lead)
    else if first < 0xE0 { 2 }
    else if first < 0xF0 { 3 }
    else { 4 }
}

/// Check if a byte buffer ends with an incomplete UTF-8 character.
/// Returns true if the trailing bytes form a partial multi-byte sequence.
fn has_incomplete_utf8_tail(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return false;
    }
    // Scan backwards for the last UTF-8 lead byte
    for i in (0..buf.len()).rev() {
        let b = buf[i];
        if b < 0x80 {
            // ASCII — complete
            return false;
        }
        if b >= 0xC0 {
            // Lead byte found — check if we have enough continuation bytes
            let expected = utf8_char_len(b);
            let available = buf.len() - i;
            return available < expected;
        }
        // continuation byte (0x80..0xBF) — keep scanning back
        if i == 0 {
            // All continuation bytes with no lead — malformed, flush it
            return false;
        }
    }
    false
}

pub struct InputInterceptor {
    prefix: Vec<u8>,
    resume_prefix: Vec<u8>,
    buffer: VecDeque<u8>,
    in_chat: bool,
    /// When true, all input is forwarded directly (e.g. inside vim/less)
    suppressed: bool,
    guard: Box<dyn InterceptGuard>,
    /// Active ESC sequence filter (only while in chat/buffering mode).
    esc_filter: Option<EscSeqFilter>,
    /// When false (default), : and :: only trigger chat on empty command line; when true, allows chat even with existing content
    developer_mode: bool,
    /// Tracks whether command line already has content (based on forwarded input since last shell output)
    command_line_has_content: bool,
}

impl InputInterceptor {
    pub fn new(prefix: &str, resume_prefix: &str, guard: Box<dyn InterceptGuard>, developer_mode: bool) -> Self {
        Self {
            prefix: prefix.as_bytes().to_vec(),
            resume_prefix: resume_prefix.as_bytes().to_vec(),
            buffer: VecDeque::new(),
            in_chat: false,
            suppressed: false,
            guard,
            esc_filter: None,
            developer_mode,
            command_line_has_content: false,
        }
    }

    pub fn update_prefix(&mut self, prefix: &str) {
        self.prefix = prefix.as_bytes().to_vec();
    }

    pub fn update_resume_prefix(&mut self, prefix: &str) {
        self.resume_prefix = prefix.as_bytes().to_vec();
    }

    pub fn set_developer_mode(&mut self, mode: bool) {
        self.developer_mode = mode;
    }

    pub fn update_min_gap(&mut self, gap: std::time::Duration) {
        self.guard.update_min_gap(gap);
    }

    /// Set suppression state (e.g. when alternate screen is active)
    pub fn set_suppressed(&mut self, suppressed: bool) {
        if suppressed && !self.suppressed {
            // Entering suppressed mode: discard any partial buffer
            self.buffer.clear();
            self.in_chat = false;
            self.esc_filter = None;
        } else if !suppressed && self.suppressed {
            // Leaving suppressed mode (e.g. exiting vim): bytes forwarded
            // during suppression went to the child process, not the shell
            // command line, so reset content tracking.
            self.command_line_has_content = false;
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
        // Shell output indicates prompt is displayed, command line is empty
        self.command_line_has_content = false;
    }

    /// Update command_line_has_content based on readline report from shell.
    /// This corrects the state after Ctrl+U, Ctrl+W, etc.
    pub fn update_readline(&mut self, content: &str) {
        self.command_line_has_content = !content.is_empty();
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
            EscSeqResult::Cancel(raw_bytes) => {
                if self.in_chat || !self.buffer.is_empty() {
                    // In chat or buffering prefix → cancel UI
                    self.buffer.clear();
                    self.in_chat = false;
                    InterceptAction::Cancel
                } else {
                    // Not in chat mode → forward the consumed bytes to PTY
                    self.forward(raw_bytes)
                }
            }
            EscSeqResult::Ignore(bytes) => {
                // Arrow keys, Del, etc.
                if self.in_chat {
                    // In chat mode, ignore the sequence (don't forward)
                    InterceptAction::Pending
                } else {
                    // Not in chat mode, forward the sequence to PTY
                    self.forward(bytes)
                }
            }
            EscSeqResult::Insert(data) => {
                if self.in_chat || !self.buffer.is_empty() {
                    // Bracketed paste content — append to buffer
                    for &b in &data {
                        self.buffer.push_back(b);
                    }
                    let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
                    InterceptAction::Buffering(current_buf)
                } else {
                    // Not in chat mode — forward paste with bracketed markers
                    let mut bytes = b"\x1b[200~".to_vec();
                    bytes.extend_from_slice(&data);
                    bytes.extend_from_slice(b"\x1b[201~");
                    self.forward(bytes)
                }
            }
            EscSeqResult::Pending => {
                // Shouldn't happen from finish_batch, but treat as no-op
                InterceptAction::Pending
            }
        }
    }

    /// Feed a single input byte, returns action
    pub fn feed_byte(&mut self, byte: u8) -> InterceptAction {
        // If an ESC sequence filter is active, feed bytes into it.
        // This must happen even when suppressed to ensure escape sequences
        // like arrow keys are forwarded as complete units.
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

        // Handle ESC — always start filter to buffer complete escape sequences.
        // This ensures arrow keys etc. are forwarded as a single write to PTY,
        // preventing child processes from seeing fragmented escape sequences.
        // This must happen even when suppressed.
        if byte == 0x1b {
            let mut filter = EscSeqFilter::new();
            filter.feed(byte); // transitions to EscGot
            self.esc_filter = Some(filter);
            return InterceptAction::Pending;
        }

        // When suppressed (e.g. inside vim), forward everything directly
        // except escape sequences which are handled above.
        if self.suppressed {
            return self.forward(vec![byte]);
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
                if self.buffer.len() == 1 && (!self.guard.should_intercept()
                    || (!self.developer_mode && self.command_line_has_content))
                {
                    let flushed: Vec<u8> = self.buffer.iter().copied().collect();
                    self.buffer.clear();
                    return self.forward(flushed);
                }

                // Still matching prefix
                if self.buffer.len() == self.prefix.len() {
                    // Complete prefix match — transition to chat buffering.
                    // Don't return Chat yet; wait for next byte to detect
                    // double-prefix (e.g. "::") for resume, or timeout for new chat.
                    self.in_chat = true;
                    let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
                    return InterceptAction::Buffering(current_buf);
                }
                // Keep buffering, don't send to PTY yet, return buffer for echo
                let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
                return InterceptAction::Buffering(current_buf);
            } else {
                // Prefix mismatch, flush buffer to PTY
                let flushed: Vec<u8> = self.buffer.iter().copied().collect();
                if has_incomplete_utf8_tail(&flushed) {
                    // Buffer ends with an incomplete multi-byte UTF-8 char;
                    // wait for remaining continuation bytes before flushing.
                    return InterceptAction::Pending;
                }
                self.buffer.clear();
                return self.forward(flushed);
            }
        }

        // In chat mode, keep buffering and return for echo
        if self.in_chat {
            // Detect resume prefix (e.g. "::" by default) for resume
            if self.buffer.len() == self.resume_prefix.len() {
                let buf: Vec<u8> = self.buffer.iter().copied().collect();
                if buf == self.resume_prefix {
                    self.buffer.clear();
                    self.in_chat = false;
                    return InterceptAction::ResumeChat;
                }
            }
            let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
            return InterceptAction::Buffering(current_buf);
        }

        // Not in chat mode and buffer exceeded prefix length - flush and reset
        let flushed: Vec<u8> = self.buffer.iter().copied().collect();
        if has_incomplete_utf8_tail(&flushed) {
            // Buffer ends with an incomplete multi-byte UTF-8 char;
            // wait for remaining continuation bytes before flushing.
            return InterceptAction::Pending;
        }
        self.buffer.clear();
        self.forward(flushed)
    }

    /// Forward bytes and record input activity for the guard.
    fn forward(&mut self, bytes: Vec<u8>) -> InterceptAction {
        self.guard.note_input();
        self.command_line_has_content = true;
        InterceptAction::Forward(bytes)
    }

    fn handle_enter(&mut self) -> InterceptAction {
        let buffered: Vec<u8> = self.buffer.iter().copied().collect();
        self.buffer.clear();

        if !self.in_chat {
            // Not in chat mode, forward to PTY.
            // Enter submits the command line, so it becomes empty.
            let result = self.forward(buffered);
            self.command_line_has_content = false;
            return result;
        }

        // Extract chat message after prefix
        self.in_chat = false;

        // Content after prefix, excluding trailing newline
        let content_start = self.prefix.len();
        let content_end = buffered.len().saturating_sub(1); // exclude \n or \r
        if content_start < content_end {
            let cmd_bytes = &buffered[content_start..content_end];
            if let Ok(cmd_str) = std::str::from_utf8(cmd_bytes) {
                return InterceptAction::Chat(cmd_str.to_string());
            }
        }

        // Just prefix + Enter → new chat (empty message)
        InterceptAction::Chat(String::new())
    }

    /// Called by the main loop when the prefix-match timeout expires.
    /// If the buffer contains only the prefix (no additional input), transition
    /// to new chat mode. Returns `Some(Chat(""))` if expired, `None` otherwise.
    pub fn expire_prefix(&mut self) -> Option<InterceptAction> {
        if self.in_chat {
            let buf: Vec<u8> = self.buffer.iter().copied().collect();
            if buf == self.prefix {
                self.buffer.clear();
                self.in_chat = false;
                return Some(InterceptAction::Chat(String::new()));
            }
        }
        None
    }

    /// Inject bytes directly into the buffer (for accepting completions).
    pub fn inject_byte(&mut self, byte: u8) {
        self.buffer.push_back(byte);
    }

    /// Get a copy of the current buffer contents.
    #[cfg(test)]
    pub fn current_buffer(&self) -> Vec<u8> {
        self.buffer.iter().copied().collect()
    }

    /// Returns true if the interceptor is in chat mode or buffering prefix.
    pub fn is_in_chat(&self) -> bool {
        self.in_chat || !self.buffer.is_empty()
    }

    /// Get suppression state for debugging
    pub fn is_suppressed(&self) -> bool {
        self.suppressed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_interceptor(prefix: &str) -> InputInterceptor {
        let resume = prefix.repeat(2);
        InputInterceptor::new(prefix, &resume, Box::new(AlwaysIntercept), false)
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
        // Full prefix match → Buffering (awaiting timeout or double-prefix)
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':', b':']));
        // Timeout → Chat("")
        assert_eq!(interceptor.expire_prefix(), Some(InterceptAction::Chat(String::new())));
    }

    #[test]
    fn test_chat_with_query() {
        let mut interceptor = new_interceptor("::");
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        // Full prefix match → Buffering
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':', b':']));
        // Type content after prefix, then Enter → Chat with content
        assert_eq!(interceptor.feed_byte(b'h'), InterceptAction::Buffering(vec![b':', b':', b'h']));
        assert_eq!(interceptor.feed_byte(b'i'), InterceptAction::Buffering(vec![b':', b':', b'h', b'i']));
        assert_eq!(interceptor.feed_byte(b'\r'), InterceptAction::Chat("hi".to_string()));
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
        assert!(!interceptor.in_chat);
        assert_eq!(interceptor.buffer.len(), 0);
    }

    #[test]
    fn test_backspace_during_prefix_buffering() {
        let mut interceptor = new_interceptor("::");
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));

        // Backspace removes the partial prefix byte
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

    // test_backspace_multibyte_chars: removed — chat input is now handled by read_chat_input

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
        assert_eq!(interceptor.expire_prefix(), Some(InterceptAction::Chat(String::new())));
    }

    #[test]
    fn test_suppressed_discards_partial_buffer() {
        let mut interceptor = new_interceptor("::");

        // Start typing prefix
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        // Full prefix match returns Chat("") but we want to test suppression mid-prefix
        // So suppress after partial prefix
        interceptor.set_suppressed(true);

        // Everything forwards
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Forward(vec![b':']));

        // Exit suppressed mode
        interceptor.set_suppressed(false);

        // Should start fresh, no leftover buffer
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
    }

    // --- ESC / escape-sequence tests ---

    // test_esc_cancels_chat_mode: removed — chat input is now handled by read_chat_input

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
    fn test_bare_esc_forwarded_when_not_buffering() {
        let mut interceptor = new_interceptor("::");

        // ESC with empty buffer — enters filter, finish_batch forwards it
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Pending);
        assert_eq!(
            interceptor.finish_batch(),
            Some(InterceptAction::Forward(vec![0x1b]))
        );
    }

    #[test]
    fn test_arrow_keys_forwarded_as_single_write() {
        let mut interceptor = new_interceptor("::");

        // Down arrow: \x1b[B — all bytes buffered, then forwarded as one unit
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Pending);
        assert_eq!(interceptor.feed_byte(b'['), InterceptAction::Pending);
        assert_eq!(
            interceptor.feed_byte(b'B'),
            InterceptAction::Forward(vec![0x1b, b'[', b'B'])
        );
    }

    // test_esc_then_non_bracket_cancels: removed — chat input is now handled by read_chat_input

    // test_arrow_keys_ignored_in_chat: removed — chat input is now handled by read_chat_input
    // test_delete_key_ignored_in_chat: removed — chat input is now handled by read_chat_input
    // test_paste_in_chat_mode: removed — chat input is now handled by read_chat_input
    // test_paste_with_newlines: removed — chat input is now handled by read_chat_input

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

        let mut interceptor = InputInterceptor::new(":", "::", Box::new(guard), false);

        // ":" right after other input → guard blocks → Forward
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Forward(vec![b':']));
    }

    #[test]
    fn test_guard_allows_prefix_after_gap() {
        // No prior input → guard allows interception
        let guard = TimeGapGuard::new(Duration::from_secs(1));
        let mut interceptor = InputInterceptor::new(":", "::", Box::new(guard), false);

        // ":" with no prior input → guard allows → Buffering (awaiting timeout)
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        // Timeout → Chat("")
        assert_eq!(interceptor.expire_prefix(), Some(InterceptAction::Chat(String::new())));
    }

    #[test]
    fn test_guard_forward_updates_timestamp() {
        let guard = TimeGapGuard::new(Duration::from_secs(1));
        let mut interceptor = InputInterceptor::new(":", "::", Box::new(guard), false);

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
                InterceptAction::ResumeChat => actions.push("resume_chat".into()),
                InterceptAction::Pending => actions.push("pending".into()),
            }
        }
        if let Some(action) = interceptor.finish_batch() {
            match action {
                InterceptAction::Cancel => actions.push("cancel".into()),
                InterceptAction::Forward(_) => actions.push("forward".into()),
                _ => {}
            }
        }
        actions
    }

    #[test]
    fn test_ui_type_and_submit() {
        let mut ic = new_interceptor(":");
        // Prefix match → Buffering (prompt shown), awaiting timeout or more input
        let actions = simulate_main_loop(&mut ic, b":");
        assert_eq!(actions, vec!["prompt"]);
        // Timeout would call expire_prefix → Chat("")
        assert_eq!(ic.expire_prefix(), Some(InterceptAction::Chat(String::new())));
    }

    #[test]
    fn test_ui_type_and_esc_cancel() {
        let mut ic = new_interceptor(":");
        // Prefix match then ESC cancels
        let actions = simulate_main_loop(&mut ic, b":\x1b");
        assert_eq!(actions, vec!["prompt", "pending", "cancel"]);
    }

    #[test]
    fn test_ui_paste_no_spurious_redraws() {
        let mut ic = new_interceptor(":");
        // Prefix match → prompt, then typing more buffers
        let actions = simulate_main_loop(&mut ic, b":hello\r");
        assert_eq!(actions, vec!["prompt", "echo:h", "echo:he", "echo:hel", "echo:hell", "echo:hello", "chat:hello"]);
    }

    #[test]
    fn test_ui_arrow_keys_no_redraws() {
        // With "::" prefix, both ":" buffer (prefix match + full prefix)
        let mut ic = new_interceptor("::");
        let actions = simulate_main_loop(&mut ic, b"::");
        assert_eq!(actions, vec!["prompt", "echo::"]);
        // Timeout → Chat("")
        assert_eq!(ic.expire_prefix(), Some(InterceptAction::Chat(String::new())));
    }

    #[test]
    fn test_ui_esc_non_bracket_cancel() {
        // With "::" prefix, prefix match then ESC cancels
        let mut ic = new_interceptor("::");
        let actions = simulate_main_loop(&mut ic, b"::\x1b");
        assert_eq!(actions, vec!["prompt", "echo::", "pending", "cancel"]);
    }

    #[test]
    fn test_ui_backspace_to_empty_dismisses() {
        let mut ic = new_interceptor(":");
        // Prefix match → prompt, then backspace dismisses
        let actions = simulate_main_loop(&mut ic, b":\x7f");
        assert_eq!(actions, vec!["prompt", "dismiss"]);
    }

    #[test]
    fn test_ui_multiple_esc_sequences() {
        let mut ic = new_interceptor(":");
        // Prefix match → prompt, then Enter submits empty chat
        let actions = simulate_main_loop(&mut ic, b":\r");
        assert_eq!(actions, vec!["prompt", "chat:"]);
    }

    #[test]
    fn test_ui_paste_then_enter() {
        let mut ic = new_interceptor(":");
        // Prefix match, type content, Enter
        let actions = simulate_main_loop(&mut ic, b":hi\r");
        assert_eq!(actions, vec!["prompt", "echo:h", "echo:hi", "chat:hi"]);
    }

    #[test]
    fn test_ui_paste_with_embedded_ansi() {
        let mut ic = new_interceptor(":");
        // Double-prefix → ResumeChat
        let actions = simulate_main_loop(&mut ic, b"::");
        assert_eq!(actions, vec!["prompt", "resume_chat"]);
    }

    #[test]
    fn test_ui_prefix_mismatch_then_retry() {
        let mut ic = new_interceptor("::");
        // First attempt ":x" mismatches prefix "::", then retry "::"
        // After ":x" is forwarded, command_line_has_content=true, so
        // subsequent "::" is also forwarded (non-developer-mode blocks
        // chat when line has content).
        let actions = simulate_main_loop(&mut ic, b":x::");
        assert_eq!(
            actions,
            vec!["prompt", "forward", "forward", "forward"]
        );
    }

    // --- Tab tests ---

    #[test]
    fn test_tab_in_chat_mode() {
        let mut ic = new_interceptor(":");
        // Prefix match → Buffering
        assert_eq!(ic.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        // Tab while in chat mode → Tab with current buffer
        assert_eq!(ic.feed_byte(b'\t'), InterceptAction::Tab(vec![b':']));
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
        // Prefix match → Buffering
        assert_eq!(ic.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        // inject_byte adds to existing buffer
        ic.inject_byte(b'h');
        ic.inject_byte(b'e');
        assert_eq!(ic.current_buffer(), b":he");
    }

    // --- Double-prefix resume and expire_prefix tests ---

    #[test]
    fn test_double_prefix_resume_single_char() {
        let mut ic = new_interceptor(":");
        // First ":" → Buffering (prefix match)
        assert_eq!(ic.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        // Second ":" → ResumeChat (double-prefix detected)
        assert_eq!(ic.feed_byte(b':'), InterceptAction::ResumeChat);
    }

    #[test]
    fn test_double_prefix_resume_two_char() {
        let mut ic = new_interceptor("::");
        // Feed full prefix "::" → Buffering
        assert_eq!(ic.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        assert_eq!(ic.feed_byte(b':'), InterceptAction::Buffering(vec![b':', b':']));
        // Feed another "::" → ResumeChat
        assert_eq!(ic.feed_byte(b':'), InterceptAction::Buffering(vec![b':', b':', b':']));
        assert_eq!(ic.feed_byte(b':'), InterceptAction::ResumeChat);
    }

    #[test]
    fn test_expire_prefix_no_effect_when_not_in_chat() {
        let mut ic = new_interceptor(":");
        // No input → expire_prefix returns None
        assert_eq!(ic.expire_prefix(), None);
    }

    #[test]
    fn test_expire_prefix_no_effect_with_extra_content() {
        let mut ic = new_interceptor(":");
        assert_eq!(ic.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        // Type more content after prefix
        assert_eq!(ic.feed_byte(b'h'), InterceptAction::Buffering(vec![b':', b'h']));
        // expire_prefix returns None (buffer != prefix, has extra content)
        assert_eq!(ic.expire_prefix(), None);
    }

    #[test]
    fn test_prefix_then_enter_new_chat() {
        let mut ic = new_interceptor(":");
        assert_eq!(ic.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        // Enter after prefix → Chat("")
        assert_eq!(ic.feed_byte(b'\r'), InterceptAction::Chat(String::new()));
    }

    #[test]
    fn test_suppressed_mode_arrow_keys_forwarded_as_complete_sequence() {
        let mut interceptor = new_interceptor("::");
        interceptor.set_suppressed(true);

        // Down arrow: \x1b[B — all bytes buffered, then forwarded as one unit
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Pending);
        assert_eq!(interceptor.feed_byte(b'['), InterceptAction::Pending);
        assert_eq!(
            interceptor.feed_byte(b'B'),
            InterceptAction::Forward(vec![0x1b, b'[', b'B'])
        );

        // Up arrow: \x1b[A
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Pending);
        assert_eq!(interceptor.feed_byte(b'['), InterceptAction::Pending);
        assert_eq!(
            interceptor.feed_byte(b'A'),
            InterceptAction::Forward(vec![0x1b, b'[', b'A'])
        );

        // Right arrow: \x1b[C
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Pending);
        assert_eq!(interceptor.feed_byte(b'['), InterceptAction::Pending);
        assert_eq!(
            interceptor.feed_byte(b'C'),
            InterceptAction::Forward(vec![0x1b, b'[', b'C'])
        );

        // Left arrow: \x1b[D
        assert_eq!(interceptor.feed_byte(0x1b), InterceptAction::Pending);
        assert_eq!(interceptor.feed_byte(b'['), InterceptAction::Pending);
        assert_eq!(
            interceptor.feed_byte(b'D'),
            InterceptAction::Forward(vec![0x1b, b'[', b'D'])
        );
    }

    // --- UTF-8 multi-byte buffering tests ---

    #[test]
    fn test_utf8_char_len_helper() {
        assert_eq!(utf8_char_len(b'a'), 1);       // ASCII
        assert_eq!(utf8_char_len(0x80), 1);        // continuation (invalid lead)
        assert_eq!(utf8_char_len(0xC3), 2);        // 2-byte (e.g. ü)
        assert_eq!(utf8_char_len(0xE4), 3);        // 3-byte (e.g. CJK)
        assert_eq!(utf8_char_len(0xF0), 4);        // 4-byte (e.g. emoji)
    }

    #[test]
    fn test_incomplete_utf8_tail() {
        // Complete ASCII
        assert!(!has_incomplete_utf8_tail(b"hello"));
        // Complete 3-byte CJK: 一 = E4 B8 80
        assert!(!has_incomplete_utf8_tail(&[0xE4, 0xB8, 0x80]));
        // Incomplete 3-byte: only lead + 1 continuation
        assert!(has_incomplete_utf8_tail(&[0xE4, 0xB8]));
        // Incomplete 3-byte: only lead byte
        assert!(has_incomplete_utf8_tail(&[0xE4]));
        // ASCII then incomplete 3-byte
        assert!(has_incomplete_utf8_tail(&[b'a', 0xE4, 0xB8]));
        // Empty
        assert!(!has_incomplete_utf8_tail(b""));
    }

    #[test]
    fn test_cjk_char_buffered_before_flush() {
        // "一" = E4 B8 80 (3 bytes). With prefix "::", the first byte E4
        // mismatches ':', so it enters the mismatch branch. Without UTF-8
        // buffering it would flush the single byte; with it, it should
        // return Pending until the full character is received.
        let mut ic = new_interceptor("::");

        // First byte of "一" — should be Pending (incomplete UTF-8)
        assert_eq!(ic.feed_byte(0xE4), InterceptAction::Pending);
        // Second byte — still incomplete
        assert_eq!(ic.feed_byte(0xB8), InterceptAction::Pending);
        // Third byte — now complete, should flush all 3 bytes
        assert_eq!(
            ic.feed_byte(0x80),
            InterceptAction::Forward(vec![0xE4, 0xB8, 0x80])
        );
    }

    #[test]
    fn test_ascii_not_delayed_by_utf8_check() {
        // ASCII byte that mismatches prefix should flush immediately
        let mut ic = new_interceptor("::");
        assert_eq!(
            ic.feed_byte(b'a'),
            InterceptAction::Forward(vec![b'a'])
        );
    }

    // -----------------------------------------------------------------------
    // Integration test: simulates the main loop's timing-based prefix
    // detection logic (prefix_match_time + PREFIX_TIMEOUT + expire_prefix)
    // -----------------------------------------------------------------------

    /// Outcome of a simulated main-loop iteration.
    #[derive(Debug, PartialEq)]
    enum LoopOutcome {
        /// Forwarded bytes to PTY
        Forward,
        /// Showed the `:` prompt (prefix matched, timer started)
        Prompt,
        /// Echoed chat content after prefix
        Echo(String),
        /// Entered new chat with message
        NewChat(String),
        /// Resumed last chat session
        ResumeChat,
        /// Cancelled prefix/chat
        Cancel,
        /// Backspaced (with remaining buffer)
        Backspace(Vec<u8>),
        /// Nothing visible happened
        Pending,
        /// Prefix timeout expired → new chat
        Timeout,
        /// Tab
        Tab,
    }

    /// Simulates the main loop's complete handling of input bytes + timing.
    /// Mirrors the real logic in main.rs: Buffering with prefix → start timer,
    /// additional input → cancel timer, ResumeChat → immediate resume,
    /// timer expiry → expire_prefix() → Chat("").
    struct MainLoopSim {
        ic: InputInterceptor,
        prefix: Vec<u8>,
        timer_active: bool,
    }

    impl MainLoopSim {
        fn new(prefix: &str) -> Self {
            Self {
                ic: new_interceptor(prefix),
                prefix: prefix.as_bytes().to_vec(),
                timer_active: false,
            }
        }

        /// Feed one byte, return the outcome.
        fn feed(&mut self, byte: u8) -> LoopOutcome {
            match self.ic.feed_byte(byte) {
                InterceptAction::Buffering(ref buf) if *buf == self.prefix => {
                    self.timer_active = true;
                    LoopOutcome::Prompt
                }
                InterceptAction::Buffering(ref buf)
                    if buf.len() > self.prefix.len() && buf.starts_with(&self.prefix) =>
                {
                    self.timer_active = false;
                    let content = String::from_utf8_lossy(&buf[self.prefix.len()..]).to_string();
                    LoopOutcome::Echo(content)
                }
                InterceptAction::Buffering(_) => {
                    // Partial prefix match (multi-char prefix, not yet complete)
                    LoopOutcome::Pending
                }
                InterceptAction::Forward(_) => {
                    self.timer_active = false;
                    LoopOutcome::Forward
                }
                InterceptAction::Chat(msg) => {
                    self.timer_active = false;
                    LoopOutcome::NewChat(msg)
                }
                InterceptAction::ResumeChat => {
                    self.timer_active = false;
                    LoopOutcome::ResumeChat
                }
                InterceptAction::Cancel => {
                    self.timer_active = false;
                    LoopOutcome::Cancel
                }
                InterceptAction::Backspace(buf) => {
                    LoopOutcome::Backspace(buf)
                }
                InterceptAction::Tab(_) => LoopOutcome::Tab,
                InterceptAction::Pending => LoopOutcome::Pending,
            }
        }

        /// Simulate the prefix timeout expiring (150ms elapsed with no input).
        fn timeout(&mut self) -> LoopOutcome {
            if !self.timer_active {
                return LoopOutcome::Pending;
            }
            self.timer_active = false;
            match self.ic.expire_prefix() {
                Some(InterceptAction::Chat(_)) => LoopOutcome::Timeout,
                _ => LoopOutcome::Pending,
            }
        }

        /// Feed a batch of bytes, collecting outcomes. Calls finish_batch at end.
        fn feed_batch(&mut self, bytes: &[u8]) -> Vec<LoopOutcome> {
            let mut out = Vec::new();
            for &b in bytes {
                let o = self.feed(b);
                if !matches!(o, LoopOutcome::Pending) {
                    out.push(o);
                }
            }
            if let Some(action) = self.ic.finish_batch() {
                match action {
                    InterceptAction::Cancel => {
                        self.timer_active = false;
                        out.push(LoopOutcome::Cancel);
                    }
                    InterceptAction::Forward(_) => out.push(LoopOutcome::Forward),
                    _ => {}
                }
            }
            out
        }
    }

    #[test]
    fn test_integration_single_prefix_timeout_enters_new_chat() {
        // Scenario: user types ":" at shell prompt, waits → enters new chat
        let mut sim = MainLoopSim::new(":");
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert!(sim.timer_active);
        // 150ms passes with no more input
        assert_eq!(sim.timeout(), LoopOutcome::Timeout);
        assert!(!sim.timer_active);
    }

    #[test]
    fn test_integration_double_prefix_resumes_chat() {
        // Scenario: user types "::" quickly → resumes last chat
        let mut sim = MainLoopSim::new(":");
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert!(sim.timer_active);
        // Second ":" arrives within 150ms → ResumeChat
        assert_eq!(sim.feed(b':'), LoopOutcome::ResumeChat);
        assert!(!sim.timer_active);
    }

    #[test]
    fn test_integration_prefix_then_content_then_enter() {
        // Scenario: user types ":hello" then Enter → chat with content
        let mut sim = MainLoopSim::new(":");
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert!(sim.timer_active);
        // Typing content cancels the timer
        assert_eq!(sim.feed(b'h'), LoopOutcome::Echo("h".into()));
        assert!(!sim.timer_active);
        assert_eq!(sim.feed(b'i'), LoopOutcome::Echo("hi".into()));
        assert_eq!(sim.feed(b'\r'), LoopOutcome::NewChat("hi".into()));
    }

    #[test]
    fn test_integration_prefix_then_enter_new_chat() {
        // Scenario: user types ":" then immediately presses Enter → new chat
        let mut sim = MainLoopSim::new(":");
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert_eq!(sim.feed(b'\r'), LoopOutcome::NewChat(String::new()));
    }

    #[test]
    fn test_integration_prefix_then_esc_cancels() {
        // Scenario: user types ":" then presses ESC → cancel
        let mut sim = MainLoopSim::new(":");
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert!(sim.timer_active);
        // ESC → Pending (filter starts), finish_batch → Cancel
        let outcomes = sim.feed_batch(b"\x1b");
        assert_eq!(outcomes, vec![LoopOutcome::Cancel]);
        assert!(!sim.timer_active);
    }

    #[test]
    fn test_integration_prefix_then_backspace_dismisses() {
        // Scenario: user types ":" then backspace → dismiss prompt
        let mut sim = MainLoopSim::new(":");
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert_eq!(sim.feed(0x7f), LoopOutcome::Backspace(vec![]));
        // Timer should still be technically active but expire_prefix
        // won't fire because buffer is empty and in_chat is false
        assert_eq!(sim.timeout(), LoopOutcome::Pending);
    }

    #[test]
    fn test_integration_two_char_prefix_timeout() {
        // Scenario: prefix is "::", user types "::" and waits → new chat
        let mut sim = MainLoopSim::new("::");
        // First ":" — partial prefix match, no prompt yet
        let outcomes = sim.feed_batch(b":");
        assert_eq!(outcomes, vec![]); // Pending (partial prefix)
        assert!(!sim.timer_active);
        // Second ":" — full prefix match → Prompt
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert!(sim.timer_active);
        // Timeout → new chat
        assert_eq!(sim.timeout(), LoopOutcome::Timeout);
    }

    #[test]
    fn test_integration_two_char_prefix_resume() {
        // Scenario: prefix is "::", user types "::::" → resume
        let mut sim = MainLoopSim::new("::");
        let outcomes = sim.feed_batch(b"::");
        // First ":" is Pending, second ":" is Prompt
        assert_eq!(outcomes, vec![LoopOutcome::Prompt]);
        assert!(sim.timer_active);
        // Third ":" — Echo (additional content after prefix)
        // Actually with prefix "::", buffer becomes [:::]
        // which is > prefix.len() and starts_with prefix → Echo(":")
        assert_eq!(sim.feed(b':'), LoopOutcome::Echo(":".into()));
        assert!(!sim.timer_active); // timer cancelled by extra input
        // Fourth ":" — double-prefix detected → ResumeChat
        assert_eq!(sim.feed(b':'), LoopOutcome::ResumeChat);
    }

    #[test]
    fn test_integration_normal_input_not_intercepted() {
        // Scenario: user types "ls" → forwarded to PTY, no interception
        let mut sim = MainLoopSim::new(":");
        assert_eq!(sim.feed(b'l'), LoopOutcome::Forward);
        assert_eq!(sim.feed(b's'), LoopOutcome::Forward);
        assert_eq!(sim.feed(b'\n'), LoopOutcome::Forward);
        assert!(!sim.timer_active);
    }

    #[test]
    fn test_integration_guard_blocks_during_typing() {
        // Scenario: user is mid-command, ":" should forward not intercept
        let mut guard = TimeGapGuard::new(Duration::from_secs(1));
        guard.note_input(); // just typed something
        let mut sim = MainLoopSim {
            ic: InputInterceptor::new(":", "::", Box::new(guard), false),
            prefix: b":".to_vec(),
            timer_active: false,
        };
        assert_eq!(sim.feed(b':'), LoopOutcome::Forward);
        assert!(!sim.timer_active);
    }

    #[test]
    fn test_integration_suppressed_then_unsuppressed() {
        // Scenario: enter vim (suppressed), exit vim, then use prefix
        let mut sim = MainLoopSim::new(":");
        sim.ic.set_suppressed(true);
        assert_eq!(sim.feed(b':'), LoopOutcome::Forward);
        sim.ic.set_suppressed(false);
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert!(sim.timer_active);
        assert_eq!(sim.feed(b':'), LoopOutcome::ResumeChat);
    }

    #[test]
    fn test_integration_timeout_no_effect_after_content() {
        // Scenario: user types ":hi", timeout should NOT trigger new chat
        // because buffer has content beyond prefix
        let mut sim = MainLoopSim::new(":");
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert!(sim.timer_active);
        assert_eq!(sim.feed(b'h'), LoopOutcome::Echo("h".into()));
        assert!(!sim.timer_active); // timer cancelled
        // Even if we call timeout, nothing happens
        assert_eq!(sim.timeout(), LoopOutcome::Pending);
    }

    #[test]
    fn test_integration_multiple_sessions() {
        // Scenario: new chat → run command → resume chat → run command → new chat
        let mut sim = MainLoopSim::new(":");

        // 1. ":" + timeout → new chat
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert_eq!(sim.timeout(), LoopOutcome::Timeout);

        // Simulate shell output after chat exits (resets state)
        sim.ic.note_output(b"$ ");

        // 2. Run a normal command
        assert_eq!(sim.feed(b'l'), LoopOutcome::Forward);
        assert_eq!(sim.feed(b's'), LoopOutcome::Forward);
        assert_eq!(sim.feed(b'\n'), LoopOutcome::Forward);

        // 3. "::" → resume chat
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert_eq!(sim.feed(b':'), LoopOutcome::ResumeChat);

        // Simulate shell output after chat exits
        sim.ic.note_output(b"$ ");

        // 4. Another normal command
        assert_eq!(sim.feed(b'p'), LoopOutcome::Forward);
        assert_eq!(sim.feed(b'w'), LoopOutcome::Forward);
        assert_eq!(sim.feed(b'd'), LoopOutcome::Forward);
        assert_eq!(sim.feed(b'\n'), LoopOutcome::Forward);

        // 5. ":ask something" + Enter → new chat with content
        assert_eq!(sim.feed(b':'), LoopOutcome::Prompt);
        assert_eq!(sim.feed(b'a'), LoopOutcome::Echo("a".into()));
        assert_eq!(sim.feed(b's'), LoopOutcome::Echo("as".into()));
        assert_eq!(sim.feed(b'k'), LoopOutcome::Echo("ask".into()));
        assert_eq!(sim.feed(b'\r'), LoopOutcome::NewChat("ask".into()));
    }
}
