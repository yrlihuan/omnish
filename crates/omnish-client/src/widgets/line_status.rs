/// A temporary multi-line status display.
///
/// Renders status messages below the current cursor position.
/// Tracks how many lines it occupies so it can erase itself completely when
/// `clear()` is called, leaving the terminal in the same state as before
/// `show()` was first called.
///
/// Features:
/// - Lines exceeding `max_cols` are truncated with "..."
/// - `append()` adds a new line without erasing previous content
/// - When total lines exceed `max_lines`, older lines are hidden
///
/// Usage:
/// ```
/// let mut status = LineStatus::new(80, 5);
/// write!(stdout, "{}", status.show("(thinking...)"));
/// write!(stdout, "{}", status.append("🔧 running tool A"));  // adds a line
/// write!(stdout, "{}", status.clear());                       // erases completely
/// ```
pub struct LineStatus {
    /// Number of lines currently occupied on screen (0 = nothing shown).
    lines: usize,
    /// All accumulated message lines (for append mode).
    content: Vec<String>,
    /// Maximum display width per line (characters). 0 = unlimited.
    max_cols: usize,
    /// Maximum number of visible lines. 0 = unlimited.
    max_lines: usize,
}

impl LineStatus {
    pub fn new(max_cols: usize, max_lines: usize) -> Self {
        Self {
            lines: 0,
            content: Vec::new(),
            max_cols,
            max_lines,
        }
    }

    /// Returns true if something is currently shown on screen.
    #[allow(dead_code)]
    pub fn is_visible(&self) -> bool {
        self.lines > 0
    }

    /// Replace the current status with `text`.
    ///
    /// Clears all accumulated content and shows only `text`.
    /// Returns an ANSI escape sequence string suitable for writing to a
    /// raw-mode terminal.
    pub fn show(&mut self, text: &str) -> String {
        self.content.clear();
        for line in text.lines() {
            self.content.push(line.to_string());
        }
        if self.content.is_empty() {
            self.content.push(String::new());
        }
        self.redraw()
    }

    /// Append a new line to the status display.
    ///
    /// Adds `text` as new line(s) below the existing content. If the total
    /// exceeds `max_lines`, older lines are hidden (only the most recent
    /// `max_lines` are shown).
    pub fn append(&mut self, text: &str) -> String {
        for line in text.lines() {
            self.content.push(line.to_string());
        }
        if text.is_empty() {
            self.content.push(String::new());
        }
        self.redraw()
    }

    /// Erase the status completely.  After this call `is_visible()` returns
    /// false and the terminal is restored to the position it was in before
    /// the first `show()` was called.
    pub fn clear(&mut self) -> String {
        let seq = self.erase_seq();
        self.lines = 0;
        self.content.clear();
        seq
    }

    /// Returns current styled content lines for ChatLayout integration.
    /// Each line has dim styling applied.
    pub fn lines_content(&self) -> Vec<String> {
        if self.content.is_empty() {
            return Vec::new();
        }
        let visible = self.visible_lines();
        visible.iter().map(|l| {
            let truncated = Self::truncate_line(l, self.max_cols);
            format!("\x1b[2m{}\x1b[0m", truncated)
        }).collect()
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Erase current display and re-render the visible portion of content.
    fn redraw(&mut self) -> String {
        let mut out = self.erase_seq();
        let visible = self.visible_lines();
        out.push_str(&Self::render_seq(&visible, self.max_cols));
        self.lines = visible.len().max(if self.content.is_empty() { 0 } else { 1 });
        out
    }

    /// Return the lines that should be visible (tail window of max_lines).
    fn visible_lines(&self) -> Vec<&str> {
        let all: Vec<&str> = self.content.iter().map(|s| s.as_str()).collect();
        if self.max_lines > 0 && all.len() > self.max_lines {
            all[all.len() - self.max_lines..].to_vec()
        } else {
            all
        }
    }

    /// Truncate a line to fit within max_cols, appending "..." if needed.
    fn truncate_line(line: &str, max_cols: usize) -> String {
        if max_cols == 0 || line.chars().count() <= max_cols {
            return line.to_string();
        }
        let truncated: String = line.chars().take(max_cols.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }

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

    /// Build the ANSI sequence that renders lines below the current cursor.
    fn render_seq(lines: &[&str], max_cols: usize) -> String {
        let mut out = String::new();
        for line in lines {
            let display = Self::truncate_line(line, max_cols);
            out.push_str(&format!("\r\n\x1b[K\x1b[2m{}\x1b[0m", display));
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
        let s = LineStatus::new(80, 5);
        assert!(!s.is_visible());
        assert_eq!(s.lines, 0);
    }

    #[test]
    fn show_makes_visible() {
        let mut s = LineStatus::new(80, 5);
        let seq = s.show("(thinking...)");
        assert!(s.is_visible());
        assert_eq!(s.lines, 1);
        assert!(seq.contains("thinking"), "seq should contain text: {seq}");
    }

    #[test]
    fn clear_after_show_restores_invisible() {
        let mut s = LineStatus::new(80, 5);
        s.show("hello");
        let seq = s.clear();
        assert!(!s.is_visible());
        assert_eq!(s.lines, 0);
        assert!(seq.contains('\x1b'));
    }

    #[test]
    fn clear_when_empty_is_empty_string() {
        let mut s = LineStatus::new(80, 5);
        assert_eq!(s.clear(), "");
    }

    #[test]
    fn show_replaces_previous() {
        let mut s = LineStatus::new(80, 5);
        s.show("first");
        let seq = s.show("second");
        assert_eq!(s.lines, 1);
        assert!(seq.contains('\x1b'));
        let visible = strip_ansi(&seq);
        assert!(visible.contains("second"));
        assert!(!visible.contains("first"));
    }

    #[test]
    fn multiline_text_counts_correctly() {
        let mut s = LineStatus::new(80, 5);
        s.show("line one\nline two\nline three");
        assert_eq!(s.lines, 3);
    }

    #[test]
    fn show_then_clear_then_show_works() {
        let mut s = LineStatus::new(80, 5);
        s.show("a");
        s.clear();
        let seq = s.show("b");
        assert!(s.is_visible());
        assert!(strip_ansi(&seq).contains('b'));
    }

    // -- Truncation tests --

    #[test]
    fn truncate_long_line() {
        let mut s = LineStatus::new(20, 5);
        let seq = s.show("this is a very long line that exceeds the limit");
        let visible = strip_ansi(&seq);
        assert!(visible.contains("..."));
        // The truncated content should be at most 20 chars
        for line in visible.lines() {
            let line = line.trim_start_matches('\r');
            if !line.is_empty() {
                assert!(line.chars().count() <= 20, "line too long: {line:?}");
            }
        }
    }

    #[test]
    fn short_line_not_truncated() {
        let mut s = LineStatus::new(80, 5);
        let seq = s.show("short");
        let visible = strip_ansi(&seq);
        assert!(visible.contains("short"));
        assert!(!visible.contains("..."));
    }

    #[test]
    fn unlimited_cols_no_truncation() {
        let mut s = LineStatus::new(0, 0);
        let long = "a".repeat(200);
        let seq = s.show(&long);
        let visible = strip_ansi(&seq);
        assert!(!visible.contains("..."));
    }

    // -- Append tests --

    #[test]
    fn append_adds_line() {
        let mut s = LineStatus::new(80, 5);
        s.show("line 1");
        s.append("line 2");
        assert_eq!(s.lines, 2);
        assert_eq!(s.content.len(), 2);
    }

    #[test]
    fn append_multiple_lines() {
        let mut s = LineStatus::new(80, 10);
        s.show("first");
        s.append("second");
        s.append("third");
        assert_eq!(s.lines, 3);
        assert_eq!(s.content, vec!["first", "second", "third"]);
    }

    #[test]
    fn append_contains_all_text() {
        let mut s = LineStatus::new(80, 5);
        s.show("line 1");
        let seq = s.append("line 2");
        let visible = strip_ansi(&seq);
        assert!(visible.contains("line 1"));
        assert!(visible.contains("line 2"));
    }

    // -- Max lines tests --

    #[test]
    fn max_lines_hides_old_lines() {
        let mut s = LineStatus::new(80, 3);
        s.show("line 1");
        s.append("line 2");
        s.append("line 3");
        let seq = s.append("line 4");
        // Only 3 lines should be visible
        assert_eq!(s.lines, 3);
        let visible = strip_ansi(&seq);
        assert!(!visible.contains("line 1"), "line 1 should be hidden");
        assert!(visible.contains("line 2"));
        assert!(visible.contains("line 3"));
        assert!(visible.contains("line 4"));
    }

    #[test]
    fn max_lines_unlimited() {
        let mut s = LineStatus::new(80, 0);
        for i in 0..20 {
            if i == 0 {
                s.show(&format!("line {i}"));
            } else {
                s.append(&format!("line {i}"));
            }
        }
        assert_eq!(s.lines, 20);
        assert_eq!(s.content.len(), 20);
    }

    #[test]
    fn show_resets_content_after_append() {
        let mut s = LineStatus::new(80, 5);
        s.show("a");
        s.append("b");
        s.append("c");
        let seq = s.show("fresh");
        assert_eq!(s.lines, 1);
        assert_eq!(s.content, vec!["fresh"]);
        let visible = strip_ansi(&seq);
        assert!(visible.contains("fresh"));
        assert!(!visible.contains("a"));
        assert!(!visible.contains("b"));
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
        let mut s = LineStatus::new(cols as usize, 5);
        out.push_str(&s.show("(thinking...)"));
        out.push_str(&s.clear());

        let parser = parse_ansi(&out, cols, 10);
        let screen = parser.screen();
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
        let mut s = LineStatus::new(cols as usize, 5);
        out.push_str(&s.show("(thinking...)"));
        out.push_str(&s.clear());
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
        let mut s = LineStatus::new(cols as usize, 5);
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

    /// vt100 test: append adds lines and clear erases all of them.
    #[test]
    fn vt100_append_and_clear() {
        let cols: u16 = 40;
        let mut out = String::new();
        let mut s = LineStatus::new(cols as usize, 5);
        out.push_str(&s.show("line 1"));
        out.push_str(&s.append("line 2"));
        out.push_str(&s.append("line 3"));
        out.push_str(&s.clear());

        let parser = parse_ansi(&out, cols, 10);
        let screen = parser.screen();
        for row in 0..10 {
            let text = get_row(screen, row, cols);
            assert!(
                text.trim().is_empty(),
                "row {row} should be blank but got: {text:?}",
            );
        }
    }

    #[test]
    fn test_lines_accessor() {
        let mut status = LineStatus::new(80, 5);
        assert!(status.lines_content().is_empty());

        status.show("thinking...");
        let lines = status.lines_content();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("thinking..."));

        status.append("tool call 1");
        let lines = status.lines_content();
        assert_eq!(lines.len(), 2);

        status.clear();
        assert!(status.lines_content().is_empty());
    }

    /// vt100 test: max_lines scrolling only shows the tail.
    #[test]
    fn vt100_max_lines_shows_tail() {
        let cols: u16 = 40;
        let mut out = String::new();
        let mut s = LineStatus::new(cols as usize, 2);
        out.push_str(&s.show("aaa"));
        out.push_str(&s.append("bbb"));
        out.push_str(&s.append("ccc"));

        let parser = parse_ansi(&out, cols, 10);
        let all = parser.screen().contents();
        assert!(!all.contains("aaa"), "aaa should be hidden: {all:?}");
        assert!(all.contains("bbb"), "bbb should be visible: {all:?}");
        assert!(all.contains("ccc"), "ccc should be visible: {all:?}");
    }
}
