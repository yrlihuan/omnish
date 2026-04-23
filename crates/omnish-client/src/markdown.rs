// crates/omnish-client/src/markdown.rs
//
// Render markdown text to ANSI-escaped terminal output.
// Uses pulldown-cmark for parsing, produces raw-mode compatible output ({NEWLINE}).

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::display::NEWLINE;

// ANSI style codes - shared constants from display, plus markdown-only styles
const RESET: &str = crate::display::RESET;
const BOLD: &str = crate::display::BOLD;
const DIM: &str = crate::display::DIM;
const CYAN: &str = crate::display::CYAN;
const YELLOW: &str = crate::display::YELLOW;
const GREEN: &str = crate::display::GREEN;
const ITALIC: &str = "\x1b[3m";
const UNDERLINE: &str = "\x1b[4m";
const STRIKE: &str = "\x1b[9m";
const CODE_BG: &str = "\x1b[40m"; // black background (avoid 256-color gray that renders poorly on some terminals)
const HEADING_COLOR: &str = "\x1b[1;36m"; // bold cyan

/// Stack-based renderer that keeps each NEWLINE-separated line self-contained:
/// active styles are reset before every NEWLINE and reapplied after, so
/// downstream consumers can split on NEWLINE without styles bleeding from one
/// line into the next (and blocks like code/blockquote that span multiple
/// lines stay uniformly styled across their lines).
struct Renderer {
    out: String,
    styles: Vec<&'static str>,
    in_code_block: bool,
    list_depth: usize,
    ordered_index: Vec<u64>,
    at_line_start: bool,
}

impl Renderer {
    fn new() -> Self {
        Self {
            out: String::new(),
            styles: Vec::new(),
            in_code_block: false,
            list_depth: 0,
            ordered_index: Vec::new(),
            at_line_start: true,
        }
    }

    fn push(&mut self, s: &str) {
        if !s.is_empty() {
            self.at_line_start = false;
        }
        self.out.push_str(s);
    }

    /// Push a style onto the active stack and emit it.
    fn open(&mut self, style: &'static str) {
        self.styles.push(style);
        self.out.push_str(style);
    }

    /// Pop the top style; emit RESET and reapply the remaining stack.
    fn close(&mut self) {
        self.styles.pop();
        self.out.push_str(RESET);
        for s in &self.styles {
            self.out.push_str(s);
        }
    }

    /// Emit a NEWLINE safe for consumers that split on NEWLINE: reset before
    /// the newline (if any style is active) and reapply the active stack after.
    fn newline(&mut self) {
        if !self.styles.is_empty() {
            self.out.push_str(RESET);
        }
        self.out.push_str(NEWLINE);
        for s in &self.styles {
            self.out.push_str(s);
        }
        self.at_line_start = true;
    }

    /// Emit a self-contained styled span (resets at its own boundary) then
    /// restore the active stack. Used for list bullets, inline code, rules.
    fn scoped(&mut self, span: &str) {
        self.out.push_str(span);
        for s in &self.styles {
            self.out.push_str(s);
        }
        self.at_line_start = false;
    }
}

/// Render markdown content to ANSI terminal output.
/// Output uses {NEWLINE} line endings for raw-mode terminals. Every NEWLINE is
/// style-safe: active styles are reset before it and reapplied after, so
/// consumers that split on NEWLINE get lines that render identically whether
/// they are printed straight to the terminal or re-assembled line by line.
pub fn render(content: &str) -> String {
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let parser = Parser::new_ext(content, options);
    let mut r = Renderer::new();

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { .. } => r.open(HEADING_COLOR),
                Tag::Paragraph
                    if !r.out.is_empty() && r.list_depth == 0 && !r.at_line_start =>
                {
                    r.newline();
                }
                Tag::Paragraph => {}
                Tag::BlockQuote(_) => {
                    r.open(DIM);
                    r.open(GREEN);
                }
                Tag::CodeBlock(_) => {
                    r.in_code_block = true;
                    r.newline();
                    r.open(CODE_BG);
                    r.open(YELLOW);
                }
                Tag::List(start) => {
                    r.list_depth += 1;
                    r.ordered_index.push(start.unwrap_or(0)); // 0 = unordered
                }
                Tag::Item => {
                    let indent = "  ".repeat(r.list_depth.saturating_sub(1));
                    if let Some(&idx) = r.ordered_index.last() {
                        let bullet = if idx > 0 {
                            if let Some(last) = r.ordered_index.last_mut() {
                                *last += 1;
                            }
                            format!("{}{}{}.{} ", indent, DIM, idx, RESET)
                        } else {
                            format!("{}{}\u{2022}{} ", indent, DIM, RESET)
                        };
                        r.scoped(&bullet);
                    }
                }
                Tag::Emphasis => r.open(ITALIC),
                Tag::Strong => r.open(BOLD),
                Tag::Strikethrough => r.open(STRIKE),
                Tag::Link { .. } => {
                    r.open(UNDERLINE);
                    r.open(CYAN);
                }
                Tag::TableCell => r.push("| "),
                Tag::Table(_) | Tag::TableHead | Tag::TableRow => {}
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    r.close();
                    r.newline();
                }
                TagEnd::Paragraph => r.newline(),
                TagEnd::BlockQuote(_) => {
                    r.close(); // GREEN
                    r.close(); // DIM
                }
                TagEnd::CodeBlock => {
                    r.in_code_block = false;
                    r.close(); // YELLOW
                    r.close(); // CODE_BG
                    r.newline();
                }
                TagEnd::List(_) => {
                    r.list_depth = r.list_depth.saturating_sub(1);
                    r.ordered_index.pop();
                }
                TagEnd::Item => r.newline(),
                TagEnd::Emphasis => r.close(),
                TagEnd::Strong => r.close(),
                TagEnd::Strikethrough => r.close(),
                TagEnd::Link => {
                    r.close(); // CYAN
                    r.close(); // UNDERLINE
                }
                TagEnd::TableHead => {
                    r.push("|");
                    r.newline();
                }
                TagEnd::TableRow => {
                    r.push("|");
                    r.newline();
                }
                TagEnd::TableCell => r.push(" "),
                _ => {}
            },
            Event::Text(text) => {
                if r.in_code_block {
                    // Code block text often carries a trailing '\n'; split on
                    // '\n' and use style-safe newlines between segments so the
                    // code-block style is reapplied on every line.
                    let segments: Vec<&str> = text.split('\n').collect();
                    let last = segments.len().saturating_sub(1);
                    for (i, seg) in segments.iter().enumerate() {
                        if i > 0 {
                            r.newline();
                        }
                        if i == last && seg.is_empty() {
                            // Trailing empty segment from a final '\n': already
                            // emitted the preceding newline; skip the empty push.
                            break;
                        }
                        r.push(seg);
                    }
                } else {
                    r.push(&text);
                }
            }
            Event::Code(code) => {
                // Inline code is scoped: reset active styles around the span
                // so CODE_BG+YELLOW don't combine with e.g. outer BOLD, then
                // restore the stack afterwards.
                let span = format!("{RESET}{CODE_BG}{YELLOW} {} {RESET}", code);
                r.scoped(&span);
            }
            Event::SoftBreak => r.newline(),
            Event::HardBreak => r.newline(),
            Event::Rule => {
                let span = format!("{DIM}───────────{RESET}");
                r.scoped(&span);
                r.newline();
            }
            _ => {}
        }
    }

    // Trim trailing empty lines (repeated NEWLINE at end means blank lines)
    let double = format!("{NEWLINE}{NEWLINE}");
    while r.out.ends_with(&double) {
        r.out.truncate(r.out.len() - NEWLINE.len());
    }

    r.out
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
        // Should contain {NEWLINE}, not bare \n
        assert!(result.contains(NEWLINE));
        let without_cr = result.replace(NEWLINE, "");
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
        // Code block content should use {NEWLINE}
        assert!(!result.replace(NEWLINE, "").contains('\n'));
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

    /// Issue #582: each NEWLINE in the output must be style-safe so that
    /// consumers splitting on NEWLINE (compact/full chat views) render the
    /// same content identically. Every line that carries block content must
    /// start with its own style sequence and end with RESET.
    #[test]
    fn test_newline_is_style_safe() {
        // Multi-line blockquote: every content line needs DIM+GREEN applied
        // and a RESET before the line break.
        let result = render("> line1\n> line2\n> line3");
        let lines: Vec<&str> = result.split(NEWLINE).collect();
        // All non-empty content lines should open the quote style and end in RESET.
        let content_lines: Vec<&&str> = lines.iter()
            .filter(|l| strip_ansi(l).trim() == "line1"
                || strip_ansi(l).trim() == "line2"
                || strip_ansi(l).trim() == "line3")
            .collect();
        assert_eq!(content_lines.len(), 3, "expected 3 content lines, got: {:?}", lines);
        for line in &content_lines {
            assert!(line.contains(DIM) && line.contains(GREEN),
                "each blockquote line should carry DIM+GREEN, got: {:?}", line);
            assert!(line.ends_with(RESET),
                "each blockquote line should end with RESET, got: {:?}", line);
        }
    }

    /// Issue #582: cross-line bold should keep every wrapped line bold and
    /// reset at each line boundary so consumers don't bleed BOLD into the
    /// following line's prefix or content.
    #[test]
    fn test_cross_line_bold_reapplies_per_line() {
        let result = render("**line1\nline2**");
        let lines: Vec<&str> = result.split(NEWLINE).collect();
        let line1 = lines.iter().find(|l| strip_ansi(l).contains("line1")).unwrap();
        let line2 = lines.iter().find(|l| strip_ansi(l).contains("line2")).unwrap();
        assert!(line1.contains(BOLD) && line1.ends_with(RESET),
            "line1 should carry BOLD and end with RESET: {:?}", line1);
        assert!(line2.contains(BOLD),
            "line2 should also reapply BOLD: {:?}", line2);
    }

    /// Issue #582: multi-line code block - every line must carry the code
    /// style (CODE_BG+YELLOW) and terminate with RESET independently.
    #[test]
    fn test_code_block_each_line_self_styled() {
        let result = render("```\nline1\nline2\nline3\n```");
        let lines: Vec<&str> = result.split(NEWLINE).collect();
        let code_lines: Vec<&&str> = lines.iter()
            .filter(|l| {
                let p = strip_ansi(l);
                p == "line1" || p == "line2" || p == "line3"
            })
            .collect();
        assert_eq!(code_lines.len(), 3, "expected 3 code lines, got: {:?}", lines);
        for line in &code_lines {
            assert!(line.contains(CODE_BG) && line.contains(YELLOW),
                "each code line should carry CODE_BG+YELLOW: {:?}", line);
            assert!(line.ends_with(RESET),
                "each code line should end with RESET: {:?}", line);
        }
    }
}
