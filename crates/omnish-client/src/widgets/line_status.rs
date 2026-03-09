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

    /// Build the ANSI sequence that moves up and clears all occupied lines.
    /// Does NOT reset `self.lines` — callers do that themselves.
    fn erase_seq(&self) -> String {
        if self.lines == 0 {
            return String::new();
        }
        let mut out = String::new();
        // Move up to the first status line and clear each row.
        out.push_str(&format!("\x1b[{}A", self.lines));
        for _ in 0..self.lines {
            out.push_str("\r\x1b[K\r\n");
        }
        // Move back up so the cursor is at the start of the cleared region.
        out.push_str(&format!("\x1b[{}A", self.lines));
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
}
