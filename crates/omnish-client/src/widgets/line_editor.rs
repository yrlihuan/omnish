use unicode_width::UnicodeWidthChar;

/// Strip ANSI escape sequences from a string (for display width measurement).
fn strip_ansi_escapes(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some(next) = chars.next() {
                if next == '[' {
                    for c2 in chars.by_ref() {
                        if c2.is_ascii_alphabetic() { break; }
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub struct LineEditor {
    lines: Vec<Vec<char>>,
    pub(crate) cursor: (usize, usize), // (row, col) in char indices
}

impl LineEditor {
    pub fn new() -> Self {
        Self {
            lines: vec![vec![]],
            cursor: (0, 0),
        }
    }

    pub fn insert(&mut self, ch: char) {
        let (row, col) = self.cursor;
        self.lines[row].insert(col, ch);
        self.cursor.1 += 1;
    }

    pub fn content(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    #[allow(dead_code)]
    pub fn cursor_display_col(&self) -> usize {
        let (row, col) = self.cursor;
        self.lines[row][..col]
            .iter()
            .map(|c| UnicodeWidthChar::width(*c).unwrap_or(1))
            .sum()
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, row: usize) -> &[char] {
        &self.lines[row]
    }

    pub fn move_left(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            self.cursor.1 -= 1;
        } else if row > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = self.lines[row - 1].len();
        }
    }

    pub fn move_right(&mut self) {
        let (row, col) = self.cursor;
        if col < self.lines[row].len() {
            self.cursor.1 += 1;
        } else if row < self.lines.len() - 1 {
            self.cursor.0 += 1;
            self.cursor.1 = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            let line_len = self.lines[self.cursor.0].len();
            if self.cursor.1 > line_len {
                self.cursor.1 = line_len;
            }
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor.0 < self.lines.len() - 1 {
            self.cursor.0 += 1;
            let line_len = self.lines[self.cursor.0].len();
            if self.cursor.1 > line_len {
                self.cursor.1 = line_len;
            }
        }
    }

    pub fn move_home(&mut self) {
        self.cursor.1 = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor.1 = self.lines[self.cursor.0].len();
    }

    pub fn move_word_left(&mut self) {
        let (row, col) = self.cursor;
        let line = &self.lines[row];
        if col == 0 {
            return;
        }
        let mut i = col;
        while i > 0 && line[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !line[i - 1].is_whitespace() {
            i -= 1;
        }
        self.cursor.1 = i;
    }

    pub fn move_word_right(&mut self) {
        let (row, col) = self.cursor;
        let line = &self.lines[row];
        let len = line.len();
        if col >= len {
            return;
        }
        let mut i = col;
        while i < len && !line[i].is_whitespace() {
            i += 1;
        }
        while i < len && line[i].is_whitespace() {
            i += 1;
        }
        self.cursor.1 = i;
    }

    pub fn delete_back(&mut self) -> bool {
        let (row, col) = self.cursor;
        if col > 0 {
            self.lines[row].remove(col - 1);
            self.cursor.1 -= 1;
            true
        } else if row > 0 {
            let current_line = self.lines.remove(row);
            let prev_len = self.lines[row - 1].len();
            self.lines[row - 1].extend(current_line);
            self.cursor = (row - 1, prev_len);
            true
        } else {
            false
        }
    }

    pub fn delete_forward(&mut self) {
        let (row, col) = self.cursor;
        if col < self.lines[row].len() {
            self.lines[row].remove(col);
        } else if row < self.lines.len() - 1 {
            let next_line = self.lines.remove(row + 1);
            self.lines[row].extend(next_line);
        }
    }

    pub fn kill_to_start(&mut self) {
        let (row, col) = self.cursor;
        self.lines[row].drain(..col);
        self.cursor.1 = 0;
    }

    pub fn newline(&mut self) {
        let (row, col) = self.cursor;
        let rest = self.lines[row].split_off(col);
        self.lines.insert(row + 1, rest);
        self.cursor = (row + 1, 0);
    }

    /// Insert a paste-block placeholder (FFFC) on its own line.
    /// Handles three cases: cursor on empty line, at end of line, or mid-line.
    /// Cursor ends up on a new empty line after the placeholder.
    pub fn insert_paste_block(&mut self) {
        let (row, col) = self.cursor;
        if self.lines[row].is_empty() {
            // Replace empty line with placeholder
            self.lines[row] = vec!['\u{FFFC}'];
            self.lines.insert(row + 1, vec![]);
            self.cursor = (row + 1, 0);
        } else if col == self.lines[row].len() {
            // At end of line: add placeholder line after
            self.lines.insert(row + 1, vec!['\u{FFFC}']);
            self.lines.insert(row + 2, vec![]);
            self.cursor = (row + 2, 0);
        } else {
            // Mid-line: split at cursor, insert placeholder between
            let rest = self.lines[row].split_off(col);
            self.lines.insert(row + 1, vec!['\u{FFFC}']);
            self.lines.insert(row + 2, rest);
            self.cursor = (row + 2, 0);
        }
    }

    /// Render editor content with prefix and optional ghost text.
    /// Returns one styled line per editor row. No cursor movement sequences.
    /// Ghost text appears dim after cursor on the last line.
    pub fn render(&self, prefix: &str, ghost: &str) -> Vec<String> {
        let mut result = Vec::new();
        for (i, line_chars) in self.lines.iter().enumerate() {
            let mut s = String::new();
            if i == 0 {
                s.push_str(prefix);
            } else {
                let stripped = strip_ansi_escapes(prefix);
                let prefix_width: usize = stripped.chars()
                    .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                    .sum();
                for _ in 0..prefix_width {
                    s.push(' ');
                }
            }
            let text: String = line_chars.iter().collect();
            s.push_str(&text);

            if i == self.lines.len() - 1 && !ghost.is_empty() {
                s.push_str(&format!("\x1b[2;37m{}\x1b[0m", ghost));
            }
            result.push(s);
        }
        if result.is_empty() {
            result.push(prefix.to_string());
        }
        result
    }

    pub fn set_content(&mut self, s: &str) {
        self.lines = if s.is_empty() {
            vec![vec![]]
        } else {
            s.lines().map(|l| l.chars().collect()).collect()
        };
        if s.ends_with('\n') {
            self.lines.push(vec![]);
        }
        let last_row = self.lines.len() - 1;
        let last_col = self.lines[last_row].len();
        self.cursor = (last_row, last_col);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_editor_is_empty() {
        let ed = LineEditor::new();
        assert!(ed.is_empty());
        assert_eq!(ed.content(), "");
        assert_eq!(ed.cursor(), (0, 0));
        assert_eq!(ed.line_count(), 1);
    }

    #[test]
    fn test_insert_chars() {
        let mut ed = LineEditor::new();
        ed.insert('h');
        ed.insert('i');
        assert_eq!(ed.content(), "hi");
        assert_eq!(ed.cursor(), (0, 2));
        assert!(!ed.is_empty());
    }

    #[test]
    fn test_insert_cjk() {
        let mut ed = LineEditor::new();
        ed.insert('你');
        ed.insert('好');
        assert_eq!(ed.content(), "你好");
        assert_eq!(ed.cursor(), (0, 2));
        assert_eq!(ed.cursor_display_col(), 4);
    }

    #[test]
    fn test_set_content() {
        let mut ed = LineEditor::new();
        ed.set_content("hello\nworld");
        assert_eq!(ed.line_count(), 2);
        assert_eq!(ed.content(), "hello\nworld");
        assert_eq!(ed.cursor(), (1, 5));
    }

    #[test]
    fn test_line_accessor() {
        let mut ed = LineEditor::new();
        ed.set_content("abc");
        assert_eq!(ed.line(0), &['a', 'b', 'c']);
    }

    #[test]
    fn test_move_left_right() {
        let mut ed = LineEditor::new();
        ed.set_content("abc");
        assert_eq!(ed.cursor(), (0, 3));
        ed.move_left();
        assert_eq!(ed.cursor(), (0, 2));
        ed.move_right();
        assert_eq!(ed.cursor(), (0, 3));
        ed.move_right(); // at end, no-op
        assert_eq!(ed.cursor(), (0, 3));
    }

    #[test]
    fn test_move_left_at_start() {
        let mut ed = LineEditor::new();
        ed.move_left(); // at (0,0), no-op
        assert_eq!(ed.cursor(), (0, 0));
    }

    #[test]
    fn test_move_left_wraps_to_prev_line() {
        let mut ed = LineEditor::new();
        ed.set_content("ab\ncd");
        ed.cursor = (1, 0); // start of second line
        ed.move_left();
        assert_eq!(ed.cursor(), (0, 2)); // end of first line
    }

    #[test]
    fn test_move_right_wraps_to_next_line() {
        let mut ed = LineEditor::new();
        ed.set_content("ab\ncd");
        ed.cursor = (0, 2); // end of first line
        ed.move_right();
        assert_eq!(ed.cursor(), (1, 0)); // start of second line
    }

    #[test]
    fn test_move_up_down() {
        let mut ed = LineEditor::new();
        ed.set_content("hello\nhi");
        ed.cursor = (1, 2); // end of "hi"
        ed.move_up();
        assert_eq!(ed.cursor(), (0, 2)); // col clamped to same position
        ed.move_down();
        assert_eq!(ed.cursor(), (1, 2));
    }

    #[test]
    fn test_move_up_clamps_col() {
        let mut ed = LineEditor::new();
        ed.set_content("hi\nhello");
        // cursor at (1, 5) — end of "hello"
        ed.move_up();
        assert_eq!(ed.cursor(), (0, 2)); // "hi" only has 2 chars
    }

    #[test]
    fn test_move_home_end() {
        let mut ed = LineEditor::new();
        ed.set_content("hello");
        assert_eq!(ed.cursor(), (0, 5));
        ed.move_home();
        assert_eq!(ed.cursor(), (0, 0));
        ed.move_end();
        assert_eq!(ed.cursor(), (0, 5));
    }

    #[test]
    fn test_move_word_left() {
        let mut ed = LineEditor::new();
        ed.set_content("hello world");
        ed.move_word_left();
        assert_eq!(ed.cursor(), (0, 6)); // before "world"
        ed.move_word_left();
        assert_eq!(ed.cursor(), (0, 0)); // before "hello"
    }

    #[test]
    fn test_move_word_right() {
        let mut ed = LineEditor::new();
        ed.set_content("hello world");
        ed.cursor = (0, 0);
        ed.move_word_right();
        assert_eq!(ed.cursor(), (0, 6)); // after "hello "
        ed.move_word_right();
        assert_eq!(ed.cursor(), (0, 11)); // after "world"
    }

    #[test]
    fn test_delete_back() {
        let mut ed = LineEditor::new();
        ed.set_content("abc");
        ed.delete_back();
        assert_eq!(ed.content(), "ab");
        assert_eq!(ed.cursor(), (0, 2));
    }

    #[test]
    fn test_delete_back_at_start_returns_false() {
        let mut ed = LineEditor::new();
        assert!(!ed.delete_back());
    }

    #[test]
    fn test_delete_back_merges_lines() {
        let mut ed = LineEditor::new();
        ed.set_content("ab\ncd");
        ed.cursor = (1, 0);
        ed.delete_back();
        assert_eq!(ed.content(), "abcd");
        assert_eq!(ed.cursor(), (0, 2));
        assert_eq!(ed.line_count(), 1);
    }

    #[test]
    fn test_delete_forward() {
        let mut ed = LineEditor::new();
        ed.set_content("abc");
        ed.cursor = (0, 1);
        ed.delete_forward();
        assert_eq!(ed.content(), "ac");
        assert_eq!(ed.cursor(), (0, 1));
    }

    #[test]
    fn test_delete_forward_merges_lines() {
        let mut ed = LineEditor::new();
        ed.set_content("ab\ncd");
        ed.cursor = (0, 2);
        ed.delete_forward();
        assert_eq!(ed.content(), "abcd");
        assert_eq!(ed.line_count(), 1);
    }

    #[test]
    fn test_kill_to_start() {
        let mut ed = LineEditor::new();
        ed.set_content("hello world");
        ed.cursor = (0, 5);
        ed.kill_to_start();
        assert_eq!(ed.content(), " world");
        assert_eq!(ed.cursor(), (0, 0));
    }

    #[test]
    fn test_newline() {
        let mut ed = LineEditor::new();
        ed.set_content("abcd");
        ed.cursor = (0, 2);
        ed.newline();
        assert_eq!(ed.content(), "ab\ncd");
        assert_eq!(ed.cursor(), (1, 0));
        assert_eq!(ed.line_count(), 2);
    }

    #[test]
    fn test_insert_mid_line() {
        let mut ed = LineEditor::new();
        ed.set_content("ac");
        ed.cursor = (0, 1);
        ed.insert('b');
        assert_eq!(ed.content(), "abc");
        assert_eq!(ed.cursor(), (0, 2));
    }

    #[test]
    fn test_insert_paste_block_empty_line() {
        let mut ed = LineEditor::new();
        // Empty editor: placeholder replaces empty line
        ed.insert_paste_block();
        assert_eq!(ed.line_count(), 2);
        assert_eq!(ed.line(0), &['\u{FFFC}']);
        assert_eq!(ed.line(1), &[] as &[char]);
        assert_eq!(ed.cursor(), (1, 0));
    }

    #[test]
    fn test_insert_paste_block_end_of_line() {
        let mut ed = LineEditor::new();
        ed.set_content("hello");
        // Cursor at end of "hello"
        ed.insert_paste_block();
        assert_eq!(ed.line_count(), 3);
        assert_eq!(ed.line(0), &['h', 'e', 'l', 'l', 'o']);
        assert_eq!(ed.line(1), &['\u{FFFC}']);
        assert_eq!(ed.line(2), &[] as &[char]);
        assert_eq!(ed.cursor(), (2, 0));
    }

    #[test]
    fn test_insert_paste_block_mid_line() {
        let mut ed = LineEditor::new();
        ed.set_content("hello");
        ed.cursor = (0, 2); // between "he" and "llo"
        ed.insert_paste_block();
        assert_eq!(ed.line_count(), 3);
        assert_eq!(ed.line(0), &['h', 'e']);
        assert_eq!(ed.line(1), &['\u{FFFC}']);
        assert_eq!(ed.line(2), &['l', 'l', 'o']);
        assert_eq!(ed.cursor(), (2, 0));
    }

    #[test]
    fn test_render_single_line() {
        let mut editor = LineEditor::new();
        editor.insert('h');
        editor.insert('i');

        let lines = editor.render("\x1b[36m> \x1b[0m", "");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("\x1b[36m> \x1b[0m"));
        assert!(lines[0].contains("hi"));
    }

    #[test]
    fn test_render_with_ghost() {
        let mut editor = LineEditor::new();
        editor.insert('h');

        let lines = editor.render("> ", "ello");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("h"));
        assert!(lines[0].contains("\x1b[2;37mello\x1b[0m"));
    }

    #[test]
    fn test_render_empty() {
        let editor = LineEditor::new();
        let lines = editor.render("> ", "");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "> ");
    }

    #[test]
    fn test_render_multiline() {
        let mut editor = LineEditor::new();
        editor.insert('a');
        editor.newline();
        editor.insert('b');

        let lines = editor.render("> ", "");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("> "));
        assert!(lines[0].contains("a"));
        assert!(lines[1].starts_with("  ")); // continuation indent
        assert!(lines[1].contains("b"));
    }

    #[test]
    fn test_delete_paste_block_two_step() {
        // Simulate: paste block after "hello", then two backspaces to delete
        let mut ed = LineEditor::new();
        ed.set_content("hello");
        ed.insert_paste_block();
        // State: ["hello", FFFC, ""], cursor (2, 0)
        assert_eq!(ed.cursor(), (2, 0));

        // Step 1: backspace merges empty line into FFFC line
        ed.delete_back();
        assert_eq!(ed.cursor(), (1, 1)); // after FFFC
        assert_eq!(ed.line(1), &['\u{FFFC}']);

        // Step 2: backspace deletes FFFC
        ed.delete_back();
        assert_eq!(ed.cursor(), (1, 0)); // FFFC gone, line now empty
        assert!(ed.line(1).is_empty());

        // Cleanup: merge empty line with previous
        ed.delete_back();
        assert_eq!(ed.line_count(), 1);
        assert_eq!(ed.content(), "hello");
        assert_eq!(ed.cursor(), (0, 5));
    }
}
