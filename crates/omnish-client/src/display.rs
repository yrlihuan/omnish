// crates/omnish-client/src/display.rs
//
// Pure functions that produce ANSI terminal output strings for the :: interactive mode.
// All functions return a String suitable for writing to a raw-mode terminal (using NEWLINE).

// ── ANSI style constants ─────────────────────────────────────────────
/// Line separator for raw-mode terminal output.
/// LF moves the cursor down; CHA (CSI G) explicitly repositions to column 1.
/// Use this instead of a bare CR+LF sequence: some terminal stacks (observed
/// in ConEmu + tmux) occasionally drop the CR byte, leaving the next write
/// aligned to the previous cursor column. CHA is a well-defined CSI and is
/// not subject to that bug.
pub const NEWLINE: &str = "\n\x1b[G";
/// Reset all text attributes.
pub const RESET: &str = "\x1b[0m";
/// Bold text.
pub const BOLD: &str = "\x1b[1m";
/// Dim gray text - combines SGR dim (2) and bright-black (90) so text
/// appears dimmed on terminals that support the dim attribute, and still
/// renders as gray on terminals that ignore it.
pub const DIM: &str = "\x1b[2;90m";
/// Bold + reverse video - used for selected/highlighted items.
pub const BOLD_REVERSE: &str = "\x1b[1;7m";
/// Red - errors.
pub const RED: &str = "\x1b[31m";
/// Green - success, ON toggles.
pub const GREEN: &str = "\x1b[32m";
/// Yellow - warnings, change arrows.
pub const YELLOW: &str = "\x1b[33m";
/// Cyan - prompts, user input prefix, links.
pub const CYAN: &str = "\x1b[36m";
/// White - fallback text color.
pub const WHITE: &str = "\x1b[37m";
/// Background black - fallback for dark backgrounds.
pub const BG_BLACK: &str = "\x1b[40m";
/// Bright-black (gray) - secondary info, OFF toggles, values.
pub const GRAY: &str = "\x1b[90m";
/// Bright white - assistant bullets, spinner.
pub const BRIGHT_WHITE: &str = "\x1b[97m";

/// Character types that have extended Unicode variants.
pub enum UiChar {
    /// ⎿ (extended) / └ (fallback) - tool output prefix
    ToolOutputCorner,
}

/// Return the appropriate character for the given UI element.
pub fn ui_char(char_type: UiChar, extended_unicode: bool) -> &'static str {
    match char_type {
        UiChar::ToolOutputCorner => if extended_unicode { "⎿" } else { "└" },
    }
}

/// Truncate a string to fit within `max_cols` display columns.
/// CJK / fullwidth characters count as 2 columns.
/// Appends "…" if truncated.
pub fn truncate_cols(s: &str, max_cols: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if max_cols == 0 {
        return String::new();
    }
    // First check if the string fits entirely (skip ANSI escape sequences)
    if display_width(s) <= max_cols {
        return s.to_string();
    }
    // Doesn't fit - truncate, reserving 1 column for "…"
    let limit = max_cols.saturating_sub(1);
    let mut width = 0usize;
    let mut end = 0usize;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Keep ANSI escape sequence in output but don't count its width
            end += ch.len_utf8();
            for c in chars.by_ref() {
                end += c.len_utf8();
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            let w = ch.width().unwrap_or(0);
            if width + w > limit {
                break;
            }
            width += w;
            end += ch.len_utf8();
        }
    }
    format!("{}…", &s[..end])
}

/// Compute the display width of a string, stripping ANSI escape sequences.
/// CJK / fullwidth characters count as 2 columns.
pub fn display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    let mut width = 0usize;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip ANSI escape sequence
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            width += ch.width().unwrap_or(0);
        }
    }
    width
}

/// Build an ANSI sequence that erases `n` visual rows from the current cursor
/// position upward, then moves the cursor one more row up so it returns to
/// the position it occupied before those rows were rendered.
///
/// Rendering convention: content is printed below the cursor with `{NEWLINE}`
/// prefixes, so N rows of output occupy rows R+1 … R+N (where R is the
/// cursor row before the first `{NEWLINE}`).  After rendering, the cursor sits
/// at row R+N.  This function clears each row from bottom to top, then
/// moves up one more to return to row R.
///
/// Returns an empty string when `n == 0`.
pub fn erase_lines(n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let mut out = String::new();
    for i in 0..n {
        if i > 0 {
            out.push_str("\x1b[1A"); // cursor up
        }
        out.push_str("\r\x1b[K"); // CR + erase to EOL
    }
    out.push_str("\x1b[1A"); // one more up to return to origin
    out
}

/// Render a plain separator line spanning `cols` columns (dim ─ characters).
pub fn render_separator_plain(cols: u16) -> String {
    format!("{DIM}{}{RESET}", "─".repeat(cols as usize))
}

/// Render a separator line spanning `cols` columns (dim ─ characters),
/// with a `ctrl+o` hint embedded on the right side.
pub fn render_separator(cols: u16) -> String {
    let hint = " ctrl+o to expand ";
    let cols = cols as usize;
    if cols > hint.len() + 4 {
        let right_dashes = 2;
        let left_dashes = cols - hint.len() - right_dashes;
        format!(
            "{DIM}{}{}{}{RESET}",
            "─".repeat(left_dashes),
            hint,
            "─".repeat(right_dashes),
        )
    } else {
        render_separator_plain(cols as u16)
    }
}

/// Render the initial prompt: newline, separator, newline, ❯ cursor.
/// The omnish UI occupies exactly 2 lines below the original cursor position
/// (separator line + ❯ input line). `render_dismiss()` relies on this count.
#[cfg(test)]
pub fn render_prompt(cols: u16) -> String {
    let separator = render_separator(cols);
    format!("{NEWLINE}{}{NEWLINE}{CYAN}❯{RESET} ", separator)
}

/// Dismiss the omnish UI by clearing only the separator and ❯ lines below
/// the shell prompt, then moving the cursor back to the prompt line.
#[cfg(test)]
pub fn render_dismiss() -> String {
    "\x1b[1A\r\x1b[J\x1b[1A".to_string()
}

/// Render the input echo line: moves cursor to column 0, prints ❯ followed by user text,
/// then clears to end of line (to handle backspace correctly).
#[cfg(test)]
pub fn render_input_echo(user_input: &[u8]) -> String {
    format!(
        "\r{CYAN}❯{RESET} {}\x1b[K",
        String::from_utf8_lossy(user_input)
    )
}

/// Format an LLM response for raw-mode display.
/// Renders markdown to ANSI-styled terminal output with {NEWLINE} line endings.
pub fn render_response(content: &str) -> String {
    let rendered = super::markdown::render(content);
    format!("{NEWLINE}{}{NEWLINE}", rendered)
}

/// Format an error message in red.
pub fn render_error(msg: &str) -> String {
    format!("{NEWLINE}{RED}[omnish] {}{RESET}{NEWLINE}", msg)
}

/// Render ghost text (completion suggestion) in dim gray after the cursor.
/// Uses save/restore cursor so the cursor stays at the real input position.
/// Returns empty string if ghost is empty.
pub fn render_ghost_text(ghost: &str) -> String {
    if ghost.is_empty() {
        return String::new();
    }
    format!("\x1b7{DIM}{}{RESET}\x1b8", ghost)
}

/// Erase ghost text from the terminal.
///
/// When ghost text wraps to the next line (prompt + input + ghost > terminal
/// width), a plain `\x1b[K` only clears the current line.  The wrapped
/// portion on the next line remains as a stale artifact.
///
/// This function erases the current line from the cursor **and** one line
/// below, then restores the cursor position so the prompt is untouched.
/// `wrapped` should be `true` when the caller knows the ghost text crossed
/// a line boundary; when `false`, only `\x1b[K` is emitted.
pub fn erase_ghost_text(wrapped: bool) -> &'static [u8] {
    if wrapped {
        // \x1b[K   - erase from cursor to end of current line
        // \x1b[1B  - move cursor down one line
        // \r\x1b[K - move to column 0, erase that line
        // \x1b[1A  - move cursor back up one line
        b"\x1b[K\x1b[1B\r\x1b[K\x1b[1A"
    } else {
        b"\x1b[K"
    }
}

/// Spinner frames for running tool status animation.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Get the spinner character for a given frame index.
pub fn spinner_char(frame: usize) -> char {
    SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]
}

fn status_icon_str(icon: &omnish_protocol::message::StatusIcon, spinner_frame: Option<usize>) -> String {
    use omnish_protocol::message::StatusIcon;
    match icon {
        StatusIcon::Running => {
            let ch = spinner_char(spinner_frame.unwrap_or(0));
            format!("{BRIGHT_WHITE}{}{RESET}", ch)
        }
        StatusIcon::Success => format!("{}\x1b[38;5;114m●\x1b[0m", GREEN),
        StatusIcon::Error => format!("{}\x1b[38;5;211m●\x1b[0m", RED),
    }
}

pub fn render_tool_header(icon: &omnish_protocol::message::StatusIcon, display_name: &str, param_desc: &str, max_cols: usize) -> String {
    render_tool_header_with_spinner(icon, display_name, param_desc, max_cols, None)
}

pub fn render_tool_header_with_spinner(icon: &omnish_protocol::message::StatusIcon, display_name: &str, param_desc: &str, max_cols: usize, spinner_frame: Option<usize>) -> String {
    let icon_str = status_icon_str(icon, spinner_frame);
    let oneline = collapse_newlines(param_desc);
    let name_cols = display_name.len() + 2;
    let available = max_cols.saturating_sub(4 + name_cols);
    let truncated = truncate_cols(&oneline, available);
    format!("{} {BOLD}{}{RESET}{DIM}({}){RESET}", icon_str, display_name, truncated)
}

pub fn render_tool_header_full(icon: &omnish_protocol::message::StatusIcon, display_name: &str, param_desc: &str) -> String {
    render_tool_header_full_with_spinner(icon, display_name, param_desc, None)
}

pub fn render_tool_header_full_with_spinner(icon: &omnish_protocol::message::StatusIcon, display_name: &str, param_desc: &str, spinner_frame: Option<usize>) -> String {
    let icon_str = status_icon_str(icon, spinner_frame);
    let oneline = collapse_newlines(param_desc);
    format!("{} {BOLD}{}{RESET}{DIM}({}){RESET}", icon_str, display_name, oneline)
}

/// Collapse newlines (and surrounding whitespace) into a single space.
fn collapse_newlines(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_was_newline = false;
    for ch in s.chars() {
        if ch == '\n' || ch == '\r' {
            if !prev_was_newline && !result.is_empty() {
                result.push(' ');
            }
            prev_was_newline = true;
        } else if prev_was_newline && ch == ' ' {
            // skip leading space after newline collapse
        } else {
            prev_was_newline = false;
            result.push(ch);
        }
    }
    result.trim_end().to_string()
}

pub fn render_tool_output(lines: &[String], extended_unicode: bool) -> Vec<String> {
    render_tool_output_with_cols(lines, 0, extended_unicode)
}

/// Render tool output lines with optional column-width limit.
/// If `max_cols > 0`, content that would exceed 3 terminal rows is truncated with "…".
pub fn render_tool_output_with_cols(lines: &[String], max_cols: usize, extended_unicode: bool) -> Vec<String> {
    let prefix_width = 5; // "  ⎿  " / "  └  " or "     "
    let corner = ui_char(UiChar::ToolOutputCorner, extended_unicode);
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let content = if max_cols > 0 {
            let avail = max_cols.saturating_sub(prefix_width);
            let max_content = avail * 3; // 3 terminal rows
            truncate_cols(line, max_content)
        } else {
            line.clone()
        };
        if i == 0 {
            out.push(format!("  {DIM}{corner}{RESET}  {}", content));
        } else {
            out.push(format!("     {}", content));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_cols_ascii_fits() {
        assert_eq!(truncate_cols("hello", 10), "hello");
        assert_eq!(truncate_cols("hello", 5), "hello");
    }

    #[test]
    fn truncate_cols_ascii_truncated() {
        assert_eq!(truncate_cols("hello world", 8), "hello w…");
    }

    #[test]
    fn truncate_cols_cjk_fits() {
        assert_eq!(truncate_cols("你好", 4), "你好");
    }

    #[test]
    fn truncate_cols_cjk_truncated() {
        // "你好世界" = 8 cols, limit to 6 → "你好…" (4+1=5 cols)
        assert_eq!(truncate_cols("你好世界", 6), "你好…");
    }

    #[test]
    fn truncate_cols_mixed() {
        // "ab你好cd" = 2+4+2 = 8 cols, limit to 6 → "ab你…" (2+2+1=5)
        assert_eq!(truncate_cols("ab你好cd", 6), "ab你…");
    }

    #[test]
    fn truncate_cols_empty() {
        assert_eq!(truncate_cols("hello", 0), "");
        assert_eq!(truncate_cols("", 10), "");
    }

    #[test]
    fn truncate_cols_ansi_fits() {
        // "\x1b[1mhello\x1b[0m world" - visible: "hello world" = 11 cols
        let s = "\x1b[1mhello\x1b[0m world";
        assert_eq!(truncate_cols(s, 11), s);
        assert_eq!(truncate_cols(s, 20), s);
    }

    #[test]
    fn truncate_cols_ansi_truncated() {
        // visible: "hello world" = 11 cols, truncate to 8 → "hello w…"
        let s = "\x1b[1mhello\x1b[0m world";
        assert_eq!(truncate_cols(s, 8), "\x1b[1mhello\x1b[0m w…");
    }

    #[test]
    fn truncate_cols_ansi_heavy() {
        // Many ANSI codes, visible content is short
        let s = "\x1b[1m\x1b[33mA\x1b[0m\x1b[2;90mB\x1b[0m";
        // visible: "AB" = 2 cols
        assert_eq!(truncate_cols(s, 2), s);
        assert_eq!(truncate_cols(s, 1), "\x1b[1m\x1b[33m…");
    }

    #[test]
    fn truncate_cols_ansi_cjk() {
        // ANSI + CJK: "\x1b[33m你好世界\x1b[0m" - visible: 8 cols
        let s = "\x1b[33m你好世界\x1b[0m";
        assert_eq!(truncate_cols(s, 8), s);
        assert_eq!(truncate_cols(s, 6), "\x1b[33m你好…");
    }

    #[test]
    fn collapse_newlines_multiline_command() {
        assert_eq!(
            collapse_newlines("git add file && git commit -m \"fix\n\nIssue #414\""),
            "git add file && git commit -m \"fix Issue #414\""
        );
        assert_eq!(collapse_newlines("single line"), "single line");
        assert_eq!(collapse_newlines("line1\n  line2"), "line1 line2");
        assert_eq!(collapse_newlines("a\n\n\nb"), "a b");
        assert_eq!(collapse_newlines(""), "");
    }

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
        // Row 1 (0-indexed) should contain the separator (row 0 is blank from {NEWLINE})
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

        // Verify raw string contains {NEWLINE} (raw mode requirement)
        assert!(output.contains(NEWLINE), "response must use \\r\\n for raw mode");
        // Should not contain bare \n (without preceding \r)
        let without_cr = output.replace(NEWLINE, "");
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
    fn test_separator() {
        let cols: u16 = 60;
        let output = render_separator(cols);
        let parser = parse_ansi(&output, cols, 24);
        let screen = parser.screen();
        let row = get_row(screen, 0, cols);
        let trimmed = row.trim_end();
        assert_eq!(trimmed.chars().count(), cols as usize, "separator should be exactly cols wide");
        assert!(trimmed.contains("ctrl+o"), "separator should contain ctrl+o hint");
        assert!(trimmed.contains('─'), "separator should contain ─ dashes");
    }

    #[test]
    fn test_error_message() {
        let output = render_error("Daemon not connected");
        let parser = parse_ansi(&output, 80, 24);
        let screen = parser.screen();

        // Error should appear on row 1 (row 0 is blank from leading {NEWLINE})
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

    /// When the cursor is near the bottom of the terminal, render_prompt's {NEWLINE}
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
            output.push_str(NEWLINE);
        }
        output.push_str("$ "); // shell prompt on last row (row 4)

        // render_prompt emits 2 {NEWLINE} sequences - both cause scrolling
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
    /// PTY (which would echo `{NEWLINE}` and add a blank line).
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
        output.push_str(&format!("previous command output{NEWLINE}"));
        output.push_str(&format!("more output here{NEWLINE}"));
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
        output.push_str(&render_dismiss());             // ESC - cursor at (0, 0)
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

        // 3. User presses Enter -> thinking status (shown then cleared before response)
        // LineStatus is tested separately; here we just omit it to keep the flow test clean.

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
        // The output is just color codes wrapping an empty string - no crash
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

        // Step 2: user types divergent input - the fix sends \x1b[K to erase ghost
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

    // -- erase_lines tests --

    #[test]
    fn erase_lines_zero_is_empty() {
        assert_eq!(erase_lines(0), "");
    }

    #[test]
    fn erase_lines_one_clears_and_moves_up() {
        let seq = erase_lines(1);
        // Should clear 1 row then move up once
        assert_eq!(seq, "\r\x1b[K\x1b[1A");
    }

    #[test]
    fn erase_lines_three() {
        let seq = erase_lines(3);
        // row 3: \r\x1b[K], up, row 2: \r\x1b[K], up, row 1: \r\x1b[K], final up
        let expected = "\r\x1b[K\x1b[1A\r\x1b[K\x1b[1A\r\x1b[K\x1b[1A";
        assert_eq!(seq, expected);
    }

    /// vt100: erase_lines(3) after rendering 3 lines leaves the screen blank.
    #[test]
    fn vt100_erase_lines_clears_all_rows() {
        let cols = 40u16;
        let mut parser = vt100::Parser::new(10, cols, 0);
        // Render 3 lines below cursor (matching render_seq pattern)
        parser.process(b"\r\n\x1b[Kline one");
        parser.process(b"\r\n\x1b[Kline two");
        parser.process(b"\r\n\x1b[Kline three");
        // Erase
        parser.process(erase_lines(3).as_bytes());

        let all = parser.screen().contents();
        assert!(!all.contains("line"), "all lines should be erased: {all:?}");
    }
}
