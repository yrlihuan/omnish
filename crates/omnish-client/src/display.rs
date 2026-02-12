// crates/omnish-client/src/display.rs
//
// Pure functions that produce ANSI terminal output strings for the :: interactive mode.
// All functions return a String suitable for writing to a raw-mode terminal (using \r\n).

/// Render a separator line spanning `cols` columns (dim ─ characters).
pub fn render_separator(cols: u16) -> String {
    format!("\x1b[2m{}\x1b[0m", "─".repeat(cols as usize))
}

/// Render the initial prompt: newline, separator, newline, ❯ cursor.
pub fn render_prompt(cols: u16) -> String {
    let separator = render_separator(cols);
    format!("\r\n{}\r\n\x1b[36m❯\x1b[0m ", separator)
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
/// - Wraps in green color
pub fn render_response(content: &str) -> String {
    let formatted: String = content
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\r\n");
    format!("\x1b[32m{}\x1b[0m\r\n", formatted)
}

/// Format an error message in red.
pub fn render_error(msg: &str) -> String {
    format!("\r\n\x1b[31m[omnish] {}\x1b[0m\r\n", msg)
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
        let row0 = get_row(screen, 0, 80);
        assert!(row0.contains("line one"), "first line should render");
        let row1 = get_row(screen, 1, 80);
        assert!(row1.contains("line two"), "second line should render on next row");
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
}
