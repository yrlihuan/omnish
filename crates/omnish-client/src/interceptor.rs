use std::collections::VecDeque;

#[derive(Debug, PartialEq)]
pub enum InterceptAction {
    /// Buffering input, don't send to PTY yet
    /// Contains current buffer for echo display
    Buffering(Vec<u8>),
    /// Forward these bytes to PTY
    Forward(Vec<u8>),
    /// Command detected and consumed: (command_string)
    Command(String),
    /// Backspace in buffering mode - erased one char
    /// Contains updated buffer for echo display
    Backspace(Vec<u8>),
}

pub struct InputInterceptor {
    prefix: Vec<u8>,
    buffer: VecDeque<u8>,
    in_command: bool,
}

impl InputInterceptor {
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.as_bytes().to_vec(),
            buffer: VecDeque::new(),
            in_command: false,
        }
    }

    /// Note output from shell (to detect prompt and reset state)
    pub fn note_output(&mut self, _data: &[u8]) {
        // On any output from shell, reset command state
        // This handles Ctrl+C, Ctrl+D, etc. that cancel partial input
        if self.in_command {
            self.in_command = false;
            self.buffer.clear();
        }
    }

    /// Feed a single input byte, returns action
    pub fn feed_byte(&mut self, byte: u8) -> InterceptAction {
        // Handle backspace/delete
        if byte == 0x7f || byte == 0x08 {
            // If we're buffering or in command mode, handle backspace
            if !self.buffer.is_empty() && (self.in_command || self.buffer.len() <= self.prefix.len()) {
                self.buffer.pop_back();

                // Check if we dropped out of command mode
                if self.in_command && self.buffer.len() < self.prefix.len() {
                    self.in_command = false;
                }

                let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
                return InterceptAction::Backspace(current_buf);
            } else {
                // Not buffering, forward to PTY
                return InterceptAction::Forward(vec![byte]);
            }
        }

        self.buffer.push_back(byte);

        // Check for Enter/newline
        if byte == b'\n' || byte == b'\r' {
            return self.handle_enter();
        }

        // Check if buffer matches prefix so far
        if !self.in_command && self.buffer.len() <= self.prefix.len() {
            if self.buffer.iter().copied().collect::<Vec<_>>() == self.prefix[..self.buffer.len()] {
                // Still matching prefix
                if self.buffer.len() == self.prefix.len() {
                    // Complete prefix match
                    self.in_command = true;
                }
                // Keep buffering, don't send to PTY yet, return buffer for echo
                let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
                return InterceptAction::Buffering(current_buf);
            } else {
                // Prefix mismatch, flush buffer to PTY
                let flushed: Vec<u8> = self.buffer.iter().copied().collect();
                self.buffer.clear();
                return InterceptAction::Forward(flushed);
            }
        }

        // In command mode, keep buffering and return for echo
        if self.in_command {
            let current_buf: Vec<u8> = self.buffer.iter().copied().collect();
            return InterceptAction::Buffering(current_buf);
        }

        // Not in command mode and buffer exceeded prefix length - flush and reset
        let flushed: Vec<u8> = self.buffer.iter().copied().collect();
        self.buffer.clear();
        InterceptAction::Forward(flushed)
    }

    fn handle_enter(&mut self) -> InterceptAction {
        let buffered: Vec<u8> = self.buffer.iter().copied().collect();
        self.buffer.clear();

        if !self.in_command {
            // Not a command, forward to PTY
            return InterceptAction::Forward(buffered);
        }

        // Extract command after prefix
        self.in_command = false;
        if buffered.len() > self.prefix.len() {
            let cmd_bytes = &buffered[self.prefix.len()..buffered.len() - 1]; // exclude final \n
            if let Ok(cmd_str) = std::str::from_utf8(cmd_bytes) {
                return InterceptAction::Command(cmd_str.to_string());
            }
        }

        // Empty command or decode error, forward to PTY
        InterceptAction::Forward(buffered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_passthrough_normal_input() {
        let mut interceptor = InputInterceptor::new("::");
        // 'l' doesn't match prefix, so buffer is flushed
        assert_eq!(interceptor.feed_byte(b'l'), InterceptAction::Forward(vec![b'l']));
        assert_eq!(interceptor.feed_byte(b's'), InterceptAction::Forward(vec![b's']));
        assert_eq!(interceptor.feed_byte(b'\n'), InterceptAction::Forward(vec![b'\n']));
    }

    #[test]
    fn test_command_detected() {
        let mut interceptor = InputInterceptor::new("::");
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':', b':']));
        assert_eq!(interceptor.feed_byte(b'a'), InterceptAction::Buffering(vec![b':', b':', b'a']));
        assert_eq!(interceptor.feed_byte(b's'), InterceptAction::Buffering(vec![b':', b':', b'a', b's']));
        assert_eq!(interceptor.feed_byte(b'k'), InterceptAction::Buffering(vec![b':', b':', b'a', b's', b'k']));

        if let InterceptAction::Command(cmd) = interceptor.feed_byte(b'\n') {
            assert_eq!(cmd, "ask");
        } else {
            panic!("Expected Command action");
        }
    }

    #[test]
    fn test_command_with_query() {
        let mut interceptor = InputInterceptor::new("::");
        let input = b"::ask why did this fail\n";
        for (idx, &byte) in input.iter().enumerate() {
            let action = interceptor.feed_byte(byte);
            if byte == b'\n' {
                if let InterceptAction::Command(cmd) = action {
                    assert_eq!(cmd, "ask why did this fail");
                } else {
                    panic!("Expected Command action");
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
        let mut interceptor = InputInterceptor::new("::");
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        // Mismatch - should flush ":x"
        assert_eq!(interceptor.feed_byte(b'x'), InterceptAction::Forward(vec![b':', b'x']));
        // New input after buffer cleared
        assert_eq!(interceptor.feed_byte(b'\n'), InterceptAction::Forward(vec![b'\n']));
    }

    #[test]
    fn test_note_output_resets_command_state() {
        let mut interceptor = InputInterceptor::new("::");
        interceptor.feed_byte(b':');
        interceptor.feed_byte(b':');
        // Now in command mode
        interceptor.note_output(b"some output");
        // Command state should be reset
        assert_eq!(interceptor.in_command, false);
        assert_eq!(interceptor.buffer.len(), 0);
    }

    #[test]
    fn test_backspace_in_command_mode() {
        let mut interceptor = InputInterceptor::new("::");
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':', b':']));
        assert_eq!(interceptor.feed_byte(b'a'), InterceptAction::Buffering(vec![b':', b':', b'a']));

        // Backspace should remove 'a'
        assert_eq!(interceptor.feed_byte(0x7f), InterceptAction::Backspace(vec![b':', b':']));

        // Still in command mode
        assert_eq!(interceptor.feed_byte(b'b'), InterceptAction::Buffering(vec![b':', b':', b'b']));
    }

    #[test]
    fn test_backspace_out_of_command_mode() {
        let mut interceptor = InputInterceptor::new("::");
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
        let mut interceptor = InputInterceptor::new("::");
        // Type something that doesn't match prefix
        interceptor.feed_byte(b'l');

        // Backspace should be forwarded to PTY
        assert_eq!(interceptor.feed_byte(0x7f), InterceptAction::Forward(vec![0x7f]));
    }
}
