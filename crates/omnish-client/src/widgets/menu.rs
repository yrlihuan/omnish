// crates/omnish-client/src/widgets/menu.rs
//
// Multi-level menu widget for hierarchical option navigation.
// Supports submenu drilling, toggle, select, and text input item types.
// Reuses picker rendering patterns (separator, viewport scrolling, hint line).

use std::os::unix::io::AsRawFd;

/// Maximum number of items visible in the menu viewport.
const MAX_VISIBLE: usize = 10;

// ── Public types ────────────────────────────────────────────────────────

/// A single menu item.
pub enum MenuItem {
    /// Navigate into a child menu.
    Submenu {
        label: String,
        children: Vec<MenuItem>,
    },
    /// Choose from a fixed set of options.
    Select {
        label: String,
        options: Vec<String>,
        selected: usize,
    },
    /// Boolean toggle (Enter flips immediately).
    Toggle {
        label: String,
        value: bool,
    },
    /// Free-form text/number input.
    TextInput {
        label: String,
        value: String,
    },
}

impl MenuItem {
    fn label(&self) -> &str {
        match self {
            MenuItem::Submenu { label, .. }
            | MenuItem::Select { label, .. }
            | MenuItem::Toggle { label, .. }
            | MenuItem::TextInput { label, .. } => label,
        }
    }
}

/// Result returned when the widget exits.
pub enum MenuResult {
    /// User exited normally (ESC at top level). Contains all modified values.
    Done(Vec<MenuChange>),
    /// User pressed Ctrl-C. Discard all changes.
    Cancelled,
}

/// A single value change made during the menu session.
#[derive(Debug, Clone)]
pub struct MenuChange {
    /// Dot-separated path, e.g. "llm.default" or "shell.developer_mode".
    pub path: String,
    /// New value as a string representation.
    pub value: String,
}

// ── Rendering helpers ───────────────────────────────────────────────────

fn terminal_cols() -> u16 {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 && ws.ws_col > 0 { ws.ws_col } else { 80 }
}

fn render_separator(cols: u16) -> String {
    format!("\r\x1b[2m{}\x1b[0m", "\u{2500}".repeat(cols as usize))
}

fn visible_count(total: usize) -> usize {
    total.min(MAX_VISIBLE)
}

/// Render a menu item line with right-aligned value/indicator.
fn render_menu_item(item: &MenuItem, selected: bool, cols: u16) -> String {
    let prefix = if selected { "> " } else { "  " };
    let label = item.label();

    let right = match item {
        MenuItem::Submenu { .. } => "\x1b[2m>\x1b[0m".to_string(),
        MenuItem::Select { options, selected: idx, .. } => {
            let val = options.get(*idx).map(|s| s.as_str()).unwrap_or("");
            format!("\x1b[2m{}\x1b[0m", val)
        }
        MenuItem::Toggle { value, .. } => {
            if *value {
                "\x1b[32mON\x1b[0m".to_string()
            } else {
                "\x1b[2mOFF\x1b[0m".to_string()
            }
        }
        MenuItem::TextInput { value, .. } => {
            if value.is_empty() {
                "\x1b[2m(empty)\x1b[0m".to_string()
            } else {
                format!("\x1b[2m{}\x1b[0m", value)
            }
        }
    };

    // Calculate visible widths (strip ANSI for measurement)
    let right_text = strip_ansi(&right);
    let left_len = prefix.len() + label.len();
    let right_len = right_text.len();
    let total_width = cols as usize;

    let padding = if left_len + right_len + 2 < total_width {
        total_width - left_len - right_len
    } else {
        2
    };

    if selected {
        // Bold + reverse for the whole line, but right value uses its own colors
        format!(
            "\r\x1b[1;7m{}{}{}\x1b[0m{}\x1b[K",
            prefix,
            label,
            " ".repeat(padding),
            right,
        )
    } else {
        format!(
            "\r{}{}{}{}\x1b[K",
            prefix,
            label,
            " ".repeat(padding),
            right,
        )
    }
}

fn render_hint(remaining_below: usize, editing: bool) -> String {
    let hint = if editing {
        "Enter confirm  ESC cancel"
    } else {
        "\u{2191}\u{2193} move  Enter select  ESC back  ^C quit"
    };
    if remaining_below > 0 {
        format!(
            "\r\x1b[2m{}  (\u{25bc} {} more)\x1b[0m\x1b[K",
            hint, remaining_below
        )
    } else {
        format!("\r\x1b[2m{}\x1b[0m\x1b[K", hint)
    }
}

fn render_full(
    breadcrumb: &str,
    items: &[MenuItem],
    cursor: usize,
    cols: u16,
    scroll_offset: usize,
) -> String {
    let vis = visible_count(items.len());
    let total_lines = 1 + 1 + vis + 1 + 1; // breadcrumb + sep + items + sep + hint
    let mut out = String::new();

    // Push screen content up
    for _ in 0..total_lines {
        out.push_str("\r\n");
    }
    out.push_str(&format!("\x1b[{}A", total_lines));

    // Breadcrumb title (with scroll-up indicator)
    if scroll_offset > 0 {
        out.push_str(&format!(
            "\r\x1b[1m{}\x1b[0m \x1b[2m(\u{25b2} {} more)\x1b[0m\x1b[K",
            breadcrumb, scroll_offset
        ));
    } else {
        out.push_str(&format!("\r\x1b[1m{}\x1b[0m\x1b[K", breadcrumb));
    }
    out.push_str("\r\n");

    // Top separator
    out.push_str(&render_separator(cols));
    out.push_str("\r\n");

    // Visible items
    let end = (scroll_offset + vis).min(items.len());
    for i in scroll_offset..end {
        out.push_str(&render_menu_item(&items[i], i == cursor, cols));
        if i < end - 1 {
            out.push_str("\r\n");
        }
    }
    out.push_str("\r\n");

    // Bottom separator
    let remaining_below = items.len().saturating_sub(end);
    out.push_str(&render_separator(cols));
    out.push_str("\r\n");

    // Hint
    out.push_str(&render_hint(remaining_below, false));

    out
}

fn render_cleanup(total_items: usize) -> String {
    let vis = visible_count(total_items);
    let total_lines = 1 + 1 + vis + 1 + 1;
    let up = total_lines - 1;
    format!("\x1b[{}A\r\x1b[J", up)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_esc = false;
    for ch in s.chars() {
        if in_esc {
            if ch.is_ascii_alphabetic() {
                in_esc = false;
            }
        } else if ch == '\x1b' {
            in_esc = true;
        } else {
            out.push(ch);
        }
    }
    out
}

fn write_stdout(data: &[u8]) {
    nix::unistd::write(std::io::stdout(), data).ok();
}

/// Parse escape sequence after ESC byte.
fn parse_esc_seq(stdin_fd: i32) -> Option<[u8; 2]> {
    let mut pfd = libc::pollfd {
        fd: stdin_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut pfd, 1, 50) };
    if ready <= 0 {
        return None;
    }
    let mut seq = [0u8; 2];
    if nix::unistd::read(stdin_fd, &mut seq[0..1]) != Ok(1) {
        return None;
    }
    if seq[0] == b'[' && nix::unistd::read(stdin_fd, &mut seq[1..2]) == Ok(1) {
        return Some(seq);
    }
    None
}

// ── Text editing ────────────────────────────────────────────────────────

/// Render a text input line in edit mode.
fn render_edit_line(label: &str, text: &str, cursor_pos: usize, cols: u16, selected: bool) -> String {
    let prefix = if selected { "> " } else { "  " };
    let label_part = format!("{}{}", prefix, label);
    let label_len = label_part.len();
    // Reserve space: label + 2 spaces minimum + text
    let available = (cols as usize).saturating_sub(label_len + 2);
    let display_text = if text.len() > available {
        &text[text.len() - available..]
    } else {
        text
    };
    let padding = (cols as usize).saturating_sub(label_len + display_text.len());

    // Show text in normal color (not dimmed), with cursor positioned
    let (before, after) = if cursor_pos <= display_text.len() {
        (&display_text[..cursor_pos], &display_text[cursor_pos..])
    } else {
        (display_text, "")
    };

    format!(
        "\r{}{}{}{}{}\x1b[K",
        prefix, label, " ".repeat(padding), before, after,
    )
}

/// Run inline text editor. Returns Some(new_value) on Enter, None on ESC.
fn run_text_edit(
    label: &str,
    initial: &str,
    cursor_row_from_bottom: usize,
    cols: u16,
) -> Option<String> {
    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut text = initial.to_string();
    let mut cursor_pos = text.len();
    let mut byte = [0u8; 1];

    // Show cursor during editing
    write_stdout(b"\x1b[?25h");

    // Move to the item line and redraw in edit mode
    if cursor_row_from_bottom > 0 {
        write_stdout(format!("\x1b[{}A", cursor_row_from_bottom).as_bytes());
    }
    let line = render_edit_line(label, &text, cursor_pos, cols, true);
    write_stdout(line.as_bytes());

    // Position terminal cursor within the text
    let redraw_cursor = |text: &str, cursor_pos: usize, label: &str, cols: u16| {
        let prefix_len = 2 + label.len(); // "> " + label
        let available = (cols as usize).saturating_sub(prefix_len + 2);
        let display_offset = if text.len() > available {
            text.len() - available
        } else {
            0
        };
        let visual_pos = cursor_pos - display_offset;
        let total_offset = (cols as usize).saturating_sub(text.len() - display_offset) + visual_pos;
        write_stdout(format!("\r\x1b[{}C", total_offset).as_bytes());
    };

    redraw_cursor(&text, cursor_pos, label, cols);

    // Update hint line
    if cursor_row_from_bottom > 0 {
        write_stdout(format!("\x1b[{}B", cursor_row_from_bottom).as_bytes());
    }
    // Move to hint line (2 down from last visible item = separator + hint)
    let hint = render_hint(0, true);
    write_stdout(b"\r");
    write_stdout(hint.as_bytes());
    // Move back to edit line
    if cursor_row_from_bottom > 0 {
        write_stdout(format!("\x1b[{}A", cursor_row_from_bottom).as_bytes());
    }
    redraw_cursor(&text, cursor_pos, label, cols);

    while let Ok(1) = nix::unistd::read(stdin_fd, &mut byte) {
        match byte[0] {
            b'\r' | b'\n' => {
                // Confirm edit
                write_stdout(b"\x1b[?25l");
                return Some(text);
            }
            0x03 => {
                // Ctrl-C: cancel entire widget (caller handles)
                write_stdout(b"\x1b[?25l");
                return None;
            }
            0x1b => {
                if let Some(seq) = parse_esc_seq(stdin_fd) {
                    if seq[0] == b'[' {
                        match seq[1] {
                            b'D' if cursor_pos > 0 => {
                                // Left arrow
                                cursor_pos -= 1;
                            }
                            b'C' if cursor_pos < text.len() => {
                                // Right arrow
                                cursor_pos += 1;
                            }
                            b'H' => cursor_pos = 0, // Home
                            b'F' => cursor_pos = text.len(), // End
                            _ => {}
                        }
                    }
                } else {
                    // Bare ESC: cancel edit, restore old value
                    write_stdout(b"\x1b[?25l");
                    // We return a special sentinel; caller restores old value
                    return None;
                }
            }
            0x7f | 0x08 => {
                // Backspace
                if cursor_pos > 0 {
                    text.remove(cursor_pos - 1);
                    cursor_pos -= 1;
                }
            }
            b if b >= 0x20 => {
                // Printable character
                text.insert(cursor_pos, b as char);
                cursor_pos += 1;
            }
            _ => {}
        }

        // Redraw edit line
        if cursor_row_from_bottom > 0 {
            // Already on the edit line from previous iteration
        }
        let line = render_edit_line(label, &text, cursor_pos, cols, true);
        write_stdout(b"\r");
        write_stdout(line.as_bytes());
        redraw_cursor(&text, cursor_pos, label, cols);
    }

    write_stdout(b"\x1b[?25l");
    None
}

// ── Select sub-picker ───────────────────────────────────────────────────

/// Run a flat picker for Select items. Returns selected index or None.
fn run_select(title: &str, options: &[String], initial: usize) -> Option<usize> {
    let refs: Vec<&str> = options.iter().map(|s| s.as_str()).collect();
    super::picker::pick_one_at(title, &refs, initial)
}

// ── Main menu loop ──────────────────────────────────────────────────────

/// Navigation stack entry: (index into parent's children, cursor position, scroll offset).
struct NavEntry {
    /// Index of the Submenu item in the parent level.
    parent_index: usize,
    /// Cursor position when we left this level.
    cursor: usize,
    /// Scroll offset when we left this level.
    scroll_offset: usize,
}

/// Run the multi-level menu widget. Returns changes made or Cancelled.
pub fn run_menu(title: &str, items: &mut [MenuItem]) -> MenuResult {
    if items.is_empty() {
        return MenuResult::Done(vec![]);
    }

    let cols = terminal_cols();
    let mut changes: Vec<MenuChange> = Vec::new();
    let mut nav_stack: Vec<NavEntry> = Vec::new();
    let mut breadcrumb_parts: Vec<String> = vec![title.to_string()];

    // Current level state
    let mut current_items: &mut [MenuItem] = items;
    let mut cursor: usize = 0;
    let mut scroll_offset: usize = 0;

    // Hide cursor
    write_stdout(b"\x1b[?25l");

    // Initial render
    let bc = breadcrumb_parts.join(" > ");
    let full = render_full(&bc, current_items, cursor, cols, scroll_offset);
    write_stdout(full.as_bytes());

    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut byte = [0u8; 1];

    // We need to track the item count for cleanup before navigation
    let mut last_item_count = current_items.len();

    loop {
        if nix::unistd::read(stdin_fd, &mut byte) != Ok(1) {
            break;
        }

        match byte[0] {
            0x03 => {
                // Ctrl-C: quit entirely
                let cleanup = render_cleanup(last_item_count);
                write_stdout(cleanup.as_bytes());
                write_stdout(b"\x1b[?25h");
                return MenuResult::Cancelled;
            }
            0x1b => {
                if let Some(seq) = parse_esc_seq(stdin_fd) {
                    if seq[0] == b'[' {
                        let vis = visible_count(current_items.len());
                        match seq[1] {
                            b'A' if cursor > 0 => {
                                // Up arrow
                                cursor -= 1;
                                if cursor < scroll_offset {
                                    scroll_offset = cursor;
                                    let bc = breadcrumb_parts.join(" > ");
                                    full_redraw(&bc, current_items, cursor, cols, scroll_offset, vis);
                                } else {
                                    incremental_redraw(current_items, cursor, cursor + 1, cols, vis, scroll_offset);
                                }
                            }
                            b'B' if cursor < current_items.len().saturating_sub(1) => {
                                // Down arrow
                                cursor += 1;
                                if cursor >= scroll_offset + vis {
                                    scroll_offset = cursor - vis + 1;
                                    let bc = breadcrumb_parts.join(" > ");
                                    full_redraw(&bc, current_items, cursor, cols, scroll_offset, vis);
                                } else {
                                    incremental_redraw(current_items, cursor, cursor - 1, cols, vis, scroll_offset);
                                }
                            }
                            _ => {}
                        }
                    }
                } else {
                    // Bare ESC: go back one level or exit
                    if nav_stack.is_empty() {
                        // Top level: exit
                        let cleanup = render_cleanup(last_item_count);
                        write_stdout(cleanup.as_bytes());
                        write_stdout(b"\x1b[?25h");
                        return MenuResult::Done(changes);
                    }

                    // Pop navigation stack
                    let cleanup = render_cleanup(last_item_count);
                    write_stdout(cleanup.as_bytes());

                    breadcrumb_parts.pop();
                    let entry = nav_stack.pop().unwrap();

                    // Navigate back to parent: rebuild pointer
                    current_items = items;
                    for nav in &nav_stack {
                        current_items = match &mut current_items[nav.parent_index] {
                            MenuItem::Submenu { children, .. } => children.as_mut_slice(),
                            _ => unreachable!(),
                        };
                    }
                    cursor = entry.cursor;
                    scroll_offset = entry.scroll_offset;
                    last_item_count = current_items.len();

                    let bc = breadcrumb_parts.join(" > ");
                    let full = render_full(&bc, current_items, cursor, cols, scroll_offset);
                    write_stdout(full.as_bytes());
                }
            }
            b'\r' | b'\n' => {
                // Enter: action depends on item type
                let vis = visible_count(current_items.len());
                let row_from_bottom = (vis - (cursor - scroll_offset)) + 1; // +1 for bottom separator

                match &mut current_items[cursor] {
                    MenuItem::Submenu { label, children } => {
                        if children.is_empty() {
                            continue;
                        }
                        let label_clone = label.clone();

                        // Clean up current view
                        let cleanup = render_cleanup(last_item_count);
                        write_stdout(cleanup.as_bytes());

                        // Push nav state
                        nav_stack.push(NavEntry {
                            parent_index: cursor,
                            cursor,
                            scroll_offset,
                        });
                        breadcrumb_parts.push(label_clone);

                        // Navigate into submenu: rebuild pointer from root
                        current_items = items;
                        for nav in &nav_stack {
                            current_items = match &mut current_items[nav.parent_index] {
                                MenuItem::Submenu { children, .. } => children.as_mut_slice(),
                                _ => unreachable!(),
                            };
                        }

                        cursor = 0;
                        scroll_offset = 0;
                        last_item_count = current_items.len();

                        let bc = breadcrumb_parts.join(" > ");
                        let full = render_full(&bc, current_items, cursor, cols, scroll_offset);
                        write_stdout(full.as_bytes());
                    }
                    MenuItem::Toggle { label, value } => {
                        *value = !*value;
                        // Record change
                        let path = build_path(&breadcrumb_parts, label);
                        changes.push(MenuChange {
                            path,
                            value: value.to_string(),
                        });
                        // Redraw just the current item
                        let up = row_from_bottom;
                        write_stdout(format!("\x1b[{}A", up).as_bytes());
                        let line = render_menu_item(&current_items[cursor], true, cols);
                        write_stdout(line.as_bytes());
                        write_stdout(format!("\x1b[{}B", up).as_bytes());
                    }
                    MenuItem::Select { label, options, selected } => {
                        let label_clone = label.clone();
                        let options_clone = options.clone();
                        let old_selected = *selected;

                        // Clean current view, run sub-picker
                        let cleanup = render_cleanup(last_item_count);
                        write_stdout(cleanup.as_bytes());

                        let select_title = format!("{} > {}", breadcrumb_parts.join(" > "), label_clone);
                        if let Some(idx) = run_select(&select_title, &options_clone, old_selected) {
                            *selected = idx;
                            if idx != old_selected {
                                let path = build_path(&breadcrumb_parts, &label_clone);
                                changes.push(MenuChange {
                                    path,
                                    value: options_clone[idx].clone(),
                                });
                            }
                        }

                        // Re-render menu
                        let bc = breadcrumb_parts.join(" > ");
                        let full = render_full(&bc, current_items, cursor, cols, scroll_offset);
                        write_stdout(full.as_bytes());
                    }
                    MenuItem::TextInput { label, value } => {
                        let label_clone = label.clone();
                        let old_value = value.clone();

                        // Run inline text editor
                        let result = run_text_edit(&label_clone, &old_value, row_from_bottom, cols);

                        match result {
                            Some(new_val) => {
                                *value = new_val.clone();
                                if new_val != old_value {
                                    let path = build_path(&breadcrumb_parts, &label_clone);
                                    changes.push(MenuChange {
                                        path,
                                        value: new_val,
                                    });
                                }
                            }
                            None => {
                                // ESC or Ctrl-C during edit: restore old value
                                *value = old_value;
                            }
                        }

                        // Full redraw to restore hint and clean up
                        let bc = breadcrumb_parts.join(" > ");
                        // Move to top of widget area
                        let vis = visible_count(current_items.len());
                        let total_lines = 1 + 1 + vis + 1 + 1;
                        write_stdout(format!("\x1b[{}A\r\x1b[J", total_lines - 1).as_bytes());
                        let full = render_full(&bc, current_items, cursor, cols, scroll_offset);
                        write_stdout(full.as_bytes());
                    }
                }
            }
            _ => {}
        }
    }

    let cleanup = render_cleanup(last_item_count);
    write_stdout(cleanup.as_bytes());
    write_stdout(b"\x1b[?25h");
    MenuResult::Done(changes)
}

/// Full redraw: move up, clear, re-render.
fn full_redraw(
    breadcrumb: &str,
    items: &[MenuItem],
    cursor: usize,
    cols: u16,
    scroll_offset: usize,
    vis: usize,
) {
    let total_lines = 1 + 1 + vis + 1 + 1;
    write_stdout(format!("\x1b[{}A\r\x1b[J", total_lines - 1).as_bytes());
    let full = render_full(breadcrumb, items, cursor, cols, scroll_offset);
    write_stdout(full.as_bytes());
}

/// Incremental redraw: update only the old and new cursor lines.
fn incremental_redraw(
    items: &[MenuItem],
    new_cursor: usize,
    old_cursor: usize,
    cols: u16,
    vis: usize,
    scroll_offset: usize,
) {
    // Redraw old position (deselect)
    let old_vis_pos = old_cursor - scroll_offset;
    let up_to_old = (vis - old_vis_pos) + 1; // +1 for bottom separator
    write_stdout(format!("\x1b[{}A", up_to_old).as_bytes());
    let line = render_menu_item(&items[old_cursor], false, cols);
    write_stdout(line.as_bytes());

    // Redraw new position (select)
    if new_cursor < old_cursor {
        write_stdout(b"\x1b[1A");
    } else {
        write_stdout(b"\x1b[1B");
    }
    let line = render_menu_item(&items[new_cursor], true, cols);
    write_stdout(line.as_bytes());

    // Move back to bottom
    let new_vis_pos = new_cursor - scroll_offset;
    let down = (vis - new_vis_pos) + 1;
    write_stdout(format!("\x1b[{}B", down).as_bytes());
}

/// Build dot-separated path from breadcrumb parts and current label.
fn build_path(breadcrumb: &[String], label: &str) -> String {
    let mut parts: Vec<&str> = breadcrumb.iter().skip(1).map(|s| s.as_str()).collect();
    parts.push(label);
    parts.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[2mhello\x1b[0m"), "hello");
        assert_eq!(strip_ansi("no escape"), "no escape");
        assert_eq!(strip_ansi("\x1b[32mON\x1b[0m"), "ON");
    }

    #[test]
    fn test_build_path() {
        let bc = vec!["Config".to_string(), "LLM".to_string()];
        assert_eq!(build_path(&bc, "default"), "LLM.default");

        let bc = vec!["Config".to_string()];
        assert_eq!(build_path(&bc, "proxy"), "proxy");
    }

    #[test]
    fn test_menu_item_label() {
        let item = MenuItem::Toggle {
            label: "enabled".to_string(),
            value: true,
        };
        assert_eq!(item.label(), "enabled");

        let item = MenuItem::Submenu {
            label: "LLM".to_string(),
            children: vec![],
        };
        assert_eq!(item.label(), "LLM");
    }

    #[test]
    fn test_render_menu_item_submenu() {
        let item = MenuItem::Submenu {
            label: "LLM".to_string(),
            children: vec![],
        };
        let line = render_menu_item(&item, false, 40);
        let text = strip_ansi(&line);
        assert!(text.contains("LLM"));
        assert!(text.contains(">"));
    }

    #[test]
    fn test_render_menu_item_toggle_on() {
        let item = MenuItem::Toggle {
            label: "Enabled".to_string(),
            value: true,
        };
        let line = render_menu_item(&item, false, 40);
        let text = strip_ansi(&line);
        assert!(text.contains("Enabled"));
        assert!(text.contains("ON"));
    }

    #[test]
    fn test_render_menu_item_toggle_off() {
        let item = MenuItem::Toggle {
            label: "Enabled".to_string(),
            value: false,
        };
        let line = render_menu_item(&item, false, 40);
        let text = strip_ansi(&line);
        assert!(text.contains("OFF"));
    }

    #[test]
    fn test_render_menu_item_select() {
        let item = MenuItem::Select {
            label: "Backend".to_string(),
            options: vec!["claude".to_string(), "openai".to_string()],
            selected: 0,
        };
        let line = render_menu_item(&item, false, 40);
        let text = strip_ansi(&line);
        assert!(text.contains("Backend"));
        assert!(text.contains("claude"));
    }

    #[test]
    fn test_render_menu_item_text_input() {
        let item = MenuItem::TextInput {
            label: "Proxy".to_string(),
            value: "http://proxy:8080".to_string(),
        };
        let line = render_menu_item(&item, false, 60);
        let text = strip_ansi(&line);
        assert!(text.contains("Proxy"));
        assert!(text.contains("http://proxy:8080"));
    }

    #[test]
    fn test_render_menu_item_text_input_empty() {
        let item = MenuItem::TextInput {
            label: "Proxy".to_string(),
            value: String::new(),
        };
        let line = render_menu_item(&item, false, 40);
        let text = strip_ansi(&line);
        assert!(text.contains("(empty)"));
    }

    #[test]
    fn test_render_hint_normal() {
        let hint = render_hint(0, false);
        assert!(hint.contains("move"));
        assert!(hint.contains("select"));
        assert!(hint.contains("back"));
        assert!(hint.contains("quit"));
    }

    #[test]
    fn test_render_hint_editing() {
        let hint = render_hint(0, true);
        assert!(hint.contains("confirm"));
        assert!(hint.contains("cancel"));
        assert!(!hint.contains("move"));
    }

    #[test]
    fn test_render_hint_with_scroll() {
        let hint = render_hint(5, false);
        assert!(hint.contains("5 more"));
    }

    #[test]
    fn test_render_full_basic() {
        let items = vec![
            MenuItem::Toggle {
                label: "Enabled".to_string(),
                value: true,
            },
            MenuItem::Submenu {
                label: "LLM".to_string(),
                children: vec![],
            },
        ];
        let output = render_full("Config", &items, 0, 60, 0);
        let text = strip_ansi(&output);
        assert!(text.contains("Config"));
        assert!(text.contains("Enabled"));
        assert!(text.contains("LLM"));
    }

    #[test]
    fn test_empty_menu_returns_done() {
        let result = run_menu("Empty", &mut []);
        match result {
            MenuResult::Done(changes) => assert!(changes.is_empty()),
            MenuResult::Cancelled => panic!("Expected Done"),
        }
    }
}
