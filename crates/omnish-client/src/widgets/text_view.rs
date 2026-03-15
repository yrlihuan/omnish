/// Trivial widget that stores pre-styled lines for display.
#[allow(dead_code)]
pub struct TextView {
    content: Vec<String>,
}

impl TextView {
    #[allow(dead_code)]
    pub fn new(lines: Vec<String>) -> Self {
        Self { content: lines }
    }

    #[allow(dead_code)]
    pub fn lines(&self) -> &[String] {
        &self.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_view_stores_lines() {
        let tv = TextView::new(vec!["hello".into(), "world".into()]);
        assert_eq!(tv.lines(), &["hello", "world"]);
    }

    #[test]
    fn test_text_view_empty() {
        let tv = TextView::new(vec![]);
        assert!(tv.lines().is_empty());
    }
}
