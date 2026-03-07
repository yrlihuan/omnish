// crates/omnish-client/src/picker.rs
//
// Pure rendering functions for the picker widget (single-select and multi-select).
// All functions return a String suitable for writing to a raw-mode terminal (using \r\n).

/// Get terminal width, fallback to 80.
fn terminal_cols() -> u16 {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 && ws.ws_col > 0 { ws.ws_col } else { 80 }
}

/// Separator line spanning `cols` columns (dim horizontal-rule characters).
fn render_separator(cols: u16) -> String {
    format!("\r\x1b[2m{}\x1b[0m", "\u{2500}".repeat(cols as usize))
}

/// Render a single item line.
/// - `selected`: this is the cursor row (render with `> ` prefix + bold + reverse video)
/// - `checked`: only used in multi mode (render `[x]` or `[ ]`)
/// - `multi`: whether to show checkboxes
fn render_item(text: &str, selected: bool, checked: bool, multi: bool) -> String {
    let prefix = if selected { "> " } else { "  " };
    let checkbox = if multi {
        if checked { "[x] " } else { "[ ] " }
    } else {
        ""
    };
    if selected {
        format!("\r\x1b[1;7m{}{}{}\x1b[0m\x1b[K", prefix, checkbox, text)
    } else {
        format!("\r{}{}{}\x1b[K", prefix, checkbox, text)
    }
}

/// Render the hint line at the bottom.
fn render_hint(multi: bool) -> String {
    let hint = if multi {
        "\u{2191}\u{2193} move  Space select  Enter confirm  ESC cancel"
    } else {
        "\u{2191}\u{2193} move  Enter confirm  ESC cancel"
    };
    format!("\r\x1b[2m{}\x1b[0m\x1b[K", hint)
}

/// Render the full picker widget (initial draw).
fn render_full(title: &str, items: &[&str], cursor: usize, checked: &[bool], multi: bool, cols: u16) -> String {
    let total_lines = 1 + 1 + items.len() + 1 + 1; // title + sep + items + sep + hint
    let mut out = String::new();

    // Push screen content up by printing N blank lines
    for _ in 0..total_lines {
        out.push_str("\r\n");
    }
    // Move cursor back up
    out.push_str(&format!("\x1b[{}A", total_lines));

    // Title
    out.push_str(&format!("\r\x1b[1m{}\x1b[0m\x1b[K", title));
    out.push_str("\r\n");

    // Top separator
    out.push_str(&render_separator(cols));
    out.push_str("\r\n");

    // Items
    for (i, item) in items.iter().enumerate() {
        out.push_str(&render_item(item, i == cursor, checked[i], multi));
        if i < items.len() - 1 {
            out.push_str("\r\n");
        }
    }
    out.push_str("\r\n");

    // Bottom separator
    out.push_str(&render_separator(cols));
    out.push_str("\r\n");

    // Hint
    out.push_str(&render_hint(multi));

    out
}

/// Render cleanup: move cursor to title line and erase everything below.
fn render_cleanup(items_len: usize) -> String {
    let total_lines = 1 + 1 + items_len + 1 + 1;
    let up = total_lines - 1;
    format!("\x1b[{}A\r\x1b[J", up)
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
    fn test_render_item_normal() {
        let output = render_item("Option A", false, false, false);
        assert!(output.contains("  "), "non-selected item should have '  ' prefix");
        assert!(output.contains("Option A"), "should contain the item text");
        // Should NOT contain bold/reverse escape
        assert!(!output.contains("\x1b[1;7m"), "non-selected item should not be bold+reverse");
    }

    #[test]
    fn test_render_item_selected() {
        let output = render_item("Option B", true, false, false);
        assert!(output.contains("> "), "selected item should have '> ' prefix");
        assert!(output.contains("Option B"), "should contain the item text");
        // Should contain bold+reverse escape
        assert!(output.contains("\x1b[1;7m"), "selected item should be bold+reverse");
    }

    #[test]
    fn test_render_item_multi_checked() {
        let output = render_item("Checked item", false, true, true);
        assert!(output.contains("[x]"), "checked item in multi mode should show [x]");
        assert!(output.contains("Checked item"), "should contain the item text");
    }

    #[test]
    fn test_render_item_multi_unchecked() {
        let output = render_item("Unchecked item", true, false, true);
        assert!(output.contains("[ ]"), "unchecked item in multi mode should show [ ]");
        assert!(output.contains("> "), "selected item should have '> ' prefix");
        assert!(output.contains("Unchecked item"), "should contain the item text");
    }

    #[test]
    fn test_render_hint_single() {
        let output = render_hint(false);
        assert!(output.contains("Enter confirm"), "single mode hint should contain 'Enter confirm'");
        assert!(!output.contains("Space"), "single mode hint should NOT contain 'Space'");
    }

    #[test]
    fn test_render_hint_multi() {
        let output = render_hint(true);
        assert!(output.contains("Space select"), "multi mode hint should contain 'Space select'");
        assert!(output.contains("Enter confirm"), "multi mode hint should contain 'Enter confirm'");
    }

    #[test]
    fn test_render_full_single_select() {
        let cols: u16 = 60;
        let items = vec!["Alpha", "Beta", "Gamma"];
        let checked = vec![false, false, false];
        let output = render_full("Pick one:", &items, 1, &checked, false, cols);

        // Use a tall terminal to accommodate the blank lines pushed by render_full
        let total_lines = 1 + 1 + items.len() + 1 + 1; // 7
        let rows = (total_lines + total_lines) as u16; // plenty of room
        let parser = parse_ansi(&output, cols, rows);
        let screen = parser.screen();

        let all_text = screen.contents();
        assert!(all_text.contains("Pick one:"), "should display the title");
        assert!(all_text.contains("Alpha"), "should display first item");
        assert!(all_text.contains("Beta"), "should display second item");
        assert!(all_text.contains("Gamma"), "should display third item");
        assert!(all_text.contains("\u{2500}"), "should display separator");

        // The selected item (Beta at index 1) should have "> " prefix
        // Find the row that contains "Beta"
        let mut beta_row = String::new();
        for r in 0..rows {
            let row = get_row(screen, r, cols);
            if row.contains("Beta") {
                beta_row = row;
                break;
            }
        }
        assert!(beta_row.contains(">"), "selected item (Beta) should have > prefix");

        // Non-selected items should NOT have ">"
        let mut alpha_row = String::new();
        for r in 0..rows {
            let row = get_row(screen, r, cols);
            if row.contains("Alpha") {
                alpha_row = row;
                break;
            }
        }
        assert!(!alpha_row.contains(">"), "non-selected item (Alpha) should not have > prefix");
    }

    #[test]
    fn test_render_full_multi_select() {
        let cols: u16 = 60;
        let items = vec!["First", "Second"];
        let checked = vec![true, false];
        let output = render_full("Select items:", &items, 0, &checked, true, cols);

        let rows = 20u16;
        let parser = parse_ansi(&output, cols, rows);
        let screen = parser.screen();

        let all_text = screen.contents();
        assert!(all_text.contains("[x]"), "checked item should show [x]");
        assert!(all_text.contains("[ ]"), "unchecked item should show [ ]");
        assert!(all_text.contains("First"), "should display first item");
        assert!(all_text.contains("Second"), "should display second item");
        assert!(all_text.contains("Space select"), "multi mode should show Space in hint");
    }

    #[test]
    fn test_render_cleanup_erases_widget() {
        let cols: u16 = 60;
        let items = vec!["One", "Two", "Three"];
        let checked = vec![false, false, false];

        // Render the full picker, then clean it up
        let mut output = render_full("Title:", &items, 0, &checked, false, cols);
        output.push_str(&render_cleanup(items.len()));

        let rows = 20u16;
        let parser = parse_ansi(&output, cols, rows);
        let screen = parser.screen();

        let all_text = screen.contents();
        // After cleanup, all widget content should be erased
        assert!(!all_text.contains("Title:"), "title should be erased after cleanup");
        assert!(!all_text.contains("One"), "items should be erased after cleanup");
        assert!(!all_text.contains("Two"), "items should be erased after cleanup");
        assert!(!all_text.contains("Three"), "items should be erased after cleanup");
        assert!(!all_text.contains("confirm"), "hint should be erased after cleanup");
    }
}
