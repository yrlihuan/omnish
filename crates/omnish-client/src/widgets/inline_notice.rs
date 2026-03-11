/// A reusable widget that inserts a notification line above the current cursor position.
///
/// Uses the Insert Line ANSI escape (`\x1b[1L`) to push the current line down
/// and render a dim message above it. The message stays in the terminal scrollback
/// and naturally scrolls away as new output appears.
pub struct InlineNotice;

impl InlineNotice {
    /// Generate the ANSI escape sequence that inserts a dim notice line above
    /// the current cursor position, then returns the cursor to its original line.
    ///
    /// The sequence:
    /// 1. Save cursor position
    /// 2. Move up one line
    /// 3. Insert a blank line (pushes content down)
    /// 4. Write the dim message
    /// 5. Restore cursor position
    pub fn render(message: &str) -> String {
        format!(
            "\x1b[s\x1b[1A\x1b[1L\r\x1b[2m{}\x1b[0m\x1b[u",
            message
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_contains_message() {
        let output = InlineNotice::render("[omnish] reconnected");
        assert!(output.contains("[omnish] reconnected"));
    }

    #[test]
    fn test_render_has_dim_formatting() {
        let output = InlineNotice::render("test");
        // \x1b[2m = dim, \x1b[0m = reset
        assert!(output.contains("\x1b[2m"));
        assert!(output.contains("\x1b[0m"));
    }

    #[test]
    fn test_render_has_insert_line() {
        let output = InlineNotice::render("test");
        // \x1b[1L = insert one line
        assert!(output.contains("\x1b[1L"));
    }

    #[test]
    fn test_render_saves_and_restores_cursor() {
        let output = InlineNotice::render("test");
        assert!(output.contains("\x1b[s")); // save
        assert!(output.contains("\x1b[u")); // restore
    }
}
