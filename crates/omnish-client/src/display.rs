// crates/omnish-client/src/display.rs
//
// Pure functions that produce ANSI terminal output strings for the :: interactive mode.
// All functions return a String suitable for writing to a raw-mode terminal (using \r\n).

/// Render a separator line spanning `cols` columns (dim ─ characters).
pub fn render_separator(cols: u16) -> String {
    format!("\x1b[2m{}\x1b[0m", "─".repeat(cols as usize))
}

/// Render the initial prompt: newline, separator, newline, ❯ cursor.
/// The omnish UI occupies exactly 2 lines below the original cursor position
/// (separator line + ❯ input line). `render_dismiss()` relies on this count.
pub fn render_prompt(cols: u16) -> String {
    let separator = render_separator(cols);
    format!("\r\n{}\r\n\x1b[36m❯\x1b[0m ", separator)
}

/// Dismiss the omnish UI by clearing only the separator and ❯ lines below
/// the shell prompt, then moving the cursor back to the prompt line.
///
/// Steps: up 1 (to separator), clear from there to end of screen, up 1
/// (to prompt line). The shell prompt text is preserved.
///
/// After dismiss the caller should send SIGWINCH to make the shell redraw
/// its prompt in place (repositioning the cursor correctly). Do NOT send
/// `\r` to the PTY — the shell would echo `\r\n` which adds a blank line
/// on every dismiss cycle, gradually pushing screen content upward.
pub fn render_dismiss() -> String {
    "\x1b[1A\r\x1b[J\x1b[1A".to_string()
}

/// Render the input echo line: moves cursor to column 0, prints ❯ followed by user text,
/// then clears to end of line (to handle backspace correctly).
pub fn render_input_echo(user_input: &[u8]) -> String {
    format!(
        "\r\x1b[36m❯\x1b[0m {}\x1b[K",
        String::from_utf8_lossy(user_input)
    )
}

/// Render the "(thinking...)" status in dim text.
/// Moves to a new line, clears it, prints status, then moves to the next line.
pub fn render_thinking() -> String {
    "\r\n\x1b[K\x1b[2m(thinking...)\x1b[0m\r\n".to_string()
}

/// Format an LLM response for raw-mode display.
/// - Trims trailing whitespace from each line
/// - Converts \n to \r\n for raw mode
/// - Wraps in dim gray color
pub fn render_response(content: &str) -> String {
    let formatted: String = content
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\r\n");
    format!("\r\n\x1b[2m{}\x1b[0m\r\n", formatted)
}

/// Format an error message in red.
pub fn render_error(msg: &str) -> String {
    format!("\r\n\x1b[31m[omnish] {}\x1b[0m\r\n", msg)
}

/// Render ghost text (completion suggestion) in dim gray after the cursor.
/// Uses save/restore cursor so the cursor stays at the real input position.
/// Returns empty string if ghost is empty.
pub fn render_ghost_text(ghost: &str) -> String {
    if ghost.is_empty() {
        return String::new();
    }
    format!("\x1b7\x1b[2;90m{}\x1b[0m\x1b8", ghost)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: feed bytes into a vt100 parser and return the parser for inspection.
    fn parse_ansi(input: &str, cols: u16, rows: u16) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(input.as_bytes());
        parser
    }

    /// Helper: get the text of a specific row from the screen.
    fn get_row(screen: &vt100::Screen, row: u16, cols: u16) -> String {
        screen.rows(0, cols).nth(row as usize).unwrap_or_default()
    }

    #[test]
    fn test_prompt_renders_correctly() {
        let cols: u16 = 40;
        let output = render_prompt(cols);
        let parser = parse_ansi(&output, cols, 24);
        let screen = parser.screen();

        // The separator line should span the full width with ─ characters
        // Row 1 (0-indexed) should contain the separator (row 0 is blank from \r\n)
        let sep_row = get_row(screen, 1, cols);
        assert_eq!(sep_row.trim_end().chars().count(), cols as usize, "separator should span full width");
        assert!(sep_row.contains('─'), "separator should contain ─ characters");

        // Row 2 should contain the ❯ prompt
        let prompt_row = get_row(screen, 2, cols);
        assert!(prompt_row.contains('❯'), "prompt row should contain ❯");

        // Cursor should be on row 2, after "❯ " (col 2)
        let cursor = screen.cursor_position();
        assert_eq!(cursor.0, 2, "cursor should be on the prompt row");
        assert_eq!(cursor.1, 2, "cursor should be after ❯ and space");
    }

    #[test]
    fn test_input_echo_shows_text() {
        let cols: u16 = 40;

        // ASCII input
        let output = render_input_echo(b"hello world");
        let parser = parse_ansi(&output, cols, 24);
        let row = get_row(parser.screen(), 0, cols);
        assert!(row.contains('❯'), "should show ❯ prompt");
        assert!(row.contains("hello world"), "should show typed text");

        // Multibyte (Chinese) input
        let chinese = "你好世界";
        let output = render_input_echo(chinese.as_bytes());
        let parser = parse_ansi(&output, cols, 24);
        let row = get_row(parser.screen(), 0, cols);
        assert!(row.contains("你好世界"), "should display multibyte characters");

        // After backspace: shorter input should clear trailing chars via \x1b[K
        let output = render_input_echo(b"hel");
        let parser = parse_ansi(&output, cols, 24);
        let row = get_row(parser.screen(), 0, cols);
        assert!(row.contains("hel"), "should show shortened input");
        // The \x1b[K should have cleared anything after "hel"
        let after_text = row.trim_end();
        assert!(!after_text.contains("hello"), "old text should be cleared");
    }

    #[test]
    fn test_response_crlf_conversion() {
        let content = "line one\nline two\nline three  ";
        let output = render_response(content);

        // Verify raw string contains \r\n (raw mode requirement)
        assert!(output.contains("\r\n"), "response must use \\r\\n for raw mode");
        // Should not contain bare \n (without preceding \r)
        let without_cr = output.replace("\r\n", "");
        assert!(!without_cr.contains('\n'), "no bare \\n should remain");

        // Check trailing whitespace is trimmed
        assert!(!output.contains("three  "), "trailing whitespace should be trimmed");
        assert!(output.contains("three"), "content should be preserved");

        // Verify rendering via vt100
        let parser = parse_ansi(&output, 80, 24);
        let screen = parser.screen();
        let row1 = get_row(screen, 1, 80);
        assert!(row1.contains("line one"), "first line should render");
        let row2 = get_row(screen, 2, 80);
        assert!(row2.contains("line two"), "second line should render on next row");
    }

    #[test]
    fn test_thinking_status() {
        let output = render_thinking();
        let parser = parse_ansi(&output, 80, 24);
        let screen = parser.screen();

        // Row 1 because render_thinking starts with \r\n (moves to next line first)
        let row = get_row(screen, 1, 80);
        assert!(row.contains("(thinking...)"), "should display thinking status");

        // Verify it moves to new line first, then clears and prints status
        assert!(output.starts_with("\r\n\x1b[K"), "should newline then clear line before status");
    }

    #[test]
    fn test_separator() {
        let cols: u16 = 60;
        let output = render_separator(cols);
        let parser = parse_ansi(&output, cols, 24);
        let screen = parser.screen();
        let row = get_row(screen, 0, cols);
        let dashes: Vec<char> = row.trim_end().chars().collect();
        assert_eq!(dashes.len(), cols as usize, "separator should be exactly cols wide");
        assert!(dashes.iter().all(|&c| c == '─'), "separator should only contain ─");
    }

    #[test]
    fn test_error_message() {
        let output = render_error("Daemon not connected");
        let parser = parse_ansi(&output, 80, 24);
        let screen = parser.screen();

        // Error should appear on row 1 (row 0 is blank from leading \r\n)
        let row = get_row(screen, 1, 80);
        assert!(row.contains("[omnish]"), "should contain [omnish] prefix");
        assert!(row.contains("Daemon not connected"), "should contain error message");
    }

    #[test]
    fn test_dismiss_restores_cursor() {
        let cols: u16 = 40;
        let mut output = String::new();

        // Simulate: some text on screen, then prompt, then dismiss
        output.push_str("user@host:~$ ");   // shell prompt at row 0
        output.push_str(&render_prompt(cols)); // saves cursor, draws separator + ❯
        output.push_str(&render_dismiss());    // restores cursor, clears below

        let parser = parse_ansi(&output, cols, 10);
        let screen = parser.screen();

        // After dismiss, the cursor should be back at row 0 (where shell prompt is)
        let cursor = screen.cursor_position();
        assert_eq!(cursor.0, 0, "cursor should be restored to original row");

        // The separator and ❯ should be cleared (rows below cursor)
        let row1 = get_row(screen, 1, cols);
        assert!(!row1.contains('─'), "separator should be cleared after dismiss");
        let row2 = get_row(screen, 2, cols);
        assert!(!row2.contains('❯'), "prompt should be cleared after dismiss");
    }

    /// When the cursor is near the bottom of the terminal, render_prompt's \r\n
    /// causes scrolling. After scrolling, DECSC/DECRC (\x1b7/\x1b8) restores to
    /// the wrong row because the absolute row number no longer points to the
    /// original content. This test verifies dismiss clears all omnish UI even
    /// after scrolling.
    #[test]
    fn test_dismiss_clears_after_scroll() {
        let cols = 40u16;
        let rows = 5u16;
        let mut output = String::new();

        // Move cursor to the last row by outputting newlines
        for _ in 0..rows - 1 {
            output.push_str("\r\n");
        }
        output.push_str("$ "); // shell prompt on last row (row 4)

        // render_prompt emits 2 \r\n sequences — both cause scrolling
        output.push_str(&render_prompt(cols));

        // User types a query
        output.push_str(&render_input_echo(b"hello"));

        // ESC → dismiss
        output.push_str(&render_dismiss());

        let parser = parse_ansi(&output, cols, rows);
        let screen = parser.screen();
        let all_text = screen.contents();

        // All omnish UI elements must be cleared
        assert!(!all_text.contains("hello"), "typed text should be cleared after dismiss");
        assert!(!all_text.contains("❯"), "prompt ❯ should be cleared after dismiss");
    }

    /// Dismiss must NOT clear the original shell prompt line. It should only
    /// clear the separator and ❯ lines below it. This allows the caller to
    /// use SIGWINCH to redraw the prompt in place instead of sending `\r` to
    /// PTY (which would echo `\r\n` and add a blank line).
    #[test]
    fn test_dismiss_preserves_prompt_line() {
        let cols: u16 = 40;
        let prompt = "user@host:~$ ";
        let mut output = String::new();

        output.push_str(prompt);                      // shell prompt at row 0
        output.push_str(&render_prompt(cols));         // separator + ❯
        output.push_str(&render_input_echo(b"hello")); // user types
        output.push_str(&render_dismiss());            // ESC

        let parser = parse_ansi(&output, cols, 10);
        let screen = parser.screen();

        // Shell prompt text must still be visible on row 0
        let row0 = get_row(screen, 0, cols);
        assert!(
            row0.starts_with("user@host:~$"),
            "shell prompt text should be preserved after dismiss, got: {:?}", row0
        );

        // Cursor should be on the prompt row
        let cursor = screen.cursor_position();
        assert_eq!(cursor.0, 0, "cursor should be on the prompt row");
    }

    /// Simulates 5 cycles of : → type → ESC. After each dismiss, the shell
    /// redraws its prompt in place (SIGWINCH response = `\r{prompt}`, no `\n`).
    /// Original screen content must not drift upward.
    #[test]
    fn test_repeated_dismiss_no_content_drift() {
        let cols = 40u16;
        let rows = 10u16;
        let prompt = "$ ";

        let mut output = String::new();
        output.push_str("previous command output\r\n");
        output.push_str("more output here\r\n");
        output.push_str(prompt); // shell prompt on row 2

        for _ in 0..5 {
            output.push_str(&render_prompt(cols));
            output.push_str(&render_input_echo(b"test"));
            output.push_str(&render_dismiss());
            // Simulate shell redraw after SIGWINCH (re-outputs prompt in place)
            output.push_str(&format!("\r{}", prompt));
        }

        let parser = parse_ansi(&output, cols, rows);
        let screen = parser.screen();

        // Cursor must stay on the same row as the initial prompt
        let cursor = screen.cursor_position();
        assert_eq!(cursor.0, 2, "prompt should stay on row 2 after 5 dismiss cycles");

        // Original content must still be visible
        let all_text = screen.contents();
        assert!(all_text.contains("previous command output"), "original content should not drift off screen");
        assert!(all_text.contains("more output here"), "original content should not drift off screen");

        // No omnish UI remnants
        assert!(!all_text.contains("test"), "typed text should be cleared");
        assert!(!all_text.contains("❯"), "omnish prompt should be cleared");
    }

    /// Dismiss + CHA column restore should place the cursor exactly at the
    /// saved column on the prompt line (simulating the full cancel flow).
    #[test]
    fn test_dismiss_with_column_restore() {
        let cols: u16 = 40;
        let prompt = "user@host:~$ ";
        let saved_col = prompt.len() as u16; // 13

        let mut output = String::new();
        output.push_str(prompt);                       // row 0, cursor at (0, 13)
        output.push_str(&render_prompt(cols));          // separator + ❯
        output.push_str(&render_input_echo(b"hello"));  // user types
        output.push_str(&render_dismiss());             // ESC — cursor at (0, 0)
        // CHA to restore column (1-indexed)
        output.push_str(&format!("\x1b[{}G", saved_col + 1));

        let parser = parse_ansi(&output, cols, 10);
        let screen = parser.screen();

        let cursor = screen.cursor_position();
        assert_eq!(cursor.0, 0, "cursor should be on prompt row");
        assert_eq!(cursor.1, saved_col, "cursor should be at saved column");

        let row0 = get_row(screen, 0, cols);
        assert!(row0.starts_with("user@host:~$"), "prompt text preserved");
    }

    #[test]
    fn test_full_interaction_flow() {
        // Simulate a complete :: interaction flow
        let cols: u16 = 80;
        let mut full_output = String::new();

        // 1. User types "::" -> prompt appears
        full_output.push_str(&render_prompt(cols));

        // 2. User types "why did it fail" -> input echo
        full_output.push_str(&render_input_echo(b"why did it fail"));

        // 3. User presses Enter -> thinking status
        full_output.push_str(&render_thinking());

        // 4. LLM responds
        full_output.push_str(&render_response("The command failed because\nthe file was not found."));

        // 5. Closing separator
        full_output.push_str(&render_separator(cols));

        // Parse the entire flow
        let parser = parse_ansi(&full_output, cols, 10);
        let screen = parser.screen();

        // Verify key elements are visible in the final screen state
        let all_text = screen.contents();

        assert!(all_text.contains('❯'), "prompt symbol should be visible");
        assert!(all_text.contains("The command failed because"), "response first line should be visible");
        assert!(all_text.contains("the file was not found."), "response second line should be visible");
        assert!(all_text.contains('─'), "separator should be visible");
    }

    // --- Boundary tests ---

    #[test]
    fn test_render_input_echo_empty() {
        let output = render_input_echo(b"");
        let parser = parse_ansi(&output, 40, 24);
        let row = get_row(parser.screen(), 0, 40);
        assert!(row.contains('❯'), "should show ❯ prompt even with empty input");
        // After "❯ " there should be no other visible content
        let trimmed = row.trim_end();
        assert_eq!(trimmed, "❯", "empty input should show only ❯");
    }

    #[test]
    fn test_render_response_empty() {
        // Should not panic on empty content
        let output = render_response("");
        let parser = parse_ansi(&output, 80, 24);
        let screen = parser.screen();
        // The output is just color codes wrapping an empty string — no crash
        let _ = get_row(screen, 0, 80);
    }

    #[test]
    fn test_render_response_single_line() {
        let output = render_response("hello world");
        let parser = parse_ansi(&output, 80, 24);
        let screen = parser.screen();
        let row1 = get_row(screen, 1, 80);
        assert!(row1.contains("hello world"), "single line should render on row 1");
        // Row 2 should be empty (no spurious content)
        let row2 = get_row(screen, 2, 80);
        assert_eq!(row2.trim(), "", "row 2 should be empty for single-line response");
    }

    #[test]
    fn test_render_prompt_narrow_terminal() {
        let cols: u16 = 5;
        let output = render_prompt(cols);
        let parser = parse_ansi(&output, cols, 24);
        let screen = parser.screen();
        // Separator should be exactly 5 ─ characters wide
        let sep_row = get_row(screen, 1, cols);
        assert_eq!(
            sep_row.trim_end().chars().count(),
            cols as usize,
            "separator should be exactly {cols} chars wide"
        );
        // Prompt row should still contain ❯
        let prompt_row = get_row(screen, 2, cols);
        assert!(prompt_row.contains('❯'), "prompt should render even in narrow terminal");
    }

    #[test]
    fn test_render_error_special_chars() {
        let output = render_error("Error: <>&\"' chars");
        let parser = parse_ansi(&output, 80, 24);
        let screen = parser.screen();
        let row = get_row(screen, 1, 80);
        assert!(row.contains("<>&"), "special chars should be preserved verbatim");
        assert!(row.contains("\"'"), "quote chars should be preserved verbatim");
    }

    #[test]
    fn test_render_ghost_text() {
        let output = render_ghost_text("ug context");
        let parser = parse_ansi(&output, 40, 24);
        let screen = parser.screen();
        let row = get_row(screen, 0, 40);
        assert!(row.contains("ug context"), "ghost text should be visible");
        // Cursor should be at column 0 (restored to start by \x1b8)
        let cursor = screen.cursor_position();
        assert_eq!(cursor.1, 0, "cursor should be restored to saved position");
    }

    #[test]
    fn test_render_ghost_text_empty() {
        let output = render_ghost_text("");
        assert!(output.is_empty(), "empty ghost should produce no output");
    }

    #[test]
    fn test_input_echo_with_ghost() {
        let cols: u16 = 40;
        let mut output = String::new();
        output.push_str(&render_input_echo(b"/deb"));
        output.push_str(&render_ghost_text("ug"));

        let parser = parse_ansi(&output, cols, 24);
        let screen = parser.screen();
        let row = get_row(screen, 0, cols);
        assert!(row.contains("/deb"), "input text should be visible");
        assert!(row.contains("ug"), "ghost text should be visible");
        // Cursor should be right after "/deb" (col = 2 for "❯ " + 4 for "/deb" = 6)
        let cursor = screen.cursor_position();
        assert_eq!(cursor.1, 6, "cursor should be after real input, not ghost");
    }

    /// Regression test: ghost text must be erased when user types divergent input.
    ///
    /// Simulates the shell completion flow:
    /// 1. Shell prompt with "cargo" typed, ghost " run" shown
    /// 2. User types " test" (diverges from ghost)
    /// 3. \x1b[K (erase to EOL) clears the stale ghost
    /// 4. Shell echoes " test" at cursor position
    ///
    /// Without \x1b[K, the ghost text " run" would remain visible after "cargo test".
    #[test]
    fn test_ghost_cleared_on_divergent_input() {
        let cols: u16 = 40;
        let mut output = String::new();

        // Step 1: shell echoes "cargo" at column 0, cursor at col 5
        output.push_str("cargo");
        // Ghost " run" rendered after cursor (save + dim + restore)
        output.push_str(&render_ghost_text(" run"));

        // Step 2: user types divergent input — the fix sends \x1b[K to erase ghost
        output.push_str("\x1b[K");

        // Step 3: shell echoes " test" at cursor position (col 5)
        output.push_str(" test");

        let parser = parse_ansi(&output, cols, 24);
        let screen = parser.screen();
        let row = get_row(screen, 0, cols);

        // "cargo test" should be visible
        assert!(row.contains("cargo test"), "divergent input should be visible");
        // Ghost text " run" must NOT be visible
        assert!(!row.contains("run"), "ghost text should be erased after divergent input");
    }
}
