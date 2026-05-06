use std::sync::{Arc, Mutex};

/// In-memory terminal emulator that mirrors what the shell writes to the user's
/// terminal. Bytes are fed in as PTY output arrives, and the visible screen plus
/// recent scrollback can be queried at any time, similar to `tmux capture-pane`.
///
/// Backed by the `vt100` crate. Rows kept in scrollback are bounded by
/// `SCROLLBACK_ROWS`.
pub struct ScreenCapture {
    parser: vt100::Parser,
    rows: u16,
    cols: u16,
}

const SCROLLBACK_ROWS: usize = 5_000;

impl ScreenCapture {
    pub fn new(rows: u16, cols: u16) -> Self {
        let (rows, cols) = clamp_size(rows, cols);
        Self {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_ROWS),
            rows,
            cols,
        }
    }

    /// Resize the emulator. No-op if size is unchanged.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = clamp_size(rows, cols);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.parser.screen_mut().set_size(rows, cols);
        self.rows = rows;
        self.cols = cols;
    }

    /// Feed PTY output bytes into the emulator.
    pub fn feed(&mut self, data: &[u8]) {
        self.parser.process(data);
    }

    /// Capture the currently visible screen as plain text. Trailing blank rows
    /// are stripped. Each row ends with `\n`.
    pub fn capture_visible(&self) -> String {
        let mut rows: Vec<String> = self
            .parser
            .screen()
            .rows(0, self.cols)
            .map(|r| r.trim_end().to_string())
            .collect();
        while rows.last().map(|s| s.is_empty()).unwrap_or(false) {
            rows.pop();
        }
        let mut out = rows.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }

    /// Capture the most recent `n` rows: scrollback rows followed by the
    /// visible screen, then keep the last `n`. Trailing blank rows are
    /// stripped. Each row ends with `\n`.
    pub fn capture_history(&mut self, n: usize) -> String {
        if n == 0 {
            return String::new();
        }
        let v = self.rows as usize;
        let prev_offset = self.parser.screen().scrollback();

        // Discover actual scrollback size by clamping a huge offset.
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let s = self.parser.screen().scrollback();

        let total = s + v;
        let take_n = n.min(total);
        let start_idx = total - take_n;

        // vt100 only exposes rows via the current scroll offset. At
        // `set_scrollback(X)`, the visible iterator yields `chain[s - X .. s - X + v)`
        // (where chain = scrollback ++ visible). Walk forward in non-overlapping
        // chunks of length `v`, skipping into the visible region once `i >= s`.
        let mut all_rows: Vec<String> = Vec::with_capacity(take_n);
        let mut i = start_idx;
        while i < total {
            let (offset_x, skip) = if i < s {
                (s - i, 0)
            } else {
                (0, i - s)
            };
            self.parser.screen_mut().set_scrollback(offset_x);
            let take = (total - i).min(v - skip);
            let mut taken = 0;
            for (k, row) in self.parser.screen().rows(0, self.cols).enumerate() {
                if k < skip {
                    continue;
                }
                if taken >= take {
                    break;
                }
                all_rows.push(row);
                taken += 1;
            }
            i += take;
        }

        // Restore prior scroll position (we always feed at offset 0 normally).
        self.parser.screen_mut().set_scrollback(prev_offset);

        // Trim trailing blanks.
        let mut tail: Vec<String> = all_rows
            .iter()
            .map(|r| r.trim_end().to_string())
            .collect();
        while tail.last().map(|s| s.is_empty()).unwrap_or(false) {
            tail.pop();
        }
        let mut out = tail.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }

    #[cfg(test)]
    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }
}

fn clamp_size(rows: u16, cols: u16) -> (u16, u16) {
    (rows.max(1), cols.max(1))
}

/// Convenience wrapper for sharing a `ScreenCapture` across the main I/O loop
/// and the chat session.
pub type SharedScreenCapture = Arc<Mutex<ScreenCapture>>;

pub fn shared(rows: u16, cols: u16) -> SharedScreenCapture {
    Arc::new(Mutex::new(ScreenCapture::new(rows, cols)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_simple_output() {
        let mut c = ScreenCapture::new(10, 20);
        c.feed(b"hello\r\nworld\r\n");
        let v = c.capture_visible();
        assert!(v.contains("hello"));
        assert!(v.contains("world"));
        // No trailing blank rows.
        assert!(!v.contains("\n\n\n"));
    }

    #[test]
    fn capture_visible_strips_trailing_blanks() {
        let mut c = ScreenCapture::new(10, 20);
        c.feed(b"line1\r\n");
        let v = c.capture_visible();
        assert_eq!(v, "line1\n");
    }

    #[test]
    fn capture_history_returns_recent_rows() {
        let mut c = ScreenCapture::new(5, 20);
        // Push 30 lines so that scrollback is populated.
        for i in 0..30 {
            c.feed(format!("line{:02}\r\n", i).as_bytes());
        }
        let h = c.capture_history(10);
        // The most recent rows must be present.
        assert!(h.contains("line29"));
        assert!(h.contains("line21"));
        // Lines older than the requested window must be excluded.
        assert!(!h.contains("line05"));
        assert!(!h.contains("line20"));
    }

    #[test]
    fn capture_history_large_window_includes_all() {
        let mut c = ScreenCapture::new(5, 20);
        for i in 0..30 {
            c.feed(format!("line{:02}\r\n", i).as_bytes());
        }
        let h = c.capture_history(1000);
        // All 30 emitted lines should be present.
        for i in 0..30 {
            assert!(h.contains(&format!("line{:02}", i)), "missing line{:02}", i);
        }
    }

    #[test]
    fn capture_history_smaller_than_visible() {
        let mut c = ScreenCapture::new(10, 20);
        for i in 0..5 {
            c.feed(format!("L{}\r\n", i).as_bytes());
        }
        let h = c.capture_history(2);
        let lines: Vec<&str> = h.lines().collect();
        assert!(lines.len() <= 2);
    }

    #[test]
    fn capture_history_zero() {
        let mut c = ScreenCapture::new(5, 20);
        c.feed(b"hi\r\n");
        assert_eq!(c.capture_history(0), "");
    }

    #[test]
    fn handles_ansi_sequences() {
        let mut c = ScreenCapture::new(5, 30);
        // Bold text + color → vt100 should strip styling for plain capture.
        c.feed(b"\x1b[1mbold\x1b[0m normal\r\n");
        let v = c.capture_visible();
        assert!(v.contains("bold normal"));
    }

    #[test]
    fn resize_changes_size() {
        let mut c = ScreenCapture::new(10, 20);
        c.resize(20, 80);
        assert_eq!(c.size(), (20, 80));
    }

    #[test]
    fn resize_noop_when_unchanged() {
        let mut c = ScreenCapture::new(10, 20);
        c.feed(b"hello\r\n");
        c.resize(10, 20);
        assert!(c.capture_visible().contains("hello"));
    }

    #[test]
    fn ignores_osc_sequences() {
        let mut c = ScreenCapture::new(5, 30);
        // OSC 133;A and 133;C should not show up in capture.
        c.feed(b"\x1b]133;A\x07prompt$ \x1b]133;B\x07cmd\r\n");
        let v = c.capture_visible();
        assert!(v.contains("prompt$"));
        assert!(v.contains("cmd"));
        assert!(!v.contains("133"));
    }

}
