// crates/omnish-client/src/markdown.rs
//
// Render markdown text to ANSI-escaped terminal output.
// Uses pulldown-cmark for parsing, produces raw-mode compatible output (\r\n).

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

// ANSI style codes — shared constants from display, plus markdown-only styles
const RESET: &str = crate::display::RESET;
const BOLD: &str = crate::display::BOLD;
const DIM: &str = crate::display::DIM;
const CYAN: &str = crate::display::CYAN;
const YELLOW: &str = crate::display::YELLOW;
const GREEN: &str = crate::display::GREEN;
const ITALIC: &str = "\x1b[3m";
const UNDERLINE: &str = "\x1b[4m";
const CODE_BG: &str = "\x1b[40m\x1b[48;5;236m"; // dark gray background
const HEADING_COLOR: &str = "\x1b[1;36m"; // bold cyan

/// Render markdown content to ANSI terminal output.
/// Output uses \r\n line endings for raw-mode terminals.
pub fn render(content: &str) -> String {
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let parser = Parser::new_ext(content, options);

    let mut out = String::new();
    let mut in_code_block = false;
    let mut list_depth: usize = 0;
    let mut ordered_index: Vec<u64> = Vec::new();
    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { .. } => {
                    out.push_str(HEADING_COLOR);
                }
                Tag::Paragraph if !out.is_empty() && list_depth == 0
                    && !out.ends_with("\r\n") => {
                    out.push_str("\r\n");
                }
                Tag::Paragraph => {}
                Tag::BlockQuote(_) => {
                    out.push_str(DIM);
                    out.push_str(GREEN);
                }
                Tag::CodeBlock(_) => {
                    in_code_block = true;
                    out.push_str("\r\n");
                    out.push_str(CODE_BG);
                    out.push_str(YELLOW);
                }
                Tag::List(start) => {
                    list_depth += 1;
                    if let Some(n) = start {
                        ordered_index.push(n);
                    } else {
                        ordered_index.push(0); // 0 = unordered
                    }
                }
                Tag::Item => {
                    let indent = "  ".repeat(list_depth.saturating_sub(1));
                    if let Some(&idx) = ordered_index.last() {
                        if idx > 0 {
                            out.push_str(&format!("{}{}{}.{} ", indent, DIM, idx, RESET));
                            // Increment for next item
                            if let Some(last) = ordered_index.last_mut() {
                                *last += 1;
                            }
                        } else {
                            out.push_str(&format!("{}{}\u{2022}{} ", indent, DIM, RESET));
                        }
                    }
                }
                Tag::Emphasis => {
                    out.push_str(ITALIC);
                }
                Tag::Strong => {
                    out.push_str(BOLD);
                }
                Tag::Strikethrough => {
                    out.push_str("\x1b[9m"); // strikethrough
                }
                Tag::Link { dest_url, .. } => {
                    out.push_str(UNDERLINE);
                    out.push_str(CYAN);
                    // Store URL to display after text if different
                    let _ = dest_url; // consumed in TagEnd
                }
                Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {
                    // Basic table support — just separate cells with |
                    if matches!(tag, Tag::TableCell) {
                        out.push_str("| ");
                    }
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    out.push_str(RESET);
                    out.push_str("\r\n");
                }
                TagEnd::Paragraph => {
                    out.push_str("\r\n");
                }
                TagEnd::BlockQuote(_) => {
                    out.push_str(RESET);
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    out.push_str(RESET);
                    out.push_str("\r\n");
                }
                TagEnd::List(_) => {
                    list_depth = list_depth.saturating_sub(1);
                    ordered_index.pop();
                }
                TagEnd::Item => {
                    out.push_str("\r\n");
                }
                TagEnd::Emphasis => {
                    out.push_str(RESET);
                }
                TagEnd::Strong => {
                    out.push_str(RESET);
                }
                TagEnd::Strikethrough => {
                    out.push_str(RESET);
                }
                TagEnd::Link => {
                    out.push_str(RESET);
                }
                TagEnd::TableHead => {
                    out.push_str("|\r\n");
                }
                TagEnd::TableRow => {
                    out.push_str("|\r\n");
                }
                TagEnd::TableCell => {
                    out.push(' ');
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_code_block {
                    // Preserve code block formatting, convert \n to \r\n
                    for line in text.split('\n') {
                        out.push_str(line);
                        out.push_str("\r\n");
                    }
                    // Remove trailing \r\n added by the loop
                    if out.ends_with("\r\n") {
                        out.truncate(out.len() - 2);
                    }
                } else {
                    out.push_str(&text);
                }
            }
            Event::Code(code) => {
                // Inline code
                out.push_str(CODE_BG);
                out.push_str(YELLOW);
                out.push(' ');
                out.push_str(&code);
                out.push(' ');
                out.push_str(RESET);
            }
            Event::SoftBreak => {
                out.push_str("\r\n");
            }
            Event::HardBreak => {
                out.push_str("\r\n");
            }
            Event::Rule => {
                out.push_str(&format!("{}───────────{}\r\n", DIM, RESET));
            }
            _ => {}
        }
    }

    // Trim trailing empty lines
    while out.ends_with("\r\n\r\n") {
        out.truncate(out.len() - 2);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip ANSI escape sequences for easier content assertions.
    fn strip_ansi(s: &str) -> String {
        let re = regex_lite::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
        re.replace_all(s, "").to_string()
    }

    #[test]
    fn test_plain_text() {
        let result = render("hello world");
        let plain = strip_ansi(&result);
        assert!(plain.contains("hello world"));
    }

    #[test]
    fn test_crlf_line_endings() {
        let result = render("line one\nline two");
        // Should contain \r\n, not bare \n
        assert!(result.contains("\r\n"));
        let without_cr = result.replace("\r\n", "");
        assert!(!without_cr.contains('\n'), "no bare \\n should remain");
    }

    #[test]
    fn test_heading() {
        let result = render("# Title");
        assert!(result.contains(HEADING_COLOR));
        assert!(result.contains("Title"));
        assert!(result.contains(RESET));
    }

    #[test]
    fn test_bold() {
        let result = render("some **bold** text");
        assert!(result.contains(BOLD));
        assert!(result.contains("bold"));
        let plain = strip_ansi(&result);
        assert!(plain.contains("some bold text"));
    }

    #[test]
    fn test_italic() {
        let result = render("some *italic* text");
        assert!(result.contains(ITALIC));
        assert!(result.contains("italic"));
    }

    #[test]
    fn test_inline_code() {
        let result = render("use `foo()` here");
        assert!(result.contains(CODE_BG));
        assert!(result.contains("foo()"));
        let plain = strip_ansi(&result);
        assert!(plain.contains("foo()"));
    }

    #[test]
    fn test_code_block() {
        let result = render("```\nfn main() {}\n```");
        assert!(result.contains(CODE_BG));
        assert!(result.contains("fn main() {}"));
        // Code block content should use \r\n
        assert!(!result.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn test_unordered_list() {
        let result = render("- item one\n- item two");
        let plain = strip_ansi(&result);
        assert!(plain.contains("item one"));
        assert!(plain.contains("item two"));
        // Should contain bullet markers
        assert!(plain.contains("\u{2022}"));
    }

    #[test]
    fn test_ordered_list() {
        let result = render("1. first\n2. second\n3. third");
        let plain = strip_ansi(&result);
        assert!(plain.contains("1."));
        assert!(plain.contains("2."));
        assert!(plain.contains("3."));
        assert!(plain.contains("first"));
        assert!(plain.contains("third"));
    }

    #[test]
    fn test_horizontal_rule() {
        let result = render("above\n\n---\n\nbelow");
        let plain = strip_ansi(&result);
        assert!(plain.contains("───"));
        assert!(plain.contains("above"));
        assert!(plain.contains("below"));
    }

    #[test]
    fn test_link() {
        let result = render("[click](https://example.com)");
        assert!(result.contains(UNDERLINE));
        assert!(result.contains("click"));
    }

    #[test]
    fn test_empty_input() {
        let result = render("");
        assert!(result.is_empty() || result.trim().is_empty());
    }

    #[test]
    fn test_multiple_paragraphs() {
        let result = render("para one\n\npara two");
        let plain = strip_ansi(&result);
        assert!(plain.contains("para one"));
        assert!(plain.contains("para two"));
    }

    #[test]
    fn test_mixed_content() {
        let md = "# Header\n\nSome **bold** and `code`.\n\n- item\n\n```\nblock\n```";
        let result = render(md);
        let plain = strip_ansi(&result);
        assert!(plain.contains("Header"));
        assert!(plain.contains("bold"));
        assert!(plain.contains("code"));
        assert!(plain.contains("item"));
        assert!(plain.contains("block"));
    }

    #[test]
    fn test_blockquote() {
        let result = render("> quoted text");
        assert!(result.contains(DIM));
        assert!(result.contains(GREEN));
        let plain = strip_ansi(&result);
        assert!(plain.contains("quoted text"));
    }
}
