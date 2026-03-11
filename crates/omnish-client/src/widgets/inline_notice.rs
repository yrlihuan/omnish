/// A reusable widget that inserts a notification line above the current cursor position.
///
/// Uses Scroll Up + Insert Line to push the current line down and render a dim
/// message above it. The message stays in the terminal scrollback and naturally
/// scrolls away as new output appears.
pub struct InlineNotice;

impl InlineNotice {
    /// Generate the ANSI escape sequence that inserts a dim notice line above
    /// the current cursor position, then returns the cursor to its original line.
    ///
    /// `max_cols` limits the visible message length to avoid wrapping.
    ///
    /// The sequence:
    /// 1. Save cursor position with DECSC (row R, col C)
    /// 2. Scroll Up (`\x1b[1S`) — content shifts up, blank line at bottom,
    ///    prompt moves from R to R-1, cursor stays at (R, C)
    /// 3. Move up one line (`\x1b[1A`) — cursor at (R-1) on the prompt
    /// 4. Insert Line (`\x1b[1L`) — blank at R-1, prompt pushed back to R
    /// 5. Write the dim notice (truncated to max_cols) on the blank line
    /// 6. Restore cursor with DECRC — back to (R, C) where the prompt is
    pub fn render(message: &str, max_cols: usize) -> String {
        let truncated = truncate(message, max_cols);
        format!(
            "\x1b7\x1b[1S\x1b[1A\x1b[1L\r\x1b[2m{}\x1b[0m\x1b8",
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
    fn test_render_preserves_cursor() {
        let output = InlineNotice::render("test", 80);
        assert!(output.contains("\x1b7"));   // DECSC save cursor
        assert!(output.contains("\x1b8"));   // DECRC restore cursor
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
        assert!(output.contains("..."));
        assert!(!output.contains(&"a".repeat(100)));
    }

    // -----------------------------------------------------------------------
    // Terminal-emulation tests using vt100 parser
    // -----------------------------------------------------------------------

    fn make_parser(cols: u16, rows: u16) -> vt100::Parser {
        vt100::Parser::new(rows, cols, 0)
    }

    fn get_row(screen: &vt100::Screen, row: u16, cols: u16) -> String {
        screen.rows(0, cols).nth(row as usize).unwrap_or_default()
    }

    /// When the screen is full and the cursor is on the last row (simulating
    /// a typical shell prompt at the bottom), InlineNotice should:
    /// 1. Show the notice on the second-to-last row
    /// 2. Keep the prompt on the last row
    /// 3. Return the cursor to its original column on the prompt line
    #[test]
    fn vt100_full_screen_cursor_on_last_row() {
        let cols: u16 = 60;
        let rows: u16 = 5;
        let mut parser = make_parser(cols, rows);

        // Fill screen: rows 0-3 with content, row 4 with prompt + partial input
        let mut setup = String::new();
        for i in 0..rows - 1 {
            setup.push_str(&format!("line {}\r\n", i));
        }
        setup.push_str("user@host:~ $ cd foo");
        parser.process(setup.as_bytes());

        // Verify setup: cursor at last row, col 20
        let screen = parser.screen();
        assert_eq!(screen.cursor_position(), (rows - 1, 20));
        assert!(get_row(screen, rows - 1, cols).contains("user@host:~ $ cd foo"));

        // Inject the InlineNotice
        let notice = InlineNotice::render("[omnish] reconnected", cols as usize);
        parser.process(notice.as_bytes());

        let screen = parser.screen();

        // The notice should appear on the second-to-last row
        let notice_row = get_row(screen, rows - 2, cols);
        assert!(
            notice_row.contains("[omnish] reconnected"),
            "notice should be on row {}: got {:?}",
            rows - 2,
            notice_row
        );

        // The prompt should still be on the last row
        let prompt_row = get_row(screen, rows - 1, cols);
        assert!(
            prompt_row.contains("user@host:~ $ cd foo"),
            "prompt should be on last row: got {:?}",
            prompt_row
        );

        // Cursor should be back at last row, same column (20)
        let (cur_row, cur_col) = screen.cursor_position();
        assert_eq!(
            cur_row,
            rows - 1,
            "cursor should be on last row ({}), got {}",
            rows - 1,
            cur_row
        );
        assert_eq!(
            cur_col, 20,
            "cursor column should be preserved at 20, got {}",
            cur_col
        );
    }

    /// Same scenario but cursor at a short prompt position.
    #[test]
    fn vt100_full_screen_cursor_at_short_prompt() {
        let cols: u16 = 60;
        let rows: u16 = 5;
        let mut parser = make_parser(cols, rows);

        let mut setup = String::new();
        for i in 0..rows - 1 {
            setup.push_str(&format!("line {}\r\n", i));
        }
        setup.push_str("$ ");
        parser.process(setup.as_bytes());

        assert_eq!(parser.screen().cursor_position(), (rows - 1, 2));

        let notice = InlineNotice::render("[omnish] reconnected to daemon", cols as usize);
        parser.process(notice.as_bytes());

        let screen = parser.screen();
        let notice_row = get_row(screen, rows - 2, cols);
        assert!(
            notice_row.contains("[omnish] reconnected to daemon"),
            "notice row: {:?}",
            notice_row
        );

        let prompt_row = get_row(screen, rows - 1, cols);
        assert!(
            prompt_row.starts_with("$ "),
            "prompt row: {:?}",
            prompt_row
        );

        let (cur_row, cur_col) = screen.cursor_position();
        assert_eq!(cur_row, rows - 1);
        assert_eq!(cur_col, 2, "cursor col should be 2, got {}", cur_col);
    }

    /// Non-full screen: cursor in the middle, plenty of blank rows below.
    #[test]
    fn vt100_mid_screen_cursor() {
        let cols: u16 = 60;
        let rows: u16 = 10;
        let mut parser = make_parser(cols, rows);

        // Only fill 3 rows, cursor at row 2
        parser.process(b"line 0\r\nline 1\r\nprompt $ cmd");

        let screen = parser.screen();
        assert_eq!(screen.cursor_position(), (2, 12));

        let notice = InlineNotice::render("[omnish] reconnected", cols as usize);
        parser.process(notice.as_bytes());

        let screen = parser.screen();

        // Notice should be one row above the prompt
        let notice_row = get_row(screen, 1, cols);
        assert!(
            notice_row.contains("[omnish] reconnected"),
            "notice row: {:?}",
            notice_row
        );

        // Prompt should still be visible
        let prompt_row = get_row(screen, 2, cols);
        assert!(
            prompt_row.contains("prompt $ cmd"),
            "prompt row: {:?}",
            prompt_row
        );

        // Cursor position preserved
        let (cur_row, cur_col) = screen.cursor_position();
        assert_eq!(cur_row, 2, "cursor row should be 2, got {}", cur_row);
        assert_eq!(cur_col, 12, "cursor col should be 12, got {}", cur_col);
    }
}
