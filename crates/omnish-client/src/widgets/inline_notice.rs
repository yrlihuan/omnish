/// A reusable widget that inserts a notification line above the current cursor position.
///
/// Uses the Insert Line ANSI escape (`\x1b[1L`) to push the current line down
/// and render a dim message above it. The message stays in the terminal scrollback
/// and naturally scrolls away as new output appears.
pub struct InlineNotice;

impl InlineNotice {
    /// Generate the ANSI escape sequence that inserts a dim notice line above
    /// the current cursor position, then returns the cursor to its original line.
    ///
    /// `max_cols` limits the visible message length to avoid wrapping.
    ///
    /// The sequence:
    /// 1. Save cursor position (row R, col C)
    /// 2. Insert a blank line at cursor (pushes current line to R+1)
    /// 3. Write the dim message (truncated to max_cols)
    /// 4. Restore cursor to (R, C), then move down 1 to (R+1, C)
    pub fn render(message: &str, max_cols: usize) -> String {
        let truncated = truncate(message, max_cols);
        format!(
            "\x1b[s\x1b[1L\r\x1b[2m{}\x1b[0m\x1b[u\x1b[1B",
            truncated
        )
    }
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else if max <= 3 {
        chars[..max].iter().collect()
    } else {
        let mut out: String = chars[..max - 3].iter().collect();
        out.push_str("...");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_contains_message() {
        let output = InlineNotice::render("[omnish] reconnected", 80);
        assert!(output.contains("[omnish] reconnected"));
    }

    #[test]
    fn test_render_has_dim_formatting() {
        let output = InlineNotice::render("test", 80);
        assert!(output.contains("\x1b[2m"));
        assert!(output.contains("\x1b[0m"));
    }

    #[test]
    fn test_render_has_insert_line() {
        let output = InlineNotice::render("test", 80);
        assert!(output.contains("\x1b[1L"));
    }

    #[test]
    fn test_render_preserves_cursor_column() {
        let output = InlineNotice::render("test", 80);
        assert!(output.contains("\x1b[s"));  // save cursor
        assert!(output.contains("\x1b[u"));  // restore cursor
        assert!(output.contains("\x1b[1B")); // +1 row to compensate insert
        assert!(!output.ends_with('\r'));     // no \r clobbering column
    }

    #[test]
    fn test_render_no_newline() {
        let output = InlineNotice::render("test", 80);
        assert!(!output.contains('\n'));
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long() {
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn test_truncate_tiny_max() {
        assert_eq!(truncate("hello", 2), "he");
    }

    #[test]
    fn test_render_truncates_to_max_cols() {
        let long_msg = "a".repeat(100);
        let output = InlineNotice::render(&long_msg, 20);
        // Should contain truncated version, not full 100 chars
        assert!(output.contains("..."));
        assert!(!output.contains(&"a".repeat(100)));
    }
}
