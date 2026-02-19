#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc133EventKind {
    PromptStart,
    CommandStart,
    OutputStart,
    CommandEnd { exit_code: i32 },
}

#[derive(Debug, Clone)]
pub struct Osc133Event {
    pub kind: Osc133EventKind,
    /// Byte offset in the input where this OSC 133 sequence starts.
    pub start: usize,
    /// Byte offset in the input where this sequence ends (exclusive).
    pub end: usize,
}

/// A byte-level state machine that parses OSC 133 sequences from PTY output.
///
/// Recognized sequences:
/// - `\x1b]133;A\x07` -> PromptStart
/// - `\x1b]133;B\x07` -> CommandStart
/// - `\x1b]133;C\x07` -> OutputStart
/// - `\x1b]133;D;{exit_code}\x07` -> CommandEnd { exit_code }
pub struct Osc133Detector {
    buf: Vec<u8>,
    in_osc: bool,
    /// Tracks how many bytes from previous feed() calls are in buf (for offset calculation).
    carried_len: usize,
}

impl Osc133Detector {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            in_osc: false,
            carried_len: 0,
        }
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Osc133Event> {
        let mut events = Vec::new();

        for (i, &byte) in data.iter().enumerate() {
            if !self.in_osc {
                if byte == 0x1b {
                    // Potential start of an escape sequence
                    self.buf.clear();
                    self.buf.push(byte);
                    self.carried_len = 0;
                    // Record the start offset relative to current data
                    // We store the index in data where ESC appeared
                    self.in_osc = true;
                }
            } else {
                self.buf.push(byte);

                if byte == 0x07 {
                    // End of OSC sequence — try to parse
                    let seq_start = i + 1 - (self.buf.len() - self.carried_len);
                    let seq_end = i + 1;

                    if let Some(kind) = Self::parse_osc133(&self.buf) {
                        events.push(Osc133Event {
                            kind,
                            start: seq_start,
                            end: seq_end,
                        });
                    }
                    self.buf.clear();
                    self.carried_len = 0;
                    self.in_osc = false;
                } else if byte == 0x1b {
                    // New ESC inside an incomplete OSC — restart
                    // The previous partial sequence is discarded.
                    self.buf.clear();
                    self.buf.push(byte);
                    self.carried_len = 0;
                }
            }
        }

        // If we are still in_osc at end, buf holds a partial sequence.
        // carried_len = how many bytes of buf came from this feed call's data
        // that are now carried over to the next feed.
        if self.in_osc {
            // Everything currently in buf that came from *this* feed is carried.
            // carried_len tracks bytes from *previous* feeds already in buf.
            // After this feed, all buf bytes become carried for the next feed.
            self.carried_len = self.buf.len();
        }

        events
    }

    fn parse_osc133(buf: &[u8]) -> Option<Osc133EventKind> {
        // Expected format: ESC ] 1 3 3 ; <payload> BEL
        // Minimum: \x1b ] 1 3 3 ; X \x07 = 8 bytes
        if buf.len() < 8 {
            return None;
        }
        if buf[0] != 0x1b || buf[1] != b']' {
            return None;
        }
        if &buf[2..6] != b"133;" {
            return None;
        }
        // Last byte should be BEL
        if *buf.last()? != 0x07 {
            return None;
        }

        let payload = &buf[6..buf.len() - 1]; // between "133;" and BEL

        match payload {
            b"A" => Some(Osc133EventKind::PromptStart),
            b"B" => Some(Osc133EventKind::CommandStart),
            b"C" => Some(Osc133EventKind::OutputStart),
            _ => {
                // Check for D;{exit_code}
                if payload.len() >= 2 && payload[0] == b'D' && payload[1] == b';' {
                    let code_bytes = &payload[2..];
                    let code_str = std::str::from_utf8(code_bytes).ok()?;
                    let exit_code = code_str.parse::<i32>().ok()?;
                    Some(Osc133EventKind::CommandEnd { exit_code })
                } else {
                    None
                }
            }
        }
    }
}

/// Strip all OSC 133 sequences from a byte buffer.
pub fn strip_osc133(data: &[u8]) -> Vec<u8> {
    let mut detector = Osc133Detector::new();
    let events = detector.feed(data);

    if events.is_empty() {
        return data.to_vec();
    }

    let mut result = Vec::with_capacity(data.len());
    let mut pos = 0;
    for event in &events {
        if pos < event.start {
            result.extend_from_slice(&data[pos..event.start]);
        }
        pos = event.end;
    }
    if pos < data.len() {
        result.extend_from_slice(&data[pos..]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_start() {
        let mut detector = Osc133Detector::new();
        let events = detector.feed(b"\x1b]133;A\x07");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, Osc133EventKind::PromptStart);
        assert_eq!(events[0].start, 0);
        assert_eq!(events[0].end, 8);
    }

    #[test]
    fn test_command_start() {
        let mut detector = Osc133Detector::new();
        let events = detector.feed(b"\x1b]133;B\x07");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, Osc133EventKind::CommandStart);
    }

    #[test]
    fn test_output_start() {
        let mut detector = Osc133Detector::new();
        let events = detector.feed(b"\x1b]133;C\x07");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, Osc133EventKind::OutputStart);
    }

    #[test]
    fn test_command_end_with_exit_code() {
        let mut detector = Osc133Detector::new();
        let events = detector.feed(b"\x1b]133;D;0\x07");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, Osc133EventKind::CommandEnd { exit_code: 0 });
    }

    #[test]
    fn test_command_end_nonzero() {
        let mut detector = Osc133Detector::new();
        let events = detector.feed(b"\x1b]133;D;127\x07");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].kind,
            Osc133EventKind::CommandEnd { exit_code: 127 }
        );
    }

    #[test]
    fn test_sequence_split_across_chunks() {
        let mut detector = Osc133Detector::new();
        let events1 = detector.feed(b"\x1b]133;");
        assert_eq!(events1.len(), 0);
        let events2 = detector.feed(b"A\x07");
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].kind, Osc133EventKind::PromptStart);
        // In the second chunk, the sequence completes at byte index 2 (end exclusive)
        assert_eq!(events2[0].start, 0);
        assert_eq!(events2[0].end, 2);
    }

    #[test]
    fn test_embedded_in_output() {
        let mut detector = Osc133Detector::new();
        // "some output" = 11 bytes, then 8-byte OSC, then "more"
        let events = detector.feed(b"some output\x1b]133;A\x07more");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, Osc133EventKind::PromptStart);
        assert_eq!(events[0].start, 11);
        assert_eq!(events[0].end, 19);
    }

    #[test]
    fn test_multiple_events_in_one_chunk() {
        let mut detector = Osc133Detector::new();
        let events = detector.feed(b"\x1b]133;A\x07\x1b]133;B\x07");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, Osc133EventKind::PromptStart);
        assert_eq!(events[1].kind, Osc133EventKind::CommandStart);
        assert_eq!(events[0].start, 0);
        assert_eq!(events[0].end, 8);
        assert_eq!(events[1].start, 8);
        assert_eq!(events[1].end, 16);
    }

    #[test]
    fn test_ignores_other_osc_sequences() {
        let mut detector = Osc133Detector::new();
        let events = detector.feed(b"\x1b]0;title\x07");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_strip_sequences() {
        let input = b"hello\x1b]133;A\x07world\x1b]133;B\x07end";
        let result = strip_osc133(input);
        assert_eq!(result, b"helloworldend");
    }
}
