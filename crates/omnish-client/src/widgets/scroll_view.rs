/// A two-mode scrollable content viewer.
///
/// - **Compact mode** (default): shows the last `compact_height` lines, like a
///   tail view.  New lines added via `push_line()` auto-scroll to the bottom.
/// - **Expanded mode**: shows `expanded_height` lines with a scrollbar on the
///   right edge and a hint line at the bottom.  The user can scroll with ↑↓/j/k
///   and exit with q/Esc.
///
/// Rendering uses ANSI cursor movement (same technique as Picker and LineStatus)
/// — no alternate screen.

/// Scrollbar block characters.
const THUMB: &str = "\x1b[2m\u{2590}\x1b[0m"; // ▐ (dim)
const TRACK: &str = "\x1b[2m\u{2502}\x1b[0m"; // │ (dim)

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewMode {
    Compact,
    Expanded,
}

pub struct ScrollView {
    /// All content lines (already rendered with ANSI styles, using \r\n).
    lines: Vec<String>,
    /// Number of visible lines in compact mode.
    compact_height: usize,
    /// Number of content lines visible in expanded mode (excluding hint).
    expanded_height: usize,
    /// Current scroll offset (top line index) in expanded mode.
    scroll_offset: usize,
    /// How many screen lines we currently occupy (for erase).
    rendered_lines: usize,
    /// Current mode.
    mode: ViewMode,
    /// Maximum display width per line.
    max_cols: usize,
}

impl ScrollView {
    pub fn new(compact_height: usize, expanded_height: usize, max_cols: usize) -> Self {
        Self {
            lines: Vec::new(),
            compact_height,
            expanded_height,
            scroll_offset: 0,
            rendered_lines: 0,
            mode: ViewMode::Compact,
            max_cols,
        }
    }

    pub fn mode(&self) -> ViewMode {
        self.mode
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Add a line to the buffer.  In compact mode, returns ANSI to redraw the
    /// tail.  In expanded mode, the line is buffered but no redraw happens
    /// (the user controls scrolling).
    pub fn push_line(&mut self, line: &str) -> String {
        self.lines.push(line.to_string());
        if self.mode == ViewMode::Compact {
            self.render_compact()
        } else {
            String::new()
        }
    }

    /// Enter expanded (browse) mode.  Returns ANSI to redraw.
    pub fn enter_browse(&mut self) -> String {
        self.mode = ViewMode::Expanded;
        // Scroll to bottom initially
        let total = self.lines.len();
        self.scroll_offset = total.saturating_sub(self.expanded_height);
        self.render_expanded()
    }

    /// Exit expanded mode, return to compact.  Returns ANSI to redraw.
    pub fn exit_browse(&mut self) -> String {
        self.mode = ViewMode::Compact;
        self.render_compact()
    }

    /// Scroll up by `n` lines in expanded mode.  Returns ANSI to redraw.
    pub fn scroll_up(&mut self, n: usize) -> String {
        if self.mode != ViewMode::Expanded {
            return String::new();
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        self.render_expanded()
    }

    /// Scroll down by `n` lines in expanded mode.  Returns ANSI to redraw.
    pub fn scroll_down(&mut self, n: usize) -> String {
        if self.mode != ViewMode::Expanded {
            return String::new();
        }
        let max_offset = self.lines.len().saturating_sub(self.expanded_height);
        self.scroll_offset = (self.scroll_offset + n).min(max_offset);
        self.render_expanded()
    }

    /// Erase everything from screen.  Returns ANSI sequence.
    pub fn clear(&mut self) -> String {
        let seq = self.erase_seq();
        self.rendered_lines = 0;
        self.lines.clear();
        self.mode = ViewMode::Compact;
        self.scroll_offset = 0;
        seq
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    /// Render compact mode: show last `compact_height` lines.
    fn render_compact(&mut self) -> String {
        let mut out = self.erase_seq();
        let total = self.lines.len();
        let visible = total.min(self.compact_height);
        let start = total.saturating_sub(visible);

        for i in start..total {
            let line = Self::truncate_line(&self.lines[i], self.max_cols);
            out.push_str(&format!("\r\n\x1b[K\x1b[2m{}\x1b[0m", line));
        }

        self.rendered_lines = visible;
        out
    }

    /// Render expanded mode: viewport of `expanded_height` lines + scrollbar + hint.
    fn render_expanded(&mut self) -> String {
        let mut out = self.erase_seq();
        let total = self.lines.len();
        let viewport = self.expanded_height.min(total);
        let end = (self.scroll_offset + viewport).min(total);
        let start = end.saturating_sub(viewport);

        // Compute scrollbar
        let scrollbar = Self::compute_scrollbar(total, viewport, start);

        // Render viewport lines with scrollbar at column max_cols-2
        let bar_col = self.max_cols.saturating_sub(2);
        for (vi, i) in (start..end).enumerate() {
            let bar = if vi < scrollbar.len() {
                &scrollbar[vi]
            } else {
                " "
            };
            let line = Self::truncate_line(&self.lines[i], bar_col.saturating_sub(1));
            out.push_str(&format!("\r\n\x1b[K{}\x1b[0m\x1b[{}G{}", line, bar_col, bar));
        }

        // Hint line
        out.push_str("\r\n\x1b[K\x1b[2m\u{2191}\u{2193}/j/k scroll  q quit\x1b[0m");

        self.rendered_lines = viewport + 1; // viewport + hint
        out
    }

    /// Compute scrollbar characters for each viewport row.
    /// Returns a Vec of &str — either THUMB or TRACK for each row.
    fn compute_scrollbar(total: usize, viewport: usize, scroll_offset: usize) -> Vec<&'static str> {
        if total <= viewport || viewport == 0 {
            // Everything fits — no scrollbar needed
            return vec![" "; viewport];
        }

        let thumb_height = (viewport * viewport / total).max(1);
        let track_range = viewport.saturating_sub(thumb_height);
        let max_offset = total.saturating_sub(viewport);
        let thumb_top = if max_offset > 0 {
            scroll_offset * track_range / max_offset
        } else {
            0
        };
        let thumb_bottom = thumb_top + thumb_height;

        (0..viewport)
            .map(|i| {
                if i >= thumb_top && i < thumb_bottom {
                    THUMB
                } else {
                    TRACK
                }
            })
            .collect()
    }

    /// Erase currently rendered lines (move up + clear each line).
    fn erase_seq(&self) -> String {
        if self.rendered_lines == 0 {
            return String::new();
        }
        let mut out = String::new();
        for i in 0..self.rendered_lines {
            if i > 0 {
                out.push_str("\x1b[1A");
            }
            out.push_str("\r\x1b[K");
        }
        out.push_str("\x1b[1A");
        out
    }

    fn truncate_line(line: &str, max_cols: usize) -> String {
        if max_cols == 0 || line.chars().count() <= max_cols {
            return line.to_string();
        }
        let truncated: String = line.chars().take(max_cols.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
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
    fn test_new_is_compact() {
        let sv = ScrollView::new(3, 10, 80);
        assert_eq!(sv.mode(), ViewMode::Compact);
        assert_eq!(sv.line_count(), 0);
    }

    #[test]
    fn test_push_line_adds_content() {
        let mut sv = ScrollView::new(3, 10, 80);
        sv.push_line("hello");
        sv.push_line("world");
        assert_eq!(sv.line_count(), 2);
    }

    #[test]
    fn test_compact_shows_tail() {
        let mut sv = ScrollView::new(2, 10, 80);
        sv.push_line("line 1");
        sv.push_line("line 2");
        let seq = sv.push_line("line 3");
        let plain = strip_ansi(&seq);
        // Compact height is 2, so only last 2 lines should be visible
        assert!(!plain.contains("line 1"), "line 1 should be hidden");
        assert!(plain.contains("line 2"));
        assert!(plain.contains("line 3"));
    }

    #[test]
    fn test_compact_rendered_lines() {
        let mut sv = ScrollView::new(3, 10, 80);
        sv.push_line("a");
        sv.push_line("b");
        assert_eq!(sv.rendered_lines, 2);
        sv.push_line("c");
        assert_eq!(sv.rendered_lines, 3);
        sv.push_line("d"); // exceeds compact_height
        assert_eq!(sv.rendered_lines, 3); // still 3
    }

    #[test]
    fn test_enter_browse() {
        let mut sv = ScrollView::new(3, 10, 80);
        for i in 0..20 {
            sv.push_line(&format!("line {}", i));
        }
        let seq = sv.enter_browse();
        assert_eq!(sv.mode(), ViewMode::Expanded);
        let plain = strip_ansi(&seq);
        // Should show hint
        assert!(plain.contains("scroll"));
        assert!(plain.contains("quit"));
    }

    #[test]
    fn test_exit_browse() {
        let mut sv = ScrollView::new(3, 10, 80);
        for i in 0..20 {
            sv.push_line(&format!("line {}", i));
        }
        sv.enter_browse();
        let seq = sv.exit_browse();
        assert_eq!(sv.mode(), ViewMode::Compact);
        let plain = strip_ansi(&seq);
        // Back to compact — should show last 3 lines
        assert!(plain.contains("line 19"));
        assert!(plain.contains("line 18"));
        assert!(plain.contains("line 17"));
        assert!(!plain.contains("line 16"));
    }

    #[test]
    fn test_scroll_up() {
        let mut sv = ScrollView::new(3, 5, 80);
        for i in 0..20 {
            sv.push_line(&format!("line {}", i));
        }
        sv.enter_browse();
        // Initially at bottom: offset = 20 - 5 = 15
        assert_eq!(sv.scroll_offset, 15);

        sv.scroll_up(3);
        assert_eq!(sv.scroll_offset, 12);

        let seq = sv.scroll_up(100); // should clamp to 0
        assert_eq!(sv.scroll_offset, 0);
        let plain = strip_ansi(&seq);
        assert!(plain.contains("line 0"));
    }

    #[test]
    fn test_scroll_down() {
        let mut sv = ScrollView::new(3, 5, 80);
        for i in 0..20 {
            sv.push_line(&format!("line {}", i));
        }
        sv.enter_browse();
        sv.scroll_up(15); // go to top
        assert_eq!(sv.scroll_offset, 0);

        sv.scroll_down(3);
        assert_eq!(sv.scroll_offset, 3);

        sv.scroll_down(100); // should clamp to max
        assert_eq!(sv.scroll_offset, 15); // 20 - 5
    }

    #[test]
    fn test_scroll_in_compact_is_noop() {
        let mut sv = ScrollView::new(3, 10, 80);
        sv.push_line("hello");
        let seq = sv.scroll_up(1);
        assert!(seq.is_empty());
        let seq = sv.scroll_down(1);
        assert!(seq.is_empty());
    }

    #[test]
    fn test_clear_resets_everything() {
        let mut sv = ScrollView::new(3, 10, 80);
        sv.push_line("a");
        sv.push_line("b");
        sv.enter_browse();
        sv.clear();
        assert_eq!(sv.mode(), ViewMode::Compact);
        assert_eq!(sv.line_count(), 0);
        assert_eq!(sv.rendered_lines, 0);
        assert_eq!(sv.scroll_offset, 0);
    }

    #[test]
    fn test_push_in_expanded_no_output() {
        let mut sv = ScrollView::new(3, 10, 80);
        sv.push_line("initial");
        sv.enter_browse();
        let seq = sv.push_line("new line");
        assert!(seq.is_empty(), "push in expanded mode should not produce output");
        assert_eq!(sv.line_count(), 2);
    }

    #[test]
    fn test_scrollbar_present_when_content_exceeds_viewport() {
        let mut sv = ScrollView::new(3, 5, 80);
        for i in 0..20 {
            sv.push_line(&format!("line {}", i));
        }
        let seq = sv.enter_browse();
        // Should contain scrollbar characters
        assert!(seq.contains('\u{2590}') || seq.contains('\u{2502}'),
            "expanded view should have scrollbar");
    }

    #[test]
    fn test_no_scrollbar_when_fits() {
        let mut sv = ScrollView::new(3, 10, 80);
        sv.push_line("a");
        sv.push_line("b");
        let seq = sv.enter_browse();
        // Only 2 lines, viewport is 10 — no scrollbar needed
        assert!(!seq.contains('\u{2591}'), "no track when content fits viewport");
    }

    #[test]
    fn test_scrollbar_position() {
        // 3 positions: top, middle, bottom
        let bar_top = ScrollView::compute_scrollbar(30, 10, 0);
        let bar_bottom = ScrollView::compute_scrollbar(30, 10, 20);

        // At top, thumb should start at position 0
        assert_eq!(bar_top[0], THUMB);

        // At bottom, thumb should end at last position
        assert_eq!(bar_bottom[9], THUMB);
    }

    #[test]
    fn test_expanded_rendered_lines() {
        let mut sv = ScrollView::new(3, 5, 80);
        for i in 0..20 {
            sv.push_line(&format!("line {}", i));
        }
        sv.enter_browse();
        // 5 viewport lines + 1 hint line = 6
        assert_eq!(sv.rendered_lines, 6);
    }

    #[test]
    fn test_few_lines_compact() {
        let mut sv = ScrollView::new(5, 10, 80);
        let seq = sv.push_line("only one");
        let plain = strip_ansi(&seq);
        assert!(plain.contains("only one"));
        assert_eq!(sv.rendered_lines, 1);
    }

    #[test]
    fn test_empty_clear() {
        let mut sv = ScrollView::new(3, 10, 80);
        let seq = sv.clear();
        assert!(seq.is_empty());
    }

    // -----------------------------------------------------------------------
    // vt100 terminal emulation tests
    // -----------------------------------------------------------------------

    fn parse_ansi(input: &str, cols: u16, rows: u16) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(input.as_bytes());
        parser
    }

    #[test]
    fn vt100_compact_tail_view() {
        let cols = 40u16;
        let mut sv = ScrollView::new(3, 10, cols as usize);
        let mut out = String::new();
        for i in 0..10 {
            out.push_str(&sv.push_line(&format!("line {}", i)));
        }

        let parser = parse_ansi(&out, cols, 20);
        let all = parser.screen().contents();
        // Only last 3 lines should be visible
        assert!(all.contains("line 9"));
        assert!(all.contains("line 8"));
        assert!(all.contains("line 7"));
        assert!(!all.contains("line 6"), "line 6 should be hidden: {all}");
    }

    #[test]
    fn vt100_clear_erases_all() {
        let cols = 40u16;
        let mut sv = ScrollView::new(3, 10, cols as usize);
        let mut out = String::new();
        out.push_str(&sv.push_line("hello"));
        out.push_str(&sv.push_line("world"));
        out.push_str(&sv.clear());

        let parser = parse_ansi(&out, cols, 10);
        let all = parser.screen().contents();
        assert!(!all.contains("hello"), "should be erased");
        assert!(!all.contains("world"), "should be erased");
    }

    #[test]
    fn vt100_expand_and_shrink() {
        let cols = 40u16;
        let mut sv = ScrollView::new(2, 5, cols as usize);
        let mut out = String::new();
        for i in 0..10 {
            out.push_str(&sv.push_line(&format!("line {}", i)));
        }
        // Enter browse
        out.push_str(&sv.enter_browse());
        let parser = parse_ansi(&out, cols, 20);
        let all = parser.screen().contents();
        assert!(all.contains("scroll"), "should show hint in expanded mode");

        // Exit browse
        out.push_str(&sv.exit_browse());
        let parser = parse_ansi(&out, cols, 20);
        let all = parser.screen().contents();
        assert!(!all.contains("scroll"), "hint should be gone after exit");
        assert!(all.contains("line 9"), "should show tail in compact");
    }
}
