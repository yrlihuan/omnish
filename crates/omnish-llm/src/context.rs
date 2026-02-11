pub struct ContextBuilder {
    max_chars: usize,
}

impl ContextBuilder {
    pub fn new() -> Self {
        Self { max_chars: 8000 }
    }

    pub fn max_chars(mut self, n: usize) -> Self {
        self.max_chars = n;
        self
    }

    /// Strip ANSI escape sequences from raw bytes
    pub fn strip_escapes(&self, raw: &[u8]) -> String {
        let s = String::from_utf8_lossy(raw);
        let mut result = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            } else {
                result.push(c);
            }
        }
        result
    }

    /// Truncate from the front, keeping the last max_chars characters
    pub fn truncate<'a>(&self, text: &'a str) -> &'a str {
        if text.len() <= self.max_chars {
            text
        } else {
            &text[text.len() - self.max_chars..]
        }
    }
}

impl Default for ContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}
