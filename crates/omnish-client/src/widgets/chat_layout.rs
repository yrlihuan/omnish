#[allow(dead_code)]
pub struct Region {
    pub(crate) id: &'static str,
    pub(crate) height: usize,
    pub(crate) content: Vec<String>,
}

#[allow(dead_code)]
pub struct ChatLayout {
    pub(crate) regions: Vec<Region>,
    cols: usize,
}

#[allow(dead_code)]
impl ChatLayout {
    pub fn new(cols: usize) -> Self {
        Self { regions: Vec::new(), cols }
    }

    #[allow(dead_code)]
    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn push_region(&mut self, id: &'static str) {
        self.regions.push(Region {
            id,
            height: 0,
            content: Vec::new(),
        });
    }

    pub fn total_height(&self) -> usize {
        self.regions.iter().map(|r| r.height).sum()
    }

    fn find_region_idx(&self, id: &str) -> usize {
        self.regions.iter().position(|r| r.id == id)
            .unwrap_or_else(|| panic!("region not found: {}", id))
    }

    pub fn region_offset(&self, id: &str) -> usize {
        let mut offset = 0;
        for r in &self.regions {
            if r.id == id {
                return offset;
            }
            offset += r.height;
        }
        panic!("region not found: {}", id);
    }

    /// Redraw all regions top-to-bottom.
    /// Assumes cursor is at the layout origin.
    pub fn redraw_all(&self) -> String {
        let mut out = String::new();
        let mut first = true;
        for region in &self.regions {
            for line in &region.content {
                if first {
                    out.push_str(&format!("\r\x1b[K{}", line));
                    first = false;
                } else {
                    out.push_str(&format!("\r\n\x1b[K{}", line));
                }
            }
        }
        out
    }

    /// Update region content without producing ANSI output.
    /// Use before redraw_all() when rebuilding layout state.
    pub fn set_content(&mut self, id: &str, lines: Vec<String>) {
        let idx = self.find_region_idx(id);
        self.regions[idx].height = lines.len();
        self.regions[idx].content = lines;
    }

    /// Update region content. Returns ANSI sequence to write to terminal.
    /// Cursor convention: cursor starts and ends at the row after the last
    /// line of the layout (row = total_height).
    pub fn update(&mut self, id: &str, lines: Vec<String>) -> String {
        let idx = self.find_region_idx(id);
        let old_height = self.regions[idx].height;
        let new_height = lines.len();
        let offset = self.region_offset(id);
        let old_total = self.total_height();

        self.regions[idx].content = lines;
        self.regions[idx].height = new_height;

        let mut out = String::new();

        if old_total == 0 && new_height == 0 {
            return out;
        }

        // Move cursor from bottom (row old_total) to region start (row offset)
        let up = old_total.saturating_sub(offset);
        if up > 0 {
            out.push_str(&format!("\x1b[{}A", up));
        }
        out.push('\r');

        if old_height == new_height {
            // Same height: overwrite region lines, move back to bottom
            for line in &self.regions[idx].content {
                out.push_str(&format!("\x1b[K{}\r\n", line));
            }
            let below: usize = self.regions[idx + 1..].iter().map(|r| r.height).sum();
            if below > 0 {
                out.push_str(&format!("\x1b[{}B", below));
            }
        } else {
            // Height changed: redraw this region + all below, clear leftover
            for i in idx..self.regions.len() {
                for line in &self.regions[i].content {
                    out.push_str(&format!("\x1b[K{}\r\n", line));
                }
            }
            let new_total = self.total_height();
            if old_total > new_total {
                for _ in 0..(old_total - new_total) {
                    out.push_str("\x1b[K\r\n");
                }
                // Move back up to new bottom
                let extra = old_total - new_total;
                if extra > 0 {
                    out.push_str(&format!("\x1b[{}A", extra));
                }
            }
        }

        out
    }

    /// Hide a region (height becomes 0). Returns ANSI to update terminal.
    pub fn hide(&mut self, id: &str) -> String {
        self.update(id, Vec::new())
    }

    /// Position cursor at the last row of a region.
    /// Cursor moves from bottom of layout (row = total_height) to
    /// the last row of the target region (row = offset + height - 1).
    /// If region is empty (height 0), positions at offset row.
    pub fn cursor_to(&self, id: &str) -> String {
        let idx = self.find_region_idx(id);
        let offset = self.region_offset(id);
        let height = self.regions[idx].height;
        let total = self.total_height();
        let target = if height > 0 { offset + height - 1 } else { offset };
        let up = total.saturating_sub(target);
        if up > 0 {
            format!("\x1b[{}A\r", up)
        } else {
            "\r".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ansi(s: &str) -> vt100::Parser {
        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(s.as_bytes());
        p
    }

    // ── Task 1: struct + redraw_all ──

    #[test]
    fn test_empty_layout() {
        let layout = ChatLayout::new(80);
        assert_eq!(layout.total_height(), 0);
        assert_eq!(layout.redraw_all(), "");
    }

    #[test]
    fn test_push_regions() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("a");
        layout.push_region("b");
        assert_eq!(layout.total_height(), 0);
    }

    #[test]
    fn test_redraw_all_with_content() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("a");
        layout.push_region("b");
        layout.regions[0].content = vec!["line 1".into(), "line 2".into()];
        layout.regions[0].height = 2;
        layout.regions[1].content = vec!["line 3".into()];
        layout.regions[1].height = 1;

        let output = layout.redraw_all();
        let p = parse_ansi(&output);
        let screen = p.screen().contents();
        assert!(screen.contains("line 1"));
        assert!(screen.contains("line 2"));
        assert!(screen.contains("line 3"));
    }

    // ── Task 2: region_offset + update ──

    #[test]
    fn test_region_offset() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("a");
        layout.push_region("b");
        layout.push_region("c");
        layout.regions[0].height = 3;
        layout.regions[1].height = 2;
        layout.regions[2].height = 1;
        layout.regions[0].content = vec!["a1".into(), "a2".into(), "a3".into()];
        layout.regions[1].content = vec!["b1".into(), "b2".into()];
        layout.regions[2].content = vec!["c1".into()];

        assert_eq!(layout.region_offset("a"), 0);
        assert_eq!(layout.region_offset("b"), 3);
        assert_eq!(layout.region_offset("c"), 5);
    }

    #[test]
    fn test_update_same_height() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("top");
        layout.push_region("bottom");

        layout.update("top", vec!["hello".into()]);
        layout.update("bottom", vec!["world".into()]);

        layout.update("top", vec!["HELLO".into()]);

        let all = layout.redraw_all();
        let p = parse_ansi(&all);
        let screen = p.screen().contents();
        assert!(screen.contains("HELLO"));
        assert!(screen.contains("world"));
        assert!(!screen.contains("hello"));
    }

    // ── Task 3: hide + cursor_to + height changes ──

    #[test]
    fn test_update_height_increase() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("top");
        layout.push_region("bottom");
        layout.update("top", vec!["t1".into()]);
        layout.update("bottom", vec!["b1".into()]);

        layout.update("top", vec!["t1".into(), "t2".into(), "t3".into()]);

        let all = layout.redraw_all();
        let p = parse_ansi(&all);
        let screen = p.screen().contents();
        assert!(screen.contains("t1"));
        assert!(screen.contains("t2"));
        assert!(screen.contains("t3"));
        assert!(screen.contains("b1"));
        assert_eq!(layout.total_height(), 4);
        assert_eq!(layout.region_offset("bottom"), 3);
    }

    #[test]
    fn test_update_height_decrease() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("top");
        layout.push_region("bottom");
        layout.update("top", vec!["t1".into(), "t2".into(), "t3".into()]);
        layout.update("bottom", vec!["b1".into()]);

        layout.update("top", vec!["t1".into()]);

        assert_eq!(layout.total_height(), 2);
        assert_eq!(layout.region_offset("bottom"), 1);
    }

    #[test]
    fn test_hide() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("a");
        layout.push_region("b");
        layout.update("a", vec!["visible".into()]);
        layout.update("b", vec!["below".into()]);

        layout.hide("a");
        assert_eq!(layout.total_height(), 1);
        assert_eq!(layout.region_offset("b"), 0);

        let all = layout.redraw_all();
        let p = parse_ansi(&all);
        let screen = p.screen().contents();
        assert!(!screen.contains("visible"));
        assert!(screen.contains("below"));
    }

    #[test]
    fn test_hide_then_update_reshows() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("a");
        layout.push_region("b");
        layout.update("a", vec!["first".into()]);
        layout.update("b", vec!["below".into()]);
        layout.hide("a");

        layout.update("a", vec!["second".into()]);
        assert_eq!(layout.total_height(), 2);

        let all = layout.redraw_all();
        let p = parse_ansi(&all);
        let screen = p.screen().contents();
        assert!(screen.contains("second"));
        assert!(screen.contains("below"));
    }

    #[test]
    fn test_cursor_to() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("top");
        layout.push_region("editor");
        layout.push_region("status");
        layout.update("top", vec!["t1".into(), "t2".into()]);
        layout.update("editor", vec!["> input".into()]);
        layout.update("status", vec!["thinking...".into()]);

        let seq = layout.cursor_to("editor");
        // Editor is at offset 2, height 1, so last row = 2
        // total = 4, so move up 4 - 2 = 2 lines
        assert!(seq.contains("\x1b[2A"));
    }

    // ── Task 4: vt100 integration tests ──

    #[test]
    fn test_vt100_update_sequence() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("sv");
        layout.push_region("ed");
        layout.push_region("st");

        let mut p = vt100::Parser::new(24, 80, 0);

        let s1 = layout.update("sv", vec!["Response line 1".into(), "Response line 2".into()]);
        p.process(s1.as_bytes());
        let s2 = layout.update("ed", vec!["> ".into()]);
        p.process(s2.as_bytes());

        let screen = p.screen().contents();
        assert!(screen.contains("Response line 1"));
        assert!(screen.contains("Response line 2"));
        assert!(screen.contains("> "));

        let s3 = layout.update("st", vec!["(thinking...)".into()]);
        p.process(s3.as_bytes());
        let screen = p.screen().contents();
        assert!(screen.contains("(thinking...)"));

        let s4 = layout.hide("st");
        p.process(s4.as_bytes());
        let screen = p.screen().contents();
        assert!(!screen.contains("(thinking...)"));
        assert!(screen.contains("> "));
    }

    #[test]
    fn test_vt100_scroll_view_grows() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("sv");
        layout.push_region("ed");

        let mut p = vt100::Parser::new(24, 80, 0);

        let s1 = layout.update("sv", vec!["line 1".into()]);
        p.process(s1.as_bytes());
        let s2 = layout.update("ed", vec!["> hello".into()]);
        p.process(s2.as_bytes());

        let s3 = layout.update("sv", vec![
            "line 1".into(), "line 2".into(), "line 3".into(),
        ]);
        p.process(s3.as_bytes());

        let screen = p.screen().contents();
        assert!(screen.contains("line 1"));
        assert!(screen.contains("line 2"));
        assert!(screen.contains("line 3"));
        assert!(screen.contains("> hello"));
    }

    #[test]
    fn test_vt100_update_last_region() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("a");
        layout.push_region("b");

        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(layout.update("a", vec!["first".into()]).as_bytes());
        p.process(layout.update("b", vec!["second".into()]).as_bytes());

        p.process(layout.update("b", vec!["UPDATED".into()]).as_bytes());

        let screen = p.screen().contents();
        assert!(screen.contains("first"));
        assert!(screen.contains("UPDATED"));
        assert!(!screen.contains("second"));
    }

    /// Issue #278: editor redraws use relative cursor movement (term_cursor_row)
    /// instead of layout.update(), so cursor_to() + typing works correctly.
    /// This test simulates the relative movement approach used in main.rs.
    #[test]
    fn test_vt100_relative_editor_redraw() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("sv");
        layout.push_region("ed");

        let mut p = vt100::Parser::new(24, 80, 0);

        // Initial state: scroll_view has content, editor prompt via layout.update
        p.process(layout.update("sv", vec![
            "line 1".into(), "line 2".into(), "line 3".into(),
        ]).as_bytes());
        p.process(layout.update("ed", vec!["> ".into()]).as_bytes());

        // cursor_to positions cursor at editor row for typing
        let ct = layout.cursor_to("ed");
        p.process(ct.as_bytes());
        p.process(b"\x1b[2C"); // move past "> "

        // Simulate typing "h" using relative movement (like the redraw closure):
        // term_cursor_row=0 → no up-movement needed, \r to column 0,
        // write content, \x1b[J to clear, position cursor
        let mut out = String::new();
        out.push('\r');            // to column 0
        out.push_str("> h\x1b[J"); // write + clear to end of screen
        out.push('\r');            // back to column 0
        out.push_str("\x1b[3C");   // cursor after "> h"
        p.process(out.as_bytes());
        layout.set_content("ed", vec!["> h".into()]);

        let screen = p.screen().contents();
        assert!(screen.contains("line 1"), "line 1 missing\n{}", screen);
        assert!(screen.contains("line 2"), "line 2 missing\n{}", screen);
        assert!(screen.contains("line 3"), "line 3 missing\n{}", screen);
        assert!(screen.contains("> h"), "editor content missing\n{}", screen);

        // No ghost lines
        let prompt_count = screen.lines().filter(|l| l.trim_end().starts_with(">")).count();
        assert_eq!(prompt_count, 1, "expected 1 editor line, got {}\n{}", prompt_count, screen);
    }

    /// Issue #283: When editor content wraps past terminal width, the redraw
    /// must account for visual (wrapped) rows in cursor movement.
    ///
    /// Bug: term_cursor_row stored the logical editor row (always 0 for single-line),
    /// so on the next redraw `\r` started from a wrapped row instead of the top.
    /// `\x1b[nC` (CUF) also can't wrap past the right margin, so cursor_display >= cols
    /// positioned the cursor incorrectly.
    ///
    /// Fix: use display_width / cols for visual cursor row, display_width % cols for column.
    #[test]
    fn test_vt100_editor_wrap_redraw_283() {
        let cols: usize = 20;
        let mut layout = ChatLayout::new(cols);
        layout.push_region("sv");
        layout.push_region("ed");

        let mut p = vt100::Parser::new(24, cols as u16, 0);

        // Setup: scroll_view + initial editor prompt
        p.process(layout.update("sv", vec!["hello".into()]).as_bytes());
        p.process(layout.update("ed", vec!["> ".into()]).as_bytes());
        let ct = layout.cursor_to("ed");
        p.process(ct.as_bytes());
        p.process(b"\x1b[2C");

        // Helper: simulate a correctly-wrapping redraw.
        // Returns the new term_cursor_row (visual row of cursor).
        let simulate_redraw = |p: &mut vt100::Parser,
                                layout: &mut ChatLayout,
                                content: &str,
                                display_width: usize,
                                cursor_display: usize,
                                term_cursor_row: usize| -> usize {
            let mut out = String::new();
            // Move up to editor visual row 0
            if term_cursor_row > 0 {
                out.push_str(&format!("\x1b[{}A", term_cursor_row));
            }
            out.push('\r');
            // Write content (terminal wraps naturally)
            out.push_str(content);
            out.push_str("\x1b[J");
            // After writing, cursor at (display_width / cols, display_width % cols)
            let write_end_row = display_width / cols;
            let target_row = cursor_display / cols;
            let target_col = cursor_display % cols;
            let up = write_end_row.saturating_sub(target_row);
            if up > 0 {
                out.push_str(&format!("\x1b[{}A", up));
            }
            out.push('\r');
            if target_col > 0 {
                out.push_str(&format!("\x1b[{}C", target_col));
            }
            p.process(out.as_bytes());
            layout.set_content("ed", vec![content.to_string()]);
            target_row
        };

        // First redraw: "> " + 24 chars = 26 display chars, wraps on 20-col terminal
        // Visual row 0: "> abcdefghijklmnopqr"
        // Visual row 1: "stuvwx"
        let tcr = simulate_redraw(
            &mut p, &mut layout,
            "> abcdefghijklmnopqrstuvwx", 26, 26, 0,
        );
        assert_eq!(tcr, 1, "cursor should be on visual row 1");

        // Second redraw: type one more char 'y'
        let tcr = simulate_redraw(
            &mut p, &mut layout,
            "> abcdefghijklmnopqrstuvwxy", 27, 27, tcr,
        );
        assert_eq!(tcr, 1, "cursor should still be on visual row 1");

        let screen = p.screen().contents();
        // scroll_view content preserved
        assert!(screen.contains("hello"), "scroll_view missing\n{}", screen);
        // Editor prompt appears exactly once
        let prompt_count = screen.lines()
            .filter(|l| l.trim_end().starts_with(">"))
            .count();
        assert_eq!(prompt_count, 1, "expected 1 '>' line, got {}\n{}", prompt_count, screen);
        // Wrapped content present
        assert!(screen.contains("stuvwxy"), "wrapped content missing\n{}", screen);
    }

    /// Issue #283: verify exact-multiple wrapping (display_width == cols).
    /// When content exactly fills one row, cursor auto-wraps to the next row.
    #[test]
    fn test_vt100_editor_wrap_exact_boundary() {
        let cols: usize = 20;
        let mut layout = ChatLayout::new(cols);
        layout.push_region("ed");

        let mut p = vt100::Parser::new(24, cols as u16, 0);
        p.process(layout.update("ed", vec!["> ".into()]).as_bytes());
        let ct = layout.cursor_to("ed");
        p.process(ct.as_bytes());
        p.process(b"\x1b[2C");

        // "> " + 18 chars = exactly 20 display chars
        let content = "> abcdefghijklmnopqr";
        let display_width = 20;
        let cursor_display = 20;

        let mut out = String::new();
        out.push('\r');
        out.push_str(content);
        out.push_str("\x1b[J");
        // After writing 20 chars: cursor at (1, 0) — auto-wrapped
        let write_end_row = display_width / cols; // = 1
        let target_row = cursor_display / cols; // = 1
        let target_col = cursor_display % cols; // = 0
        let up = write_end_row.saturating_sub(target_row);
        if up > 0 {
            out.push_str(&format!("\x1b[{}A", up));
        }
        out.push('\r');
        if target_col > 0 {
            out.push_str(&format!("\x1b[{}C", target_col));
        }
        p.process(out.as_bytes());
        layout.set_content("ed", vec![content.to_string()]);
        let tcr = target_row; // = 1

        // Second redraw: type one more char, now wraps
        let content2 = "> abcdefghijklmnopqrs";
        let display_width2 = 21;
        let cursor_display2 = 21;

        let mut out2 = String::new();
        if tcr > 0 {
            out2.push_str(&format!("\x1b[{}A", tcr));
        }
        out2.push('\r');
        out2.push_str(content2);
        out2.push_str("\x1b[J");
        let write_end_row2 = display_width2 / cols; // = 1
        let target_row2 = cursor_display2 / cols; // = 1
        let target_col2 = cursor_display2 % cols; // = 1
        let up2 = write_end_row2.saturating_sub(target_row2);
        if up2 > 0 {
            out2.push_str(&format!("\x1b[{}A", up2));
        }
        out2.push('\r');
        if target_col2 > 0 {
            out2.push_str(&format!("\x1b[{}C", target_col2));
        }
        p.process(out2.as_bytes());

        let screen = p.screen().contents();
        let prompt_count = screen.lines()
            .filter(|l| l.trim_end().starts_with(">"))
            .count();
        assert_eq!(prompt_count, 1, "expected 1 '>' line, got {}\n{}", prompt_count, screen);
        assert!(screen.contains("s"), "wrapped char missing\n{}", screen);
    }

    #[test]
    fn test_update_with_empty_lines_hides() {
        let mut layout = ChatLayout::new(80);
        layout.push_region("a");
        layout.push_region("b");
        layout.update("a", vec!["visible".into()]);
        layout.update("b", vec!["below".into()]);

        layout.update("a", vec![]);
        assert_eq!(layout.total_height(), 1);
        assert_eq!(layout.region_offset("b"), 0);
    }
}
