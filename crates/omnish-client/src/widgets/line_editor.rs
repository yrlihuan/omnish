use unicode_width::UnicodeWidthChar;

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
}
