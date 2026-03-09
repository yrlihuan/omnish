/// A temporary single-line status display.
///
/// Renders a status message on its own line below the current cursor position.
/// Tracks how many lines it occupies so it can erase itself completely when
/// `clear()` is called, leaving the terminal in the same state as before
/// `show()` was first called.
///
/// Usage:
/// ```
/// let mut status = LineStatus::new();
/// write!(stdout, "{}", status.show("(thinking...)"));
/// write!(stdout, "{}", status.show("🔧 running tool A"));  // replaces previous
/// write!(stdout, "{}", status.clear());                     // erases completely
/// ```
pub struct LineStatus {
    /// Number of lines currently occupied on screen (0 = nothing shown).
    lines: usize,
}

impl LineStatus {
    pub fn new() -> Self {
        Self { lines: 0 }
    }

    /// Returns true if something is currently shown on screen.
    #[allow(dead_code)]
    pub fn is_visible(&self) -> bool {
        self.lines > 0
    }

    /// Replace the current status with `text`.
    ///
    /// If something is already shown, erases it first then renders the new
    /// text.  The text may contain newlines; each line gets its own row.
    /// Returns an ANSI escape sequence string suitable for writing to a
    /// raw-mode terminal.
    pub fn show(&mut self, text: &str) -> String {
        let mut out = self.erase_seq();
        out.push_str(&Self::render_seq(text));
        self.lines = text.lines().count().max(1);
        out
    }

    /// Erase the status completely.  After this call `is_visible()` returns
    /// false and the terminal is restored to the position it was in before
    /// the first `show()`.
    pub fn clear(&mut self) -> String {
        let seq = self.erase_seq();
        self.lines = 0;
        seq
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Build the ANSI sequence that clears all occupied lines from bottom to
    /// top and leaves the cursor at the original position (before the first
    /// `show()` call).
    ///
    /// `render_seq()` prefixes every line with `\r\n`, so N lines of text
    /// occupy rows R+1 … R+N (where R is the cursor row before `show()`).
    /// After rendering the cursor sits at row R+N.  We clear each line from
    /// bottom to top, then move up one more to return to R.
    fn erase_seq(&self) -> String {
        if self.lines == 0 {
            return String::new();
        }
        let mut out = String::new();
        // Clear current line (R+N), then move up & clear until R+1.
        for i in 0..self.lines {
            if i > 0 {
                out.push_str("\x1b[1A");
            }
            out.push_str("\r\x1b[K");
        }
        // Move up one more to return to the original row R.
        out.push_str("\x1b[1A");
        out
    }

    /// Build the ANSI sequence that renders `text` below the current cursor.
    fn render_seq(text: &str) -> String {
        let mut out = String::new();
        for line in text.lines() {
            out.push_str(&format!("\r\n\x1b[K\x1b[2m{}\x1b[0m", line));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                // skip until a letter that ends the escape sequence
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    #[test]
    fn new_is_not_visible() {
        let s = LineStatus::new();
        assert!(!s.is_visible());
        assert_eq!(s.lines, 0);
    }

    #[test]
    fn show_makes_visible() {
        let mut s = LineStatus::new();
        let seq = s.show("(thinking...)");
        assert!(s.is_visible());
        assert_eq!(s.lines, 1);
        assert!(seq.contains("thinking"), "seq should contain text: {seq}");
    }

    #[test]
    fn clear_after_show_restores_invisible() {
        let mut s = LineStatus::new();
        s.show("hello");
        let seq = s.clear();
        assert!(!s.is_visible());
        assert_eq!(s.lines, 0);
        // erase seq must contain cursor-up and erase-line
        assert!(seq.contains('\x1b'));
    }

    #[test]
    fn clear_when_empty_is_empty_string() {
        let mut s = LineStatus::new();
        assert_eq!(s.clear(), "");
    }

    #[test]
    fn show_replaces_previous() {
        let mut s = LineStatus::new();
        s.show("first");
        let seq = s.show("second");
        assert_eq!(s.lines, 1);
        // The replacement sequence must erase the old line (cursor-up present)
        assert!(seq.contains('\x1b'));
        let visible = strip_ansi(&seq);
        assert!(visible.contains("second"));
    }

    #[test]
    fn multiline_text_counts_correctly() {
        let mut s = LineStatus::new();
        s.show("line one\nline two\nline three");
        assert_eq!(s.lines, 3);
    }

    #[test]
    fn show_then_clear_then_show_works() {
        let mut s = LineStatus::new();
        s.show("a");
        s.clear();
        let seq = s.show("b");
        assert!(s.is_visible());
        // After clear the erase part of show() should be empty (lines was 0)
        assert!(strip_ansi(&seq).contains('b'));
    }

    // -----------------------------------------------------------------------
    // Terminal-emulation tests using vt100 parser
    // -----------------------------------------------------------------------

    fn parse_ansi(input: &str, cols: u16, rows: u16) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(input.as_bytes());
        parser
    }

    fn get_row(screen: &vt100::Screen, row: u16, cols: u16) -> String {
        screen.rows(0, cols).nth(row as usize).unwrap_or_default()
    }

    /// Regression: after clear(), the line that held "(thinking...)" must be
    /// fully erased — no residual characters when the LLM response is short.
    #[test]
    fn clear_erases_text_completely() {
        let cols: u16 = 40;
        let mut out = String::new();
        let mut s = LineStatus::new();
        out.push_str(&s.show("(thinking...)"));
        out.push_str(&s.clear());

        let parser = parse_ansi(&out, cols, 10);
        let screen = parser.screen();
        // Every visible row should be blank after show+clear.
        for row in 0..10 {
            let text = get_row(screen, row, cols);
            assert!(
                text.trim().is_empty(),
                "row {row} should be blank but got: {text:?}",
            );
        }
    }

    /// After clear(), a short response written to the same area must not show
    /// any leftover "(thinking...)" characters.
    #[test]
    fn short_response_after_clear_has_no_residue() {
        let cols: u16 = 40;
        let mut out = String::new();
        let mut s = LineStatus::new();
        out.push_str(&s.show("(thinking...)"));
        out.push_str(&s.clear());
        // Simulate render_response("OK") — \r\n + text + \r\n
        out.push_str("\r\n\x1b[37mOK\x1b[0m\r\n");

        let parser = parse_ansi(&out, cols, 10);
        let all = parser.screen().contents();
        assert!(all.contains("OK"), "response should be visible");
        assert!(
            !all.contains("thinking"),
            "no residual thinking text: {all:?}",
        );
    }

    /// show() replacement should fully erase the previous text before
    /// rendering the new text, even if the new text is shorter.
    #[test]
    fn show_replace_shorter_text_no_residue() {
        let cols: u16 = 40;
        let mut out = String::new();
        let mut s = LineStatus::new();
        out.push_str(&s.show("(thinking very long text here...)"));
        out.push_str(&s.show("OK"));

        let parser = parse_ansi(&out, cols, 10);
        let all = parser.screen().contents();
        assert!(all.contains("OK"), "new text should be visible");
        assert!(
            !all.contains("thinking"),
            "old text should be gone: {all:?}",
        );
    }
}
