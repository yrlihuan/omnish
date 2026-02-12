use std::collections::VecDeque;

#[derive(Debug, PartialEq)]
pub enum InterceptAction {
    /// Pass through normally
    PassThrough,
    /// Command detected and consumed: (command_string, full_buffered_input)
    Command(String, Vec<u8>),
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
                return InterceptAction::PassThrough;
            } else {
                // Prefix mismatch, flush buffer
                self.buffer.clear();
            }
        }

        InterceptAction::PassThrough
    }

    fn handle_enter(&mut self) -> InterceptAction {
        let buffered: Vec<u8> = self.buffer.iter().copied().collect();
        self.buffer.clear();

        if !self.in_command {
            self.in_command = false;
            return InterceptAction::PassThrough;
        }

        // Extract command after prefix
        if buffered.len() > self.prefix.len() {
            let cmd_bytes = &buffered[self.prefix.len()..buffered.len() - 1]; // exclude final \n
            if let Ok(cmd_str) = std::str::from_utf8(cmd_bytes) {
                self.in_command = false;
                return InterceptAction::Command(cmd_str.to_string(), buffered);
            }
        }

        self.in_command = false;
        InterceptAction::PassThrough
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_passthrough_normal_input() {
        let mut interceptor = InputInterceptor::new("::");
        assert_eq!(interceptor.feed_byte(b'l'), InterceptAction::PassThrough);
        assert_eq!(interceptor.feed_byte(b's'), InterceptAction::PassThrough);
        assert_eq!(interceptor.feed_byte(b'\n'), InterceptAction::PassThrough);
    }

    #[test]
    fn test_command_detected() {
        let mut interceptor = InputInterceptor::new("::");
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::PassThrough);
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::PassThrough);
        assert_eq!(interceptor.feed_byte(b'a'), InterceptAction::PassThrough);
        assert_eq!(interceptor.feed_byte(b's'), InterceptAction::PassThrough);
        assert_eq!(interceptor.feed_byte(b'k'), InterceptAction::PassThrough);

        if let InterceptAction::Command(cmd, buf) = interceptor.feed_byte(b'\n') {
            assert_eq!(cmd, "ask");
            assert_eq!(buf, b"::ask\n");
        } else {
            panic!("Expected Command action");
        }
    }

    #[test]
    fn test_command_with_query() {
        let mut interceptor = InputInterceptor::new("::");
        for &byte in b"::ask why did this fail\n" {
            let action = interceptor.feed_byte(byte);
            if byte == b'\n' {
                if let InterceptAction::Command(cmd, _) = action {
                    assert_eq!(cmd, "ask why did this fail");
                } else {
                    panic!("Expected Command action");
                }
            }
        }
    }

    #[test]
    fn test_partial_prefix_then_mismatch() {
        let mut interceptor = InputInterceptor::new("::");
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::PassThrough);
        assert_eq!(interceptor.feed_byte(b'x'), InterceptAction::PassThrough);
        // Buffer should be cleared after mismatch
        assert_eq!(interceptor.feed_byte(b'\n'), InterceptAction::PassThrough);
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
}
