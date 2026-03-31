// crates/omnish-client/src/widgets/menu.rs
//
// Multi-level menu widget for hierarchical option navigation.
// Supports submenu drilling, toggle, select, and text input item types.
// Reuses shared terminal utilities from widgets/common.rs.

use std::os::unix::io::AsRawFd;

use super::common::{self, MAX_VISIBLE};

// ── Layout constants ────────────────────────────────────────────────────

/// Number of non-item lines in the widget: breadcrumb + sep + sep + hint.
const CHROME_LINES: usize = 4;

/// Total lines occupied by the widget for a given item count.
fn total_lines(item_count: usize) -> usize {
    CHROME_LINES + visible_count(item_count)
}

/// Lines below the cursor item: remaining visible items + separator + hint.
fn lines_below_cursor(vis: usize, cursor_vis_pos: usize) -> usize {
    (vis - 1 - cursor_vis_pos) + 2 // remaining items + separator + hint
}

// ── Public types ────────────────────────────────────────────────────────

/// Callback type for handling menu exit events.
type MenuExitHandler<'a> = Option<&'a mut dyn FnMut(&str, Vec<MenuChange>) -> Option<Vec<MenuItem>>>;

/// Callback type for immediate per-item change notification (non-form-mode items).
/// Returns `true` if the change was saved successfully, `false` to revert.
type MenuChangeHandler<'a> = Option<&'a mut dyn FnMut(&MenuChange) -> bool>;

/// A single menu item.
#[derive(Clone)]
pub enum MenuItem {
    /// Navigate into a child menu.
    Submenu {
        label: String,
        children: Vec<MenuItem>,
        handler: Option<String>,
        /// When true, TextInput items auto-enter edit on focus and Enter advances to next item.
        form_mode: bool,
    },
    /// Choose from a fixed set of options.
    Select {
        label: String,
        options: Vec<String>,
        selected: usize,
        /// When an option is selected in form_mode, fill sibling items with these values.
        /// `Vec<(option_name, Vec<(sibling_label, value)>)>`
        prefills: Vec<(String, Vec<(String, String)>)>,
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
    /// Action button (e.g. Confirm in form mode).
    Button {
        label: String,
    },
}

impl MenuItem {
    fn label(&self) -> &str {
        match self {
            MenuItem::Submenu { label, .. }
            | MenuItem::Select { label, .. }
            | MenuItem::Toggle { label, .. }
            | MenuItem::TextInput { label, .. }
            | MenuItem::Button { label, .. } => label,
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

fn visible_count(total: usize) -> usize {
    total.min(MAX_VISIBLE)
}

/// Render a menu item line. All values/indicators shown inline after label.
fn render_menu_item(item: &MenuItem, selected: bool) -> String {
    let indent = "  ";
    let label = item.label();

    // Inline suffix: value shown right after label
    let suffix = match item {
        MenuItem::Toggle { value, .. } => {
            if *value {
                " \x1b[32m[ON]\x1b[0m".to_string()
            } else {
                " \x1b[90m[OFF]\x1b[0m".to_string()
            }
        }
        MenuItem::TextInput { value, .. } => {
            if value.is_empty() {
                " \x1b[90m(empty)\x1b[0m".to_string()
            } else {
                format!(" \x1b[90m\"{}\"\x1b[0m", value)
            }
        }
        MenuItem::Select { options, selected: idx, .. } => {
            let val = options.get(*idx).map(|s| s.as_str()).unwrap_or("");
            format!(" \x1b[90m[{}]\x1b[0m", val)
        }
        MenuItem::Submenu { .. } => " \x1b[90m\u{25b8}\x1b[0m".to_string(),
        MenuItem::Button { .. } => String::new(),
    };

    // Button items render without brackets, aligned with other items
    if matches!(item, MenuItem::Button { .. }) {
        if selected {
            return format!("\r{}\x1b[1;7m{}\x1b[0m\x1b[K", indent, label);
        } else {
            return format!("\r{}{}\x1b[K", indent, label);
        }
    }

    if selected {
        format!("\r{}\x1b[1;7m{}\x1b[0m{}\x1b[K", indent, label, suffix)
    } else {
        format!("\r{}{}{}\x1b[K", indent, label, suffix)
    }
}

fn render_hint(remaining_below: usize, item: Option<&MenuItem>) -> String {
    let action = match item {
        None => "confirm",  // editing mode
        Some(MenuItem::Submenu { .. }) => "open",
        Some(MenuItem::Toggle { .. }) => "toggle",
        Some(MenuItem::Select { .. }) => "select",
        Some(MenuItem::TextInput { .. }) => "edit",
        Some(MenuItem::Button { .. }) => "confirm",
    };
    let hint = match item {
        None => format!("Enter {}  ESC cancel", action),
        Some(_) => format!("\u{2191}\u{2193} move  Enter {}  ESC back  ^C quit", action),
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
    let tl = total_lines(items.len());
    let mut out = String::new();

    // Push screen content up
    for _ in 0..tl {
        out.push_str("\r\n");
    }
    out.push_str(&format!("\x1b[{}A", tl));

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
    out.push_str(&common::render_separator(cols));
    out.push_str("\r\n");

    // Visible items
    let end = (scroll_offset + vis).min(items.len());
    for (i, item) in items.iter().enumerate().take(end).skip(scroll_offset) {
        out.push_str(&render_menu_item(item, i == cursor));
        if i < end - 1 {
            out.push_str("\r\n");
        }
    }
    out.push_str("\r\n");

    // Bottom separator
    let remaining_below = items.len().saturating_sub(end);
    out.push_str(&common::render_separator(cols));
    out.push_str("\r\n");

    // Hint
    out.push_str(&render_hint(remaining_below, Some(&items[cursor])));

    out
}

fn render_cleanup(total_items: usize) -> String {
    let tl = total_lines(total_items);
    let up = tl - 1;
    format!("\x1b[{}A\r\x1b[J", up)
}

// ── Text editing (char-aware) ───────────────────────────────────────────

/// Render a text input line in edit mode.
/// Keeps the same layout as the menu item: `  {label} {value}` with value highlighted.
fn render_edit_line(label: &str, text: &str, cols: u16) -> String {
    let indent = "  ";
    let prefix_len = indent.len() + label.len() + 1; // "  " + label + " "
    let available = (cols as usize).saturating_sub(prefix_len + 1);

    let chars: Vec<char> = text.chars().collect();
    let display_text: String = if chars.len() > available {
        chars[chars.len() - available..].iter().collect()
    } else {
        text.to_string()
    };

    // Dark background + bright text for edit mode (distinct from bold-inverse selected highlight)
    format!(
        "\r{}{} \x1b[48;5;236m\x1b[38;5;255m{}\x1b[0m\x1b[K",
        indent, label, display_text,
    )
}

/// Compute terminal column for the cursor within the edit line.
fn edit_cursor_col(label: &str, text: &str, char_cursor: usize, cols: u16) -> usize {
    let prefix_len = 2 + label.len() + 1; // "  " + label + " "
    let available = (cols as usize).saturating_sub(prefix_len + 1);
    let chars: Vec<char> = text.chars().collect();
    let display_offset = if chars.len() > available {
        chars.len() - available
    } else {
        0
    };
    let visual_pos = char_cursor.saturating_sub(display_offset);
    prefix_len + visual_pos
}

/// Run inline text editor. Returns Some(new_value) on Enter, None on ESC/Ctrl-C.
///
/// Cursor starts at the hint line (bottom of widget). We:
/// 1. Update hint to "Enter confirm  ESC cancel"
/// 2. Move up to the edit line and redraw it with highlight
/// 3. Keep cursor on the edit line throughout editing
/// 4. On exit, move cursor back to the hint line
fn run_text_edit(
    label: &str,
    initial: &str,
    cursor_row_from_bottom: usize,
    cols: u16,
) -> Option<String> {
    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut chars: Vec<char> = initial.chars().collect();
    let mut char_cursor = chars.len();
    let mut buf = [0u8; 4];
    let rfb = cursor_row_from_bottom;

    // -- Phase 1: Setup (cursor starts at hint line) --

    // Update hint line in place (cursor is already there)
    let hint = render_hint(0, None);
    common::write_stdout(b"\r");
    common::write_stdout(hint.as_bytes());

    // Move up to the edit line
    if rfb > 0 {
        common::write_stdout(format!("\x1b[{}A", rfb).as_bytes());
    }

    // Draw edit line content
    let text: String = chars.iter().collect();
    let line = render_edit_line(label, &text, cols);
    common::write_stdout(line.as_bytes());

    // Position cursor within the value
    let col = edit_cursor_col(label, &text, char_cursor, cols);
    common::write_stdout(format!("\r\x1b[{}C", col).as_bytes());

    // Show cursor
    common::write_stdout(b"\x1b[?25h");

    // -- Phase 2: Edit loop (cursor stays on edit line) --

    // Helper: redraw current line and reposition cursor (no vertical movement)
    let redraw_in_place = |chars: &[char], char_cursor: usize| {
        let text: String = chars.iter().collect();
        let line = render_edit_line(label, &text, cols);
        common::write_stdout(line.as_bytes());
        let col = edit_cursor_col(label, &text, char_cursor, cols);
        common::write_stdout(format!("\r\x1b[{}C", col).as_bytes());
    };

    // Helper: exit edit — move back to hint line, hide cursor
    let exit_edit = |rfb: usize| {
        if rfb > 0 {
            common::write_stdout(format!("\x1b[{}B", rfb).as_bytes());
        }
        common::write_stdout(b"\x1b[?25l");
    };

    while let Ok(n) = nix::unistd::read(stdin_fd, &mut buf) {
        if n == 0 { break; }

        match buf[0] {
            b'\r' | b'\n' => {
                exit_edit(rfb);
                return Some(chars.into_iter().collect());
            }
            0x03 => {
                exit_edit(rfb);
                return None;
            }
            0x1b => {
                // Arrow keys may arrive as a single 3-byte read (\x1b[A),
                // so check buf first before reading more from stdin.
                let seq = if n >= 3 && buf[1] == b'[' {
                    Some([buf[1], buf[2]])
                } else {
                    common::parse_esc_seq(stdin_fd)
                };
                if let Some(seq) = seq {
                    if seq[0] == b'[' {
                        match seq[1] {
                            b'D' if char_cursor > 0 => char_cursor -= 1,
                            b'C' if char_cursor < chars.len() => char_cursor += 1,
                            b'H' => char_cursor = 0,
                            b'F' => char_cursor = chars.len(),
                            _ => {}
                        }
                    }
                } else {
                    exit_edit(rfb);
                    return None;
                }
            }
            0x7f | 0x08 => {
                if char_cursor > 0 {
                    chars.remove(char_cursor - 1);
                    char_cursor -= 1;
                }
            }
            b if (0x20..0x80).contains(&b) => {
                for &byte in buf.iter().take(n) {
                    if (0x20..0x80).contains(&byte) {
                        chars.insert(char_cursor, byte as char);
                        char_cursor += 1;
                    }
                }
            }
            b if b >= 0x80 => {
                if let Ok(s) = std::str::from_utf8(&buf[..n]) {
                    for ch in s.chars() {
                        chars.insert(char_cursor, ch);
                        char_cursor += 1;
                    }
                }
            }
            _ => {}
        }

        redraw_in_place(&chars, char_cursor);
    }

    exit_edit(rfb);
    None
}

// ── Select sub-picker ───────────────────────────────────────────────────

/// Run a flat picker for Select items. Returns selected index or None.
fn run_select(title: &str, options: &[String], initial: usize) -> Option<usize> {
    let refs: Vec<&str> = options.iter().map(|s| s.as_str()).collect();
    super::picker::pick_one_at(title, &refs, initial)
}

// ── Navigation helpers ──────────────────────────────────────────────────

/// Navigation stack entry.
struct NavEntry {
    parent_index: usize,
    cursor: usize,
    scroll_offset: usize,
}

/// Dispatch a non-form-mode change: call `on_change` if provided, otherwise
/// accumulate in `changes` for batch return on menu exit.
fn dispatch_change(
    change: MenuChange,
    in_form_mode: bool,
    changes: &mut Vec<MenuChange>,
    on_change: &mut MenuChangeHandler,
) -> bool {
    if in_form_mode {
        changes.push(change);
        true
    } else if let Some(ref mut cb) = on_change {
        cb(&change)
    } else {
        changes.push(change);
        true
    }
}

/// Handle TextInput edit: enters inline editor, applies result.
/// Returns (confirmed, change) — confirmed is true if user pressed Enter.
fn handle_text_edit(
    item: &mut MenuItem,
    breadcrumb_parts: &[String],
    row_from_bottom: usize,
    cols: u16,
) -> (bool, Option<MenuChange>) {
    let MenuItem::TextInput { label, value } = item else { return (false, None) };
    let label_clone = label.clone();
    let old_value = value.clone();

    let result = run_text_edit(&label_clone, &old_value, row_from_bottom, cols);
    match result {
        Some(new_val) => {
            *value = new_val.clone();
            let change = if new_val != old_value {
                let path = build_path(breadcrumb_parts, &label_clone);
                Some(MenuChange { path, value: new_val })
            } else {
                None
            };
            (true, change)
        }
        None => {
            *value = old_value;
            (false, None)
        }
    }
}

/// Rebuild `current_items` pointer by traversing nav_stack from root.
fn resolve_level<'a>(items: &'a mut [MenuItem], nav_stack: &[NavEntry]) -> &'a mut [MenuItem] {
    let mut level = items;
    for nav in nav_stack {
        level = match &mut level[nav.parent_index] {
            MenuItem::Submenu { children, .. } => children.as_mut_slice(),
            _ => unreachable!(),
        };
    }
    level
}

// ── Main menu loop ──────────────────────────────────────────────────────

/// Run the multi-level menu widget. Returns changes made or Cancelled.
///
/// `on_change`: if provided, called immediately for each non-form-mode item
/// change (Toggle/Select/TextInput) instead of accumulating them in the
/// returned `MenuResult::Done` vec. If `None`, changes are accumulated and
/// returned on exit (original batch behavior). Form-mode changes always
/// accumulate for the handler callback / Done button regardless.
pub fn run_menu(
    title: &str,
    items: &mut Vec<MenuItem>,
    mut on_handler_exit: MenuExitHandler,
    mut on_change: MenuChangeHandler,
) -> MenuResult {
    if items.is_empty() {
        return MenuResult::Done(vec![]);
    }

    let cols = common::terminal_cols();
    let mut changes: Vec<MenuChange> = Vec::new();
    let mut nav_stack: Vec<NavEntry> = Vec::new();
    let mut breadcrumb_parts: Vec<String> = vec![title.to_string()];

    // Current level state
    let mut cursor: usize = 0;
    let mut scroll_offset: usize = 0;

    // Hide cursor
    common::write_stdout(b"\x1b[?25l");

    // Initial render
    {
        let current_items = resolve_level(items.as_mut_slice(), &nav_stack);
        let bc = breadcrumb_parts.join(" > ");
        let full = render_full(&bc, current_items, cursor, cols, scroll_offset);
        common::write_stdout(full.as_bytes());
    }

    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut byte = [0u8; 1];

    let mut last_item_count = items.len();
    let mut needs_redraw = false;
    let mut pending_auto_edit = false; // form_mode: auto-enter text edit after redraw
    let mut auto_edit_advance = false; // true = advance cursor after auto-edit (Down/Enter), false = stay (Up)
    let mut form_auto_edit_active = true; // form_mode: when false, items require Enter to edit (disabled when cursor reaches Button)

    loop {
        // 1. Read handler name and form_mode FIRST (immutable borrow of items).
        let (current_handler, in_form_mode): (Option<String>, bool) = if !nav_stack.is_empty() {
            let last = nav_stack.last().unwrap();
            let mut node: &[MenuItem] = items.as_slice();
            for entry in &nav_stack[..nav_stack.len() - 1] {
                match &node[entry.parent_index] {
                    MenuItem::Submenu { children, .. } => node = children,
                    _ => break,
                }
            }
            match &node[last.parent_index] {
                MenuItem::Submenu { handler, form_mode, .. } => (
                    handler.clone(),
                    *form_mode,
                ),
                _ => (None, false),
            }
        } else {
            (None, false)
        };

        // 2. Re-derive current_items (mutable borrow of items).
        let current_items: &mut [MenuItem] = {
            let mut slice = items.as_mut_slice();
            for entry in &nav_stack {
                match &mut slice[entry.parent_index] {
                    MenuItem::Submenu { children, .. } => slice = children.as_mut_slice(),
                    _ => unreachable!(),
                }
            }
            slice
        };

        // 3. Redraw if nav changed (push/pop/handler reset).
        if needs_redraw {
            needs_redraw = false;
            last_item_count = current_items.len();
            let bc = breadcrumb_parts.join(" > ");
            let full = render_full(&bc, current_items, cursor, cols, scroll_offset);
            common::write_stdout(full.as_bytes());
        }

        // 3b. Form mode: auto-enter text edit after redraw
        if pending_auto_edit && in_form_mode && form_auto_edit_active {
            pending_auto_edit = false;
            if cursor < current_items.len() && matches!(current_items[cursor], MenuItem::TextInput { .. }) {
                let vis = visible_count(current_items.len());
                let cursor_vis_pos = cursor - scroll_offset;
                let rfb = lines_below_cursor(vis, cursor_vis_pos);
                let (confirmed, change) = handle_text_edit(
                    &mut current_items[cursor],
                    &breadcrumb_parts,
                    rfb,
                    cols,
                );
                if let Some(change) = change {
                    dispatch_change(change, in_form_mode, &mut changes, &mut on_change);
                }
                // After text edit returns, advance cursor only on confirm (not cancel)
                if confirmed && auto_edit_advance && cursor < current_items.len() - 1 {
                    cursor += 1;
                    if cursor >= scroll_offset + vis {
                        scroll_offset = cursor - vis + 1;
                    }
                    // Chain: if next item is also TextInput, set pending again
                    if matches!(current_items[cursor], MenuItem::Button { .. }) {
                        form_auto_edit_active = false;
                    }
                    if form_auto_edit_active && matches!(current_items[cursor], MenuItem::TextInput { .. }) {
                        pending_auto_edit = true;
                    }
                }
                // Redraw
                let bc = breadcrumb_parts.join(" > ");
                let tl = total_lines(current_items.len());
                common::write_stdout(format!("\x1b[{}A\r\x1b[J", tl - 1).as_bytes());
                let full = render_full(&bc, current_items, cursor, cols, scroll_offset);
                common::write_stdout(full.as_bytes());
                last_item_count = current_items.len();
                continue;
            }
        }

        if nix::unistd::read(stdin_fd, &mut byte) != Ok(1) {
            break;
        }

        match byte[0] {
            0x03 => {
                // Ctrl-C: quit entirely
                let cleanup = render_cleanup(last_item_count);
                common::write_stdout(cleanup.as_bytes());
                common::write_stdout(b"\x1b[?25h");
                return MenuResult::Cancelled;
            }
            0x1b => {
                if let Some(seq) = common::parse_esc_seq(stdin_fd) {
                    if seq[0] == b'[' {
                        let vis = visible_count(current_items.len());
                        match seq[1] {
                            b'A' if cursor > 0 => {
                                cursor -= 1;
                                if cursor < scroll_offset {
                                    scroll_offset = cursor;
                                    let bc = breadcrumb_parts.join(" > ");
                                    full_redraw(&bc, current_items, cursor, cols, scroll_offset, vis);
                                } else {
                                    incremental_redraw(current_items, cursor, cursor + 1, vis, scroll_offset);
                                }
                                if in_form_mode && form_auto_edit_active && matches!(current_items[cursor], MenuItem::TextInput { .. }) {
                                    pending_auto_edit = true;
                                    auto_edit_advance = false;
                                }
                                if in_form_mode && matches!(current_items[cursor], MenuItem::Button { .. }) {
                                    form_auto_edit_active = false;
                                }
                            }
                            b'B' if cursor < current_items.len().saturating_sub(1) => {
                                cursor += 1;
                                if cursor >= scroll_offset + vis {
                                    scroll_offset = cursor - vis + 1;
                                    let bc = breadcrumb_parts.join(" > ");
                                    full_redraw(&bc, current_items, cursor, cols, scroll_offset, vis);
                                } else {
                                    incremental_redraw(current_items, cursor, cursor - 1, vis, scroll_offset);
                                }
                                if in_form_mode && form_auto_edit_active && matches!(current_items[cursor], MenuItem::TextInput { .. }) {
                                    pending_auto_edit = true;
                                    auto_edit_advance = true;
                                }
                                if in_form_mode && matches!(current_items[cursor], MenuItem::Button { .. }) {
                                    form_auto_edit_active = false;
                                }
                            }
                            _ => {}
                        }
                    }
                } else {
                    // Bare ESC: go back one level or exit
                    if nav_stack.is_empty() {
                        let cleanup = render_cleanup(last_item_count);
                        common::write_stdout(cleanup.as_bytes());
                        common::write_stdout(b"\x1b[?25h");
                        return MenuResult::Done(changes);
                    }

                    // Handler detection: when leaving a handler submenu, call the callback
                    let mut already_cleaned_up = false;
                    if let Some(ref handler_name) = current_handler {
                        if let Some(ref mut callback) = on_handler_exit {
                            let handler_prefix = breadcrumb_parts[1..].join(".");
                            let handler_changes: Vec<MenuChange> = changes.iter()
                                .filter(|c| c.path.starts_with(&handler_prefix))
                                .cloned()
                                .collect();

                            // Remove handler changes from main changes vec
                            changes.retain(|c| !c.path.starts_with(&handler_prefix));

                            if !handler_changes.is_empty() {
                                // Clean up widget BEFORE calling callback so any
                                // stdout output from the callback (e.g. error messages)
                                // doesn't shift the cursor and break cleanup (#469).
                                let cleanup = render_cleanup(last_item_count);
                                common::write_stdout(cleanup.as_bytes());

                                if let Some(new_items) = callback(handler_name, handler_changes) {
                                    common::write_stdout(b"\x1b[J"); // clear any callback output
                                    *items = new_items;
                                    nav_stack.clear();
                                    breadcrumb_parts.truncate(1);
                                    cursor = 0;
                                    scroll_offset = 0;
                                    form_auto_edit_active = true;
                                    needs_redraw = true;
                                    continue;
                                }

                                // Handler error: clear any callback output
                                common::write_stdout(b"\x1b[J");
                                already_cleaned_up = true;
                            }
                        }
                    }

                    // Reset form fields before popping (prevents stale data on re-entry)
                    if in_form_mode {
                        for item in current_items.iter_mut() {
                            match item {
                                MenuItem::TextInput { value, .. } => *value = String::new(),
                                MenuItem::Select { selected, .. } => *selected = 0,
                                _ => {}
                            }
                        }
                    }

                    // Normal pop
                    let entry = nav_stack.pop().unwrap();
                    cursor = entry.cursor;
                    scroll_offset = entry.scroll_offset;
                    breadcrumb_parts.pop();
                    if !already_cleaned_up {
                        let cleanup = render_cleanup(last_item_count);
                        common::write_stdout(cleanup.as_bytes());
                    }
                    needs_redraw = true;
                    continue;
                }
            }
            b'\r' | b'\n' => {
                let vis = visible_count(current_items.len());
                let cursor_vis_pos = cursor - scroll_offset;
                let row_from_bottom = lines_below_cursor(vis, cursor_vis_pos);

                match &mut current_items[cursor] {
                    MenuItem::Submenu { label, children, form_mode, .. } => {
                        if children.is_empty() {
                            continue;
                        }
                        // Auto-append Confirm button for form_mode submenus
                        if *form_mode && !matches!(children.last(), Some(MenuItem::Button { .. })) {
                            children.push(MenuItem::Button { label: "Done".to_string() });
                        }
                        let label_clone = label.clone();
                        let entering_form = *form_mode;

                        let cleanup = render_cleanup(last_item_count);
                        common::write_stdout(cleanup.as_bytes());

                        nav_stack.push(NavEntry {
                            parent_index: cursor,
                            cursor,
                            scroll_offset,
                        });
                        breadcrumb_parts.push(label_clone);

                        cursor = 0;
                        scroll_offset = 0;
                        needs_redraw = true;
                        pending_auto_edit = entering_form;
                        auto_edit_advance = true;
                        form_auto_edit_active = true;
                        continue;
                    }
                    MenuItem::Toggle { label, value } => {
                        *value = !*value;
                        let path = build_path(&breadcrumb_parts, label);
                        let change = MenuChange {
                            path,
                            value: value.to_string(),
                        };
                        if !dispatch_change(change, in_form_mode, &mut changes, &mut on_change) {
                            *value = !*value;
                        }

                        if in_form_mode && cursor < current_items.len() - 1 {
                            // Form mode: advance to next item
                            cursor += 1;
                            if cursor >= scroll_offset + vis {
                                scroll_offset = cursor - vis + 1;
                            }
                            if matches!(current_items[cursor], MenuItem::Button { .. }) {
                                form_auto_edit_active = false;
                            }
                            pending_auto_edit = form_auto_edit_active && matches!(current_items[cursor], MenuItem::TextInput { .. });
                            auto_edit_advance = true;
                            let bc = breadcrumb_parts.join(" > ");
                            let tl = total_lines(current_items.len());
                            common::write_stdout(format!("\x1b[{}A\r\x1b[J", tl - 1).as_bytes());
                            let full = render_full(&bc, current_items, cursor, cols, scroll_offset);
                            common::write_stdout(full.as_bytes());
                        } else {
                            // Normal mode: redraw just the current item
                            common::write_stdout(format!("\x1b[{}A", row_from_bottom).as_bytes());
                            let line = render_menu_item(&current_items[cursor], true);
                            common::write_stdout(line.as_bytes());
                            common::write_stdout(format!("\x1b[{}B", row_from_bottom).as_bytes());
                        }
                    }
                    MenuItem::Select { label, options, selected, prefills } => {
                        let label_clone = label.clone();
                        let options_clone = options.clone();
                        let prefills_clone = prefills.clone();
                        let old_selected = *selected;

                        let cleanup = render_cleanup(last_item_count);
                        common::write_stdout(cleanup.as_bytes());

                        let select_title = format!("{} > {}", breadcrumb_parts.join(" > "), label_clone);
                        let mut prefill_applied = false;
                        if let Some(idx) = run_select(&select_title, &options_clone, old_selected) {
                            *selected = idx;
                            if idx != old_selected {
                                let path = build_path(&breadcrumb_parts, &label_clone);
                                let change = MenuChange {
                                    path,
                                    value: options_clone[idx].clone(),
                                };
                                if !dispatch_change(change, in_form_mode, &mut changes, &mut on_change) {
                                    *selected = old_selected;
                                }
                            }

                            // Apply prefills to sibling items
                            if !prefills_clone.is_empty() {
                                let selected_option = &options_clone[idx];
                                if let Some((_, fields)) = prefills_clone.iter().find(|(name, _)| name == selected_option) {
                                    for (sibling_label, value) in fields {
                                        for item in current_items.iter_mut() {
                                            match item {
                                                MenuItem::TextInput { label: l, value: v } if l == sibling_label => {
                                                    *v = value.clone();
                                                }
                                                MenuItem::Select { label: l, options: opts, selected: sel, .. } if l == sibling_label => {
                                                    if let Some(pos) = opts.iter().position(|o| o == value) {
                                                        *sel = pos;
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    // Disable auto-edit so user can review prefilled fields
                                    form_auto_edit_active = false;
                                    prefill_applied = true;
                                }
                            }
                        }

                        if prefill_applied {
                            // Prefill applied: don't auto-advance, let user review filled fields
                        } else if in_form_mode && cursor < current_items.len() - 1 {
                            cursor += 1;
                            if cursor >= scroll_offset + vis {
                                scroll_offset = cursor - vis + 1;
                            }
                            if matches!(current_items[cursor], MenuItem::Button { .. }) {
                                form_auto_edit_active = false;
                            }
                            pending_auto_edit = form_auto_edit_active && matches!(current_items[cursor], MenuItem::TextInput { .. });
                            auto_edit_advance = true;
                        }

                        let bc = breadcrumb_parts.join(" > ");
                        let full = render_full(&bc, current_items, cursor, cols, scroll_offset);
                        common::write_stdout(full.as_bytes());
                    }
                    MenuItem::TextInput { .. } => {
                        let old_text = match &current_items[cursor] {
                            MenuItem::TextInput { value, .. } => value.clone(),
                            _ => unreachable!(),
                        };
                        let (confirmed, change) = handle_text_edit(
                            &mut current_items[cursor],
                            &breadcrumb_parts,
                            row_from_bottom,
                            cols,
                        );
                        if let Some(change) = change {
                            if !dispatch_change(change, in_form_mode, &mut changes, &mut on_change) {
                                if let MenuItem::TextInput { value, .. } = &mut current_items[cursor] {
                                    *value = old_text;
                                }
                            }
                        }
                        if confirmed && in_form_mode && cursor < current_items.len() - 1 {
                            cursor += 1;
                            if cursor >= scroll_offset + vis {
                                scroll_offset = cursor - vis + 1;
                            }
                            if matches!(current_items[cursor], MenuItem::Button { .. }) {
                                form_auto_edit_active = false;
                            }
                            pending_auto_edit = form_auto_edit_active && matches!(current_items[cursor], MenuItem::TextInput { .. });
                            auto_edit_advance = true;
                        }

                        // Full redraw to restore hint and clean up
                        let bc = breadcrumb_parts.join(" > ");
                        let tl = total_lines(current_items.len());
                        common::write_stdout(format!("\x1b[{}A\r\x1b[J", tl - 1).as_bytes());
                        let full = render_full(&bc, current_items, cursor, cols, scroll_offset);
                        common::write_stdout(full.as_bytes());
                    }
                    MenuItem::Button { .. } => {
                        // Button confirm: trigger handler and pop level
                        if let Some(ref handler_name) = current_handler {
                            if let Some(ref mut callback) = on_handler_exit {
                                let handler_prefix = breadcrumb_parts[1..].join(".");
                                // Collect current item values directly (not from changes)
                                // so that default/unchanged Select values are included.
                                let handler_changes: Vec<MenuChange> = current_items.iter()
                                    .filter_map(|item| {
                                        let (label, value) = match item {
                                            MenuItem::TextInput { label, value } => (label.clone(), value.clone()),
                                            MenuItem::Select { label, options, selected, .. } => (label.clone(), options[*selected].clone()),
                                            MenuItem::Toggle { label, value } => (label.clone(), value.to_string()),
                                            _ => return None,
                                        };
                                        Some(MenuChange {
                                            path: format!("{}.{}", handler_prefix, label),
                                            value,
                                        })
                                    })
                                    .collect();
                                changes.retain(|c| !c.path.starts_with(&handler_prefix));
                                if !handler_changes.is_empty() {
                                    if let Some(new_items) = callback(handler_name, handler_changes) {
                                        *items = new_items;
                                        nav_stack.clear();
                                        breadcrumb_parts.truncate(1);
                                        cursor = 0;
                                        scroll_offset = 0;
                                        form_auto_edit_active = true;
                                        let cleanup = render_cleanup(last_item_count);
                                        common::write_stdout(cleanup.as_bytes());
                                        needs_redraw = true;
                                        continue;
                                    }
                                }
                            }
                        }

                        // Callback returned None or no handler: reset form fields
                        for item in current_items.iter_mut() {
                            match item {
                                MenuItem::TextInput { value, .. } => *value = String::new(),
                                MenuItem::Select { selected, .. } => *selected = 0,
                                _ => {}
                            }
                        }

                        if nav_stack.is_empty() {
                            let cleanup = render_cleanup(last_item_count);
                            common::write_stdout(cleanup.as_bytes());
                            common::write_stdout(b"\x1b[?25h");
                            return MenuResult::Done(changes);
                        }

                        // Normal pop
                        let entry = nav_stack.pop().unwrap();
                        cursor = entry.cursor;
                        scroll_offset = entry.scroll_offset;
                        breadcrumb_parts.pop();
                        form_auto_edit_active = true;
                        let cleanup = render_cleanup(last_item_count);
                        common::write_stdout(cleanup.as_bytes());
                        needs_redraw = true;
                        continue;
                    }
                }
            }
            _ => {}
        }

        // Update last_item_count for next iteration
        last_item_count = current_items.len();
    }

    let cleanup = render_cleanup(last_item_count);
    common::write_stdout(cleanup.as_bytes());
    common::write_stdout(b"\x1b[?25h");

    // Deduplicate: keep only the last change for each path (#411)
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for change in changes.into_iter().rev() {
        if seen.insert(change.path.clone()) {
            deduped.push(change);
        }
    }
    deduped.reverse();
    MenuResult::Done(deduped)
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
    let tl = CHROME_LINES + vis;
    common::write_stdout(format!("\x1b[{}A\r\x1b[J", tl - 1).as_bytes());
    let full = render_full(breadcrumb, items, cursor, cols, scroll_offset);
    common::write_stdout(full.as_bytes());
}

/// Incremental redraw: update only the old and new cursor lines.
fn incremental_redraw(
    items: &[MenuItem],
    new_cursor: usize,
    old_cursor: usize,
    vis: usize,
    scroll_offset: usize,
) {
    let old_vis_pos = old_cursor - scroll_offset;
    let up_to_old = lines_below_cursor(vis, old_vis_pos);
    common::write_stdout(format!("\x1b[{}A", up_to_old).as_bytes());
    let line = render_menu_item(&items[old_cursor], false);
    common::write_stdout(line.as_bytes());

    if new_cursor < old_cursor {
        common::write_stdout(b"\x1b[1A");
    } else {
        common::write_stdout(b"\x1b[1B");
    }
    let line = render_menu_item(&items[new_cursor], true);
    common::write_stdout(line.as_bytes());

    let new_vis_pos = new_cursor - scroll_offset;
    let down = lines_below_cursor(vis, new_vis_pos);
    // Move down to hint line (skip separator) and update hint
    common::write_stdout(format!("\x1b[{}B", down).as_bytes());
    let remaining = items.len().saturating_sub(scroll_offset + vis);
    let hint = render_hint(remaining, Some(&items[new_cursor]));
    common::write_stdout(hint.as_bytes());
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
            handler: None,
            form_mode: false,
        };
        assert_eq!(item.label(), "LLM");
    }

    #[test]
    fn test_render_menu_item_submenu() {
        let item = MenuItem::Submenu {
            label: "LLM".to_string(),
            children: vec![],
            handler: None,
            form_mode: false,
        };
        let line = render_menu_item(&item, false);
        let text = common::strip_ansi(&line);
        assert!(text.contains("LLM \u{25b8}"));
    }

    #[test]
    fn test_render_menu_item_toggle_on() {
        let item = MenuItem::Toggle {
            label: "Enabled".to_string(),
            value: true,
        };
        let line = render_menu_item(&item, false);
        let text = common::strip_ansi(&line);
        assert!(text.contains("Enabled"));
        assert!(text.contains("[ON]"));
    }

    #[test]
    fn test_render_menu_item_toggle_off() {
        let item = MenuItem::Toggle {
            label: "Enabled".to_string(),
            value: false,
        };
        let line = render_menu_item(&item, false);
        let text = common::strip_ansi(&line);
        assert!(text.contains("[OFF]"));
    }

    #[test]
    fn test_render_menu_item_select() {
        let item = MenuItem::Select {
            label: "Backend".to_string(),
            options: vec!["claude".to_string(), "openai".to_string()],
            selected: 0,
            prefills: vec![],
        };
        let line = render_menu_item(&item, false);
        let text = common::strip_ansi(&line);
        assert!(text.contains("Backend [claude]"));
    }

    #[test]
    fn test_render_menu_item_text_input() {
        let item = MenuItem::TextInput {
            label: "Proxy".to_string(),
            value: "http://proxy:8080".to_string(),
        };
        let line = render_menu_item(&item, false);
        let text = common::strip_ansi(&line);
        assert!(text.contains("Proxy \"http://proxy:8080\""));
    }

    #[test]
    fn test_render_menu_item_text_input_empty() {
        let item = MenuItem::TextInput {
            label: "Proxy".to_string(),
            value: String::new(),
        };
        let line = render_menu_item(&item, false);
        let text = common::strip_ansi(&line);
        assert!(text.contains("Proxy"));
        assert!(text.contains("(empty)"));
    }

    #[test]
    fn test_render_hint_toggle() {
        let item = MenuItem::Toggle { label: "X".to_string(), value: true };
        let hint = render_hint(0, Some(&item));
        assert!(hint.contains("move"));
        assert!(hint.contains("toggle"));
        assert!(hint.contains("back"));
    }

    #[test]
    fn test_render_hint_submenu() {
        let item = MenuItem::Submenu { label: "X".to_string(), children: vec![], handler: None, form_mode: false };
        let hint = render_hint(0, Some(&item));
        assert!(hint.contains("open"));
    }

    #[test]
    fn test_render_hint_select() {
        let item = MenuItem::Select { label: "X".to_string(), options: vec![], selected: 0, prefills: vec![] };
        let hint = render_hint(0, Some(&item));
        assert!(hint.contains("select"));
    }

    #[test]
    fn test_render_hint_text_input() {
        let item = MenuItem::TextInput { label: "X".to_string(), value: String::new() };
        let hint = render_hint(0, Some(&item));
        assert!(hint.contains("edit"));
    }

    #[test]
    fn test_render_hint_editing() {
        let hint = render_hint(0, None);
        assert!(hint.contains("confirm"));
        assert!(hint.contains("cancel"));
        assert!(!hint.contains("move"));
    }

    #[test]
    fn test_render_hint_with_scroll() {
        let item = MenuItem::Toggle { label: "X".to_string(), value: true };
        let hint = render_hint(5, Some(&item));
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
                handler: None,
                form_mode: false,
            },
        ];
        let output = render_full("Config", &items, 0, 60, 0);
        let text = common::strip_ansi(&output);
        assert!(text.contains("Config"));
        assert!(text.contains("Enabled"));
        assert!(text.contains("LLM"));
    }

    #[test]
    fn test_empty_menu_returns_done() {
        let result = run_menu("Empty", &mut vec![], None, None);
        match result {
            MenuResult::Done(changes) => assert!(changes.is_empty()),
            MenuResult::Cancelled => panic!("Expected Done"),
        }
    }

    #[test]
    fn test_total_lines() {
        assert_eq!(total_lines(3), 7);  // 4 chrome + 3 items
        assert_eq!(total_lines(15), 14); // 4 chrome + 10 (capped)
    }

    #[test]
    fn test_lines_below_cursor() {
        // 5 visible items, cursor at position 2 (3rd item): 2 remaining + 2 (sep+hint) = 4
        assert_eq!(lines_below_cursor(5, 2), 4);
        // cursor at last item: 0 remaining + 2 = 2
        assert_eq!(lines_below_cursor(5, 4), 2);
    }

    #[test]
    fn test_edit_cursor_col() {
        // layout: "  Proxy hello" — indent(2) + label(5) + space(1) + text
        // cursor at end of "hello" (pos 5)
        let col = edit_cursor_col("Proxy", "hello", 5, 40);
        // 2 + 5 + 1 + 5 = 13
        assert_eq!(col, 13);

        // cursor at start of text (pos 0)
        let col = edit_cursor_col("Proxy", "hello", 0, 40);
        // 2 + 5 + 1 + 0 = 8
        assert_eq!(col, 8);
    }
}
