/// A reusable widget that inserts a notification line above the current cursor position.
///
/// Two rendering modes selected by `at_bottom`:
/// - **Bottom mode** (`at_bottom = true`): Scroll Up + Insert Line. Optimized for
///   when the cursor is near the bottom of the screen (typical runtime prompt).
/// - **Top mode** (`at_bottom = false`): Insert Line + Move Down. Optimized for
///   when the cursor is near the top (startup, screen not full).
pub struct InlineNotice;

impl InlineNotice {
    /// Generate the ANSI escape sequence that inserts a dim notice line above
    /// the current cursor position, then returns the cursor to its original line.
    ///
    /// `max_cols` limits the visible message length to avoid wrapping.
    /// `at_bottom` selects the rendering strategy:
    ///
    /// **Bottom mode** (`at_bottom = true`) — for cursor near screen bottom:
    /// 1. DECSC save cursor (R, C)
    /// 2. Scroll Up (`\x1b[1S`) — content shifts up, row 0 enters scrollback
    /// 3. Move up (`\x1b[1A`) — cursor at R-1
    /// 4. Insert Line (`\x1b[1L`) — blank at R-1, prompt pushed back to R
    /// 5. Write dim notice on the blank line
    /// 6. DECRC restore cursor to (R, C)
    ///
    /// **Top mode** (`at_bottom = false`) — for cursor near screen top:
    /// 1. DECSC save cursor (R, C)
    /// 2. Insert Line (`\x1b[1L`) — blank at R, content pushed to R+1
    /// 3. Write dim notice on the blank line
    /// 4. DECRC restore cursor to (R, C) — now on the notice line
    /// 5. Move down (`\x1b[1B`) — follow original content at R+1
    #[cfg(test)]
    pub fn render(message: &str, max_cols: usize) -> String {
        Self::render_at(message, max_cols, true)
    }

    pub fn render_at(message: &str, max_cols: usize, at_bottom: bool) -> String {
        let truncated = crate::display::truncate_cols(message, max_cols);
        if at_bottom {
            format!(
                "\x1b7\x1b[1S\x1b[1A\x1b[1L\r\x1b[2m{}\x1b[0m\x1b8",
                truncated
            )
        } else {
            format!(
                "\x1b7\r\x1b[1L\x1b[2m{}\x1b[0m\x1b8\x1b[1B",
                truncated
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::truncate_cols as truncate;

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
    fn test_render_bottom_has_scroll_up_and_insert_line() {
        let output = InlineNotice::render_at("test", 80, true);
        assert!(output.contains("\x1b[1S"));
        assert!(output.contains("\x1b[1L"));
    }

    #[test]
    fn test_render_top_has_insert_line_and_move_down() {
        let output = InlineNotice::render_at("test", 80, false);
        assert!(output.contains("\x1b[1L"));
        assert!(output.contains("\x1b[1B"));
        assert!(!output.contains("\x1b[1S")); // no scroll up
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
        assert_eq!(truncate("hello world", 8), "hello w…");
    }

    #[test]
    fn test_truncate_tiny_max() {
        assert_eq!(truncate("hello", 2), "h…");
    }

    #[test]
    fn test_render_truncates_to_max_cols() {
        let long_msg = "a".repeat(100);
        let output = InlineNotice::render(&long_msg, 20);
        assert!(output.contains("…"));
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

    /// Full screen, cursor on the last row (typical shell prompt at bottom).
    #[test]
    fn vt100_full_screen_cursor_on_last_row() {
        let cols: u16 = 60;
        let rows: u16 = 5;
        let mut parser = make_parser(cols, rows);

        let mut setup = String::new();
        for i in 0..rows - 1 {
            setup.push_str(&format!("line {}\r\n", i));
        }
        setup.push_str("user@host:~ $ cd foo");
        parser.process(setup.as_bytes());

        assert_eq!(parser.screen().cursor_position(), (rows - 1, 20));

        let notice = InlineNotice::render("[omnish] reconnected", cols as usize);
        parser.process(notice.as_bytes());

        let screen = parser.screen();

        let notice_row = get_row(screen, rows - 2, cols);
        assert!(
            notice_row.contains("[omnish] reconnected"),
            "notice should be on row {}: got {:?}",
            rows - 2, notice_row
        );

        let prompt_row = get_row(screen, rows - 1, cols);
        assert!(
            prompt_row.contains("user@host:~ $ cd foo"),
            "prompt should be on last row: got {:?}",
            prompt_row
        );

        let (cur_row, cur_col) = screen.cursor_position();
        assert_eq!(cur_row, rows - 1, "cursor row");
        assert_eq!(cur_col, 20, "cursor col");
    }

    /// Full screen, short prompt.
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
        assert!(notice_row.contains("[omnish] reconnected to daemon"), "notice: {:?}", notice_row);

        let prompt_row = get_row(screen, rows - 1, cols);
        assert!(prompt_row.starts_with("$ "), "prompt: {:?}", prompt_row);

        let (cur_row, cur_col) = screen.cursor_position();
        assert_eq!(cur_row, rows - 1);
        assert_eq!(cur_col, 2);
    }

    /// Non-full screen: cursor in the middle.
    #[test]
    fn vt100_mid_screen_cursor() {
        let cols: u16 = 60;
        let rows: u16 = 10;
        let mut parser = make_parser(cols, rows);

        parser.process(b"line 0\r\nline 1\r\nprompt $ cmd");

        assert_eq!(parser.screen().cursor_position(), (2, 12));

        let notice = InlineNotice::render("[omnish] reconnected", cols as usize);
        parser.process(notice.as_bytes());

        let screen = parser.screen();

        let notice_row = get_row(screen, 1, cols);
        assert!(notice_row.contains("[omnish] reconnected"), "notice: {:?}", notice_row);

        let prompt_row = get_row(screen, 2, cols);
        assert!(prompt_row.contains("prompt $ cmd"), "prompt: {:?}", prompt_row);

        let (cur_row, cur_col) = screen.cursor_position();
        assert_eq!(cur_row, 2, "cursor row");
        assert_eq!(cur_col, 12, "cursor col");
    }

    /// Cursor at row 0 with top mode: Insert Line pushes content down,
    /// notice at row 0, cursor follows content to row 1.
    #[test]
    fn vt100_cursor_at_top_row() {
        let cols: u16 = 60;
        let rows: u16 = 5;
        let mut parser = make_parser(cols, rows);

        parser.process(b"startup msg");

        assert_eq!(parser.screen().cursor_position(), (0, 11));

        let notice = InlineNotice::render_at("[omnish] connected", cols as usize, false);
        parser.process(notice.as_bytes());

        let screen = parser.screen();

        // Notice at row 0
        let row0 = get_row(screen, 0, cols);
        assert!(
            row0.contains("[omnish] connected"),
            "notice at row 0: {:?}", row0
        );

        // Original content pushed to row 1
        let row1 = get_row(screen, 1, cols);
        assert!(
            row1.contains("startup msg"),
            "original content at row 1: {:?}", row1
        );

        // Cursor follows content to row 1
        let (cur_row, cur_col) = screen.cursor_position();
        assert_eq!(cur_row, 1, "cursor row");
        assert_eq!(cur_col, 11, "cursor col");
    }

    /// Multiple sequential top-mode notices at startup.
    #[test]
    fn vt100_sequential_top_notices() {
        let cols: u16 = 60;
        let rows: u16 = 10;
        let mut parser = make_parser(cols, rows);

        parser.process(b"startup msg");
        assert_eq!(parser.screen().cursor_position(), (0, 11));

        let n1 = InlineNotice::render_at("[omnish] msg 1", cols as usize, false);
        parser.process(n1.as_bytes());

        let n2 = InlineNotice::render_at("[omnish] msg 2", cols as usize, false);
        parser.process(n2.as_bytes());

        let screen = parser.screen();
        let all = screen.contents();

        assert!(all.contains("[omnish] msg 1"), "msg 1 visible");
        assert!(all.contains("[omnish] msg 2"), "msg 2 visible");
        assert!(all.contains("startup msg"), "original content visible");

        // Cursor follows content down (row 0 → row 2 after 2 inserts)
        let (cur_row, cur_col) = screen.cursor_position();
        assert_eq!(cur_row, 2, "cursor row");
        assert_eq!(cur_col, 11, "cursor col");
    }

    /// Multiple sequential notices from mid-screen: each inserts above,
    /// all remain visible.
    #[test]
    fn vt100_sequential_notices() {
        let cols: u16 = 60;
        let rows: u16 = 10;
        let mut parser = make_parser(cols, rows);

        // Start at row 5 so there's room above
        parser.process(b"line 0\r\nline 1\r\nline 2\r\nline 3\r\nline 4\r\nprompt $");

        assert_eq!(parser.screen().cursor_position().0, 5);

        let n1 = InlineNotice::render("[omnish] msg 1", cols as usize);
        parser.process(n1.as_bytes());

        let n2 = InlineNotice::render("[omnish] msg 2", cols as usize);
        parser.process(n2.as_bytes());

        let n3 = InlineNotice::render("[omnish] msg 3", cols as usize);
        parser.process(n3.as_bytes());

        let screen = parser.screen();
        let all = screen.contents();

        assert!(all.contains("[omnish] msg 1"), "msg 1 visible");
        assert!(all.contains("[omnish] msg 2"), "msg 2 visible");
        assert!(all.contains("[omnish] msg 3"), "msg 3 visible");
        assert!(all.contains("prompt $"), "prompt visible");
    }
}
