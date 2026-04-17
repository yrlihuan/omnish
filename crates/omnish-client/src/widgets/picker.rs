// crates/omnish-client/src/picker.rs
//
// Pure rendering functions for the picker widget (single-select and multi-select).
// All functions return a String suitable for writing to a raw-mode terminal (using {NEWLINE}).
// Supports scrolling viewport when items exceed MAX_VISIBLE.

use std::os::unix::io::AsRawFd;

use super::common::{self, MAX_VISIBLE};
use crate::display::{BOLD, BOLD_REVERSE, NEWLINE, RESET};

/// Icon style for disabled items in the picker.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledIcon {
    /// 🔒 (U+1F512) - padlock emoji
    Lock,
    /// ⚿ (U+26BF) - squared key
    Key,
    /// ⊘ (U+2298) - circled division slash
    Forbidden,
}

impl DisabledIcon {
    fn as_str(self) -> &'static str {
        match self {
            DisabledIcon::Lock => "\u{1f512}",
            DisabledIcon::Key => "\u{26bf}",
            DisabledIcon::Forbidden => "\u{2298}",
        }
    }
}

fn terminal_cols() -> u16 { common::terminal_cols() }
fn render_separator(cols: u16) -> String { common::render_separator(cols) }

/// Render a single item line.
/// - `selected`: this is the cursor row (render with `> ` prefix + bold + reverse video)
/// - `checked`: only used in multi mode (render `[x]` or `[ ]`)
/// - `multi`: whether to show checkboxes
/// - `disabled_icon`: if Some, render dim with icon and block selection
fn render_item(text: &str, selected: bool, checked: bool, multi: bool, disabled_icon: Option<DisabledIcon>) -> String {
    let prefix = if selected { "> " } else { "  " };
    let checkbox = if multi {
        if checked { "[x] " } else { "[ ] " }
    } else {
        ""
    };
    if let Some(icon) = disabled_icon {
        // Dim style with icon, no reverse video even when cursor is on it
        format!("\r{}{}{}{} {}{}\x1b[K", crate::display::DIM, prefix, checkbox, icon.as_str(), text, crate::display::RESET)
    } else if selected {
        format!("\r{BOLD_REVERSE}{}{}{}{RESET}\x1b[K", prefix, checkbox, text)
    } else {
        format!("\r{}{}{}\x1b[K", prefix, checkbox, text)
    }
}

/// Render the hint line at the bottom, with optional scroll-down indicator.
fn render_hint(multi: bool, remaining_below: usize) -> String {
    let hint = if multi {
        crate::i18n::t("hint.picker_multi")
    } else {
        crate::i18n::t("hint.picker_single")
    };
    if remaining_below > 0 {
        let more = crate::i18n::tf("hint.more_below", &[("n", &remaining_below.to_string())]);
        format!("\r{}{}  ({}){}\x1b[K", crate::display::DIM, hint, more, crate::display::RESET)
    } else {
        format!("\r{}{}{}\x1b[K", crate::display::DIM, hint, crate::display::RESET)
    }
}

/// Number of items visible in the viewport.
fn visible_count(total: usize) -> usize {
    total.min(MAX_VISIBLE)
}

/// Render the full picker widget (initial draw or full redraw after scroll).
#[allow(clippy::too_many_arguments)]
fn render_full(
    title: &str,
    items: &[&str],
    cursor: usize,
    checked: &[bool],
    disabled: &[Option<DisabledIcon>],
    multi: bool,
    cols: u16,
    scroll_offset: usize,
) -> String {
    let vis = visible_count(items.len());
    let total_lines = 1 + 1 + vis + 1 + 1; // title + sep + visible items + sep + hint
    let mut out = String::new();

    // Push screen content up by printing N blank lines
    for _ in 0..total_lines {
        out.push_str(NEWLINE);
    }
    // Move cursor back up
    out.push_str(&format!("\x1b[{}A", total_lines));

    // Title (with scroll indicator if applicable)
    if scroll_offset > 0 {
        out.push_str(&format!(
            "\r{BOLD}{}{RESET} {}(\u{25b2} {} more){}\x1b[K",
            title, crate::display::DIM, scroll_offset, crate::display::RESET
        ));
    } else {
        out.push_str(&format!("\r{BOLD}{}{RESET}\x1b[K", title));
    }
    out.push_str(NEWLINE);

    // Top separator
    out.push_str(&render_separator(cols));
    out.push_str(NEWLINE);

    // Visible items
    let end = (scroll_offset + vis).min(items.len());
    for i in scroll_offset..end {
        out.push_str(&render_item(items[i], i == cursor, checked[i], multi, disabled[i]));
        if i < end - 1 {
            out.push_str(NEWLINE);
        }
    }
    out.push_str(NEWLINE);

    // Bottom separator (always full-width)
    let remaining_below = items.len().saturating_sub(end);
    out.push_str(&render_separator(cols));
    out.push_str(NEWLINE);

    // Hint (with scroll-down indicator if applicable)
    out.push_str(&render_hint(multi, remaining_below));

    out
}

/// Render cleanup: move cursor to title line and erase everything below.
fn render_cleanup(total_items: usize) -> String {
    let vis = visible_count(total_items);
    let total_lines = 1 + 1 + vis + 1 + 1;
    let up = total_lines - 1;
    format!("\x1b[{}A\r\x1b[J", up)
}

fn parse_esc_seq(stdin_fd: i32) -> Option<[u8; 2]> { common::parse_esc_seq(stdin_fd) }

/// Rewrite a single item line in-place (cursor must already be on that line).
fn redraw_item(text: &str, selected: bool, checked: bool, multi: bool, disabled_icon: Option<DisabledIcon>) {
    let line = render_item(text, selected, checked, multi, disabled_icon);
    common::write_stdout(line.as_bytes());
}

/// Extract shortcut key from item text.
/// Looks for `[X]` pattern where X is a single ASCII letter.
/// Returns the lowercase byte of the shortcut key.
fn extract_shortcut(text: &str) -> Option<u8> {
    let bytes = text.as_bytes();
    for i in 0..bytes.len().saturating_sub(2) {
        if bytes[i] == b'[' && bytes[i + 2] == b']' && bytes[i + 1].is_ascii_alphabetic() {
            return Some(bytes[i + 1].to_ascii_lowercase());
        }
    }
    None
}

/// Build shortcut key → item index map from items.
fn build_shortcut_map(items: &[&str]) -> std::collections::HashMap<u8, usize> {
    let mut map = std::collections::HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if let Some(key) = extract_shortcut(item) {
            map.entry(key).or_insert(i);
        }
    }
    map
}

/// Core picker loop. Returns selected index(es) or None on ESC.
/// `disabled_items` marks items that cannot be selected (shown dim with icon).
fn run_picker(title: &str, items: &[&str], multi: bool, initial_cursor: usize, disabled_items: &[Option<DisabledIcon>]) -> Option<Vec<usize>> {
    if items.is_empty() {
        return None;
    }

    let cols = terminal_cols();
    let mut cursor: usize = initial_cursor.min(items.len().saturating_sub(1));
    let vis = visible_count(items.len());
    let max_scroll = items.len().saturating_sub(vis);
    let mut scroll_offset: usize = cursor.saturating_sub(vis / 2).min(max_scroll);
    let mut checked = vec![false; items.len()];
    let shortcuts = build_shortcut_map(items);
    // Pad disabled to match items length
    let disabled: Vec<Option<DisabledIcon>> = (0..items.len())
        .map(|i| disabled_items.get(i).copied().flatten())
        .collect();

    // Hide cursor during picker interaction
    common::write_stdout(b"\x1b[?25l");

    // Initial render
    let full = render_full(title, items, cursor, &checked, &disabled, multi, cols, scroll_offset);
    common::write_stdout(full.as_bytes());

    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut byte = [0u8; 3];

    loop {
        let n = match nix::unistd::read(stdin_fd, &mut byte) {
            Ok(n) if n > 0 => n,
            _ => break,
        };
        match byte[0] {
                0x03 => {
                    // Ctrl+C - cancel
                    let cleanup = render_cleanup(items.len());
                    common::write_stdout(cleanup.as_bytes());
                    common::write_stdout(b"\x1b[?25h");
                    return None;
                }
                0x1b => {
                    // Arrow keys may arrive as a single 3-byte read (\x1b[A or \x1bOA),
                    // so check buf first before reading more from stdin.
                    // Support both CSI (\x1b[) and SS3 (\x1bO) cursor key encodings.
                    let seq = if n >= 3 && (byte[1] == b'[' || byte[1] == b'O') {
                        Some([b'[', byte[2]])
                    } else {
                        parse_esc_seq(stdin_fd)
                    };
                    if let Some(seq) = seq {
                        if seq[0] == b'[' {
                            match seq[1] {
                                b'A' if cursor > 0 => { // Up arrow
                                    let old = cursor;
                                    cursor -= 1;

                                    if cursor < scroll_offset {
                                        // Need to scroll up - full redraw
                                        scroll_offset = cursor;
                                        let full = render_full(title, items, cursor, &checked, &disabled, multi, cols, scroll_offset);
                                        // Move up to title line first
                                        let total_lines = 1 + 1 + vis + 1 + 1;
                                        let s = format!("\x1b[{}A", total_lines - 1);
                                        common::write_stdout(s.as_bytes());
                                        common::write_stdout(b"\r\x1b[J");
                                        common::write_stdout(full.as_bytes());
                                    } else {
                                        // Incremental: redraw old and new within viewport
                                        let old_vis_pos = old - scroll_offset; // 0-based position in viewport
                                        let up_to_old = (vis - old_vis_pos) + 1; // +1 for bottom separator
                                        let s = format!("\x1b[{}A", up_to_old);
                                        common::write_stdout(s.as_bytes());
                                        redraw_item(items[old], false, checked[old], multi, disabled[old]);
                                        common::write_stdout(b"\x1b[1A");
                                        redraw_item(items[cursor], true, checked[cursor], multi, disabled[cursor]);
                                        let new_vis_pos = cursor - scroll_offset;
                                        let down = (vis - new_vis_pos) + 1;
                                        let s = format!("\x1b[{}B", down);
                                        common::write_stdout(s.as_bytes());
                                    }
                                }
                                b'B' if cursor < items.len() - 1 => { // Down arrow
                                    let old = cursor;
                                    cursor += 1;

                                    if cursor >= scroll_offset + vis {
                                        // Need to scroll down - full redraw
                                        scroll_offset = cursor - vis + 1;
                                        let full = render_full(title, items, cursor, &checked, &disabled, multi, cols, scroll_offset);
                                        let total_lines = 1 + 1 + vis + 1 + 1;
                                        let s = format!("\x1b[{}A", total_lines - 1);
                                        common::write_stdout(s.as_bytes());
                                        common::write_stdout(b"\r\x1b[J");
                                        common::write_stdout(full.as_bytes());
                                    } else {
                                        // Incremental: redraw old and new within viewport
                                        let old_vis_pos = old - scroll_offset;
                                        let up_to_old = (vis - old_vis_pos) + 1;
                                        let s = format!("\x1b[{}A", up_to_old);
                                        common::write_stdout(s.as_bytes());
                                        redraw_item(items[old], false, checked[old], multi, disabled[old]);
                                        common::write_stdout(b"\x1b[1B");
                                        redraw_item(items[cursor], true, checked[cursor], multi, disabled[cursor]);
                                        let new_vis_pos = cursor - scroll_offset;
                                        let down = (vis - new_vis_pos) + 1;
                                        let s = format!("\x1b[{}B", down);
                                        common::write_stdout(s.as_bytes());
                                    }
                                }
                                _ => {} // Ignore other sequences
                            }
                        }
                    } else {
                        // Bare ESC - cancel
                        let cleanup = render_cleanup(items.len());
                        common::write_stdout(cleanup.as_bytes());
                        common::write_stdout(b"\x1b[?25h");
                        return None;
                    }
                }
                b' ' if multi && disabled[cursor].is_none() => {
                    // Toggle check on current item (skip if disabled)
                    checked[cursor] = !checked[cursor];
                    let vis_pos = cursor - scroll_offset;
                    let up = (vis - vis_pos) + 1;
                    let s = format!("\x1b[{}A", up);
                    common::write_stdout(s.as_bytes());
                    redraw_item(items[cursor], true, checked[cursor], multi, disabled[cursor]);
                    let down = (vis - vis_pos) + 1;
                    let s = format!("\x1b[{}B", down);
                    common::write_stdout(s.as_bytes());
                }
                b'\r' | b'\n' if disabled[cursor].is_none() => {
                    // Confirm selection (skip if cursor is on disabled item)
                    let cleanup = render_cleanup(items.len());
                    common::write_stdout(cleanup.as_bytes());
                    common::write_stdout(b"\x1b[?25h");
                    if multi {
                        let selected: Vec<usize> = checked.iter()
                            .enumerate()
                            .filter(|(_, &c)| c)
                            .map(|(i, _)| i)
                            .collect();
                        return Some(selected);
                    } else {
                        return Some(vec![cursor]);
                    }
                }
            key => {
                // Check shortcut keys (e.g., 'y' for [Y]es) - skip disabled items
                if let Some(&idx) = shortcuts.get(&key.to_ascii_lowercase()) {
                    if disabled[idx].is_none() {
                        let cleanup = render_cleanup(items.len());
                        common::write_stdout(cleanup.as_bytes());
                        common::write_stdout(b"\x1b[?25h");
                        if multi {
                            checked[idx] = !checked[idx];
                            let selected: Vec<usize> = checked.iter()
                                .enumerate()
                                .filter(|(_, &c)| c)
                                .map(|(i, _)| i)
                                .collect();
                            return Some(selected);
                        } else {
                            return Some(vec![idx]);
                        }
                    }
                }
            }
        }
    }

    let cleanup = render_cleanup(items.len());
    common::write_stdout(cleanup.as_bytes());
    common::write_stdout(b"\x1b[?25h");
    None
}

/// Single select: returns the selected index (0-based), or None on ESC.
pub fn pick_one(title: &str, items: &[&str]) -> Option<usize> {
    run_picker(title, items, false, 0, &[]).map(|v| v[0])
}

/// Single select with pre-selected index: returns the selected index (0-based), or None on ESC.
pub fn pick_one_at(title: &str, items: &[&str], initial: usize) -> Option<usize> {
    run_picker(title, items, false, initial, &[]).map(|v| v[0])
}

/// Single select with disabled items: disabled items are shown dim with the given icon
/// and cannot be selected. Returns the selected index (0-based), or None on ESC.
pub fn pick_one_with_disabled(title: &str, items: &[&str], disabled: &[Option<DisabledIcon>]) -> Option<usize> {
    run_picker(title, items, false, 0, disabled).map(|v| v[0])
}

/// Multi select: returns selected indices (0-based), or None on ESC.
pub fn pick_many(title: &str, items: &[&str]) -> Option<Vec<usize>> {
    run_picker(title, items, true, 0, &[])
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
        let output = render_item("Option A", false, false, false, None);
        assert!(output.contains("  "), "non-selected item should have '  ' prefix");
        assert!(output.contains("Option A"), "should contain the item text");
        // Should NOT contain bold/reverse escape
        assert!(!output.contains(BOLD_REVERSE), "non-selected item should not be bold+reverse");
    }

    #[test]
    fn test_render_item_selected() {
        let output = render_item("Option B", true, false, false, None);
        assert!(output.contains("> "), "selected item should have '> ' prefix");
        assert!(output.contains("Option B"), "should contain the item text");
        // Should contain bold+reverse escape
        assert!(output.contains(BOLD_REVERSE), "selected item should be bold+reverse");
    }

    #[test]
    fn test_render_item_multi_checked() {
        let output = render_item("Checked item", false, true, true, None);
        assert!(output.contains("[x]"), "checked item in multi mode should show [x]");
        assert!(output.contains("Checked item"), "should contain the item text");
    }

    #[test]
    fn test_render_item_multi_unchecked() {
        let output = render_item("Unchecked item", true, false, true, None);
        assert!(output.contains("[ ]"), "unchecked item in multi mode should show [ ]");
        assert!(output.contains("> "), "selected item should have '> ' prefix");
        assert!(output.contains("Unchecked item"), "should contain the item text");
    }

    #[test]
    fn test_render_hint_single() {
        crate::i18n::init("en");
        let output = render_hint(false, 0);
        assert!(output.contains("Enter confirm"), "single mode hint should contain 'Enter confirm'");
        assert!(!output.contains("Space"), "single mode hint should NOT contain 'Space'");
        assert!(!output.contains("more"), "no scroll indicator when remaining_below=0");
    }

    #[test]
    fn test_render_hint_multi() {
        crate::i18n::init("en");
        let output = render_hint(true, 0);
        assert!(output.contains("Space select"), "multi mode hint should contain 'Space select'");
        assert!(output.contains("Enter confirm"), "multi mode hint should contain 'Enter confirm'");
    }

    #[test]
    fn test_render_hint_with_scroll_indicator() {
        crate::i18n::init("en");
        let output = render_hint(false, 5);
        assert!(output.contains("ESC/Ctrl-C cancel"), "should contain hint text");
        assert!(output.contains("\u{25bc} 5 more"), "should show scroll-down indicator");
    }

    #[test]
    fn test_render_full_single_select() {
        let cols: u16 = 60;
        let items = vec!["Alpha", "Beta", "Gamma"];
        let checked = vec![false, false, false];
        let disabled: Vec<Option<DisabledIcon>> = vec![None, None, None];
        let output = render_full("Pick one:", &items, 1, &checked, &disabled, false, cols, 0);

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
        crate::i18n::init("en");
        let cols: u16 = 60;
        let items = vec!["First", "Second"];
        let checked = vec![true, false];
        let disabled: Vec<Option<DisabledIcon>> = vec![None, None];
        let output = render_full("Select items:", &items, 0, &checked, &disabled, true, cols, 0);

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
        let disabled: Vec<Option<DisabledIcon>> = vec![None, None, None];

        // Render the full picker, then clean it up
        let mut output = render_full("Title:", &items, 0, &checked, &disabled, false, cols, 0);
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

    // -- Scrolling viewport tests --

    #[test]
    fn test_visible_count_small_list() {
        assert_eq!(visible_count(3), 3);
        assert_eq!(visible_count(10), 10);
    }

    #[test]
    fn test_visible_count_large_list() {
        assert_eq!(visible_count(15), MAX_VISIBLE);
        assert_eq!(visible_count(100), MAX_VISIBLE);
    }

    #[test]
    fn test_render_full_with_scroll_shows_viewport() {
        crate::i18n::init("en");
        let cols: u16 = 60;
        let items: Vec<&str> = (0..15).map(|i| match i {
            0 => "Item-00", 1 => "Item-01", 2 => "Item-02", 3 => "Item-03",
            4 => "Item-04", 5 => "Item-05", 6 => "Item-06", 7 => "Item-07",
            8 => "Item-08", 9 => "Item-09", 10 => "Item-10", 11 => "Item-11",
            12 => "Item-12", 13 => "Item-13", 14 => "Item-14",
            _ => unreachable!(),
        }).collect();
        let checked = vec![false; 15];
        let disabled: Vec<Option<DisabledIcon>> = vec![None; 15];

        // Render with scroll_offset=0, cursor=0
        let output = render_full("Pick:", &items, 0, &checked, &disabled, false, cols, 0);
        let rows = 30u16;
        let parser = parse_ansi(&output, cols, rows);
        let all_text = parser.screen().contents();

        // Should show items 0-9, not 10-14
        assert!(all_text.contains("Item-00"), "should show first item");
        assert!(all_text.contains("Item-09"), "should show last visible item");
        assert!(!all_text.contains("Item-10"), "should NOT show items beyond viewport");
        // Should show "more" indicator at bottom
        assert!(all_text.contains("5 more"), "should show remaining count below");
    }

    #[test]
    fn test_render_full_scrolled_down() {
        let cols: u16 = 60;
        let items: Vec<&str> = (0..15).map(|i| match i {
            0 => "Item-00", 1 => "Item-01", 2 => "Item-02", 3 => "Item-03",
            4 => "Item-04", 5 => "Item-05", 6 => "Item-06", 7 => "Item-07",
            8 => "Item-08", 9 => "Item-09", 10 => "Item-10", 11 => "Item-11",
            12 => "Item-12", 13 => "Item-13", 14 => "Item-14",
            _ => unreachable!(),
        }).collect();
        let checked = vec![false; 15];
        let disabled: Vec<Option<DisabledIcon>> = vec![None; 15];

        // Render with scroll_offset=5, cursor=10
        let output = render_full("Pick:", &items, 10, &checked, &disabled, false, cols, 5);
        let rows = 30u16;
        let parser = parse_ansi(&output, cols, rows);
        let all_text = parser.screen().contents();

        // Should show items 5-14
        assert!(!all_text.contains("Item-04"), "should NOT show items above viewport");
        assert!(all_text.contains("Item-05"), "should show first visible item");
        assert!(all_text.contains("Item-14"), "should show last item");
        // Should show "more" indicator at top
        assert!(all_text.contains("5 more"), "should show count above");
    }

    #[test]
    fn test_render_cleanup_scrolled_list() {
        let cols: u16 = 60;
        let items: Vec<&str> = (0..15).map(|_| "item").collect();
        let checked = vec![false; 15];
        let disabled: Vec<Option<DisabledIcon>> = vec![None; 15];

        let mut output = render_full("Title:", &items, 0, &checked, &disabled, false, cols, 0);
        output.push_str(&render_cleanup(items.len()));

        let rows = 30u16;
        let parser = parse_ansi(&output, cols, rows);
        let all_text = parser.screen().contents();

        assert!(!all_text.contains("Title:"), "title should be erased");
        assert!(!all_text.contains("item"), "items should be erased");
        assert!(!all_text.contains("confirm"), "hint should be erased");
    }

    // -- Shortcut key tests --

    #[test]
    fn test_extract_shortcut_basic() {
        assert_eq!(extract_shortcut("[Y]es"), Some(b'y'));
        assert_eq!(extract_shortcut("[N]o"), Some(b'n'));
        assert_eq!(extract_shortcut("[C]ancel"), Some(b'c'));
    }

    #[test]
    fn test_extract_shortcut_mid_text() {
        assert_eq!(extract_shortcut("cd to previous dir [Y]"), Some(b'y'));
        assert_eq!(extract_shortcut("stay [H]ere"), Some(b'h'));
    }

    #[test]
    fn test_extract_shortcut_none() {
        assert_eq!(extract_shortcut("No shortcut"), None);
        assert_eq!(extract_shortcut("[]empty"), None);
        assert_eq!(extract_shortcut("[12]digits"), None);
    }

    #[test]
    fn test_build_shortcut_map() {
        let items = vec!["[Y]es", "[N]o", "[C]ancel"];
        let map = build_shortcut_map(&items);
        assert_eq!(map.get(&b'y'), Some(&0));
        assert_eq!(map.get(&b'n'), Some(&1));
        assert_eq!(map.get(&b'c'), Some(&2));
        assert_eq!(map.get(&b'x'), None);
    }

    #[test]
    fn test_build_shortcut_map_first_wins() {
        let items = vec!["[A] first", "[A] second"];
        let map = build_shortcut_map(&items);
        assert_eq!(map.get(&b'a'), Some(&0));
    }

    // -- Disabled item tests --

    #[test]
    fn test_render_item_disabled_lock() {
        let output = render_item("Locked thread", false, false, false, Some(DisabledIcon::Lock));
        assert!(output.contains("\u{1f512}"), "Lock icon should show 🔒");
        assert!(output.contains(crate::display::DIM), "disabled item should be dim");
        assert!(!output.contains(BOLD_REVERSE), "disabled item should NOT be bold+reverse");
    }

    #[test]
    fn test_render_item_disabled_key() {
        let output = render_item("Locked thread", false, false, false, Some(DisabledIcon::Key));
        assert!(output.contains("\u{26bf}"), "Key icon should show ⚿");
        assert!(output.contains(crate::display::DIM), "disabled item should be dim");
    }

    #[test]
    fn test_render_item_disabled_forbidden() {
        let output = render_item("Locked thread", false, false, false, Some(DisabledIcon::Forbidden));
        assert!(output.contains("\u{2298}"), "Forbidden icon should show ⊘");
        assert!(output.contains(crate::display::DIM), "disabled item should be dim");
    }

    #[test]
    fn test_render_item_disabled_selected() {
        // Even when cursor is on a disabled item, it should be dim (no reverse video)
        let output = render_item("Locked thread", true, false, false, Some(DisabledIcon::Lock));
        assert!(output.contains("\u{1f512}"), "disabled selected item should show lock icon");
        assert!(output.contains(crate::display::DIM), "disabled selected item should be dim");
        assert!(!output.contains(BOLD_REVERSE), "disabled selected item should NOT be bold+reverse");
    }

    #[test]
    fn test_render_full_with_disabled() {
        let cols: u16 = 60;
        let items = vec!["Active", "Locked", "Active2"];
        let checked = vec![false, false, false];
        let disabled = vec![None, Some(DisabledIcon::Key), None];
        let output = render_full("Pick:", &items, 0, &checked, &disabled, false, cols, 0);

        let rows = 20u16;
        let parser = parse_ansi(&output, cols, rows);
        let all_text = parser.screen().contents();

        assert!(all_text.contains("Active"), "should display active item");
        assert!(all_text.contains("Locked"), "should display locked item text");
    }
}
