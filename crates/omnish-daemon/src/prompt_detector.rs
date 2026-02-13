use regex::Regex;

const DEFAULT_PATTERNS: &[&str] = &[r"[\$#%❯]\s*$"];

#[derive(Debug, Clone)]
pub struct PromptEvent {
    pub line_start_offset: usize,
}

pub struct PromptDetector {
    patterns: Vec<Regex>,
    line_buf: Vec<u8>,
}

impl PromptDetector {
    pub fn new() -> Self {
        let patterns = DEFAULT_PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("invalid default prompt pattern"))
            .collect();
        Self {
            patterns,
            line_buf: Vec::new(),
        }
    }

    pub fn with_patterns(patterns: Vec<String>) -> Self {
        let patterns = patterns
            .iter()
            .map(|p| Regex::new(p).expect("invalid prompt pattern"))
            .collect();
        Self {
            patterns,
            line_buf: Vec::new(),
        }
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<PromptEvent> {
        let mut events = Vec::new();
        let mut line_start_offset = 0;

        for (i, &byte) in data.iter().enumerate() {
            self.line_buf.push(byte);
            if byte == b'\n' {
                self.line_buf.clear();
                line_start_offset = i + 1;
            }
        }

        if !self.line_buf.is_empty() && self.is_prompt() {
            events.push(PromptEvent { line_start_offset });
            self.line_buf.clear();
        }

        events
    }

    fn is_prompt(&self) -> bool {
        let stripped = strip_ansi(&self.line_buf);
        let text = match std::str::from_utf8(&stripped) {
            Ok(s) => s,
            Err(_) => return false,
        };

        // Require at least 1 non-whitespace char to avoid matching blank lines
        if text.chars().filter(|c| !c.is_whitespace()).count() < 1 {
            return false;
        }

        self.patterns.iter().any(|p| p.is_match(text))
    }
}

fn strip_ansi(data: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == b'[' {
            // Skip ESC [ ... <final byte>
            i += 2;
            while i < data.len() {
                let b = data[i];
                i += 1;
                // Final byte of CSI sequence is in range 0x40-0x7E
                if (0x40..=0x7E).contains(&b) {
                    break;
                }
            }
        } else {
            result.push(data[i]);
            i += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_dollar_prompt() {
        let mut detector = PromptDetector::new();
        let events = detector.feed(b"total 0\r\nfile.txt\r\nuser@host:~$ ");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_detect_hash_prompt() {
        let mut detector = PromptDetector::new();
        let events = detector.feed(b"some output\r\nroot@host:/# ");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_no_prompt_in_partial_output() {
        let mut detector = PromptDetector::new();
        let events = detector.feed(b"compiling crate...\r\n");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_consecutive_prompts() {
        let mut detector = PromptDetector::new();
        let events1 = detector.feed(b"user@host:~$ ");
        assert_eq!(events1.len(), 1);
        let events2 = detector.feed(b"hello\r\nuser@host:~$ ");
        assert_eq!(events2.len(), 1);
    }

    #[test]
    fn test_prompt_split_across_chunks() {
        let mut detector = PromptDetector::new();
        let events1 = detector.feed(b"output\r\nuser@ho");
        assert_eq!(events1.len(), 0);
        let events2 = detector.feed(b"st:~$ ");
        assert_eq!(events2.len(), 1);
    }

    #[test]
    fn test_ansi_stripped_before_matching() {
        let mut detector = PromptDetector::new();
        let events =
            detector.feed(b"output\r\n\x1b[32muser@host\x1b[0m:\x1b[34m~\x1b[0m$ ");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_dollar_in_output_not_false_positive() {
        let mut detector = PromptDetector::new();
        let events = detector.feed(b"price is $100\r\n");
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_custom_pattern() {
        let mut detector = PromptDetector::with_patterns(vec![r"❯\s*$".to_string()]);
        let events = detector.feed(b"output\r\n\xe2\x9d\xaf ");
        assert_eq!(events.len(), 1);
    }
}
