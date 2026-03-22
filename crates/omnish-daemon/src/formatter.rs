use omnish_protocol::message::StatusIcon;

pub struct FormatInput {
    pub tool_name: String,
    pub display_name: String,
    pub status_template: String,
    pub params: serde_json::Value,
    pub output: Option<String>,
    pub is_error: Option<bool>,
}

pub struct FormatOutput {
    pub status_icon: StatusIcon,
    pub param_desc: String,
    pub result_compact: Vec<String>,
    pub result_full: Vec<String>,
}

pub trait ToolFormatter: Send + Sync {
    fn format(&self, input: &FormatInput) -> FormatOutput;
}

fn interpolate_template(template: &str, params: &serde_json::Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = params.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{}}}", key);
            let replacement = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }
    }
    result.replace('\n', "\\n").replace('\r', "\\r")
}

fn status_icon(output: &Option<String>, is_error: &Option<bool>) -> StatusIcon {
    match output {
        None => StatusIcon::Running,
        Some(_) => match is_error {
            Some(true) => StatusIcon::Error,
            _ => StatusIcon::Success,
        },
    }
}

fn head_lines(text: &str, n: usize) -> Vec<String> {
    text.lines().take(n).map(|l| l.to_string()).collect()
}

fn all_lines(text: &str) -> Vec<String> {
    text.lines().map(|l| l.to_string()).collect()
}

/// Truncate output to head + tail if it exceeds `max` lines.
/// Keeps the first `keep` and last `keep` lines with a separator in between.
fn truncate_lines(lines: Vec<String>, max: usize, keep: usize) -> Vec<String> {
    if lines.len() <= max {
        return lines;
    }
    let mut out = Vec::with_capacity(keep * 2 + 1);
    out.extend_from_slice(&lines[..keep]);
    out.push(format!("... ({} lines omitted) ...", lines.len() - keep * 2));
    out.extend_from_slice(&lines[lines.len() - keep..]);
    out
}

// --- DefaultFormatter ---

struct DefaultFormatter;

impl ToolFormatter for DefaultFormatter {
    fn format(&self, input: &FormatInput) -> FormatOutput {
        let param_desc = interpolate_template(&input.status_template, &input.params);
        let icon = status_icon(&input.output, &input.is_error);
        let (result_compact, result_full) = match &input.output {
            None => (vec![], vec![]),
            Some(text) => (head_lines(text, 5), truncate_lines(all_lines(text), 50, 20)),
        };
        FormatOutput {
            status_icon: icon,
            param_desc,
            result_compact,
            result_full,
        }
    }
}

// --- ReadFormatter ---

struct ReadFormatter;

impl ToolFormatter for ReadFormatter {
    fn format(&self, input: &FormatInput) -> FormatOutput {
        let param_desc = input
            .params
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let icon = status_icon(&input.output, &input.is_error);
        let (result_compact, result_full) = match &input.output {
            None => (vec![], vec![]),
            Some(text) => {
                if input.is_error == Some(true) {
                    let full = all_lines(text);
                    let compact = head_lines(text, 5);
                    (compact, full)
                } else {
                    // Count content lines (exclude metadata like "N more lines after...")
                    // Read tool outputs cat -n format: "     1\tcontent"
                    // Count lines that have the tab-separated line number prefix
                    let content_lines: Vec<&str> = text
                        .lines()
                        .filter(|l| l.starts_with(|c: char| c == ' ' || c.is_ascii_digit()) && l.contains('\t'))
                        .collect();
                    let n = content_lines.len();
                    let compact = vec![format!("Read {} lines", n)];
                    let full = if n <= 10 {
                        // Already in cat -n format (lineno\tcontent), pass through
                        content_lines.iter().map(|l| l.to_string()).collect()
                    } else {
                        vec![format!("Read {} lines", n)]
                    };
                    (compact, full)
                }
            }
        };
        FormatOutput {
            status_icon: icon,
            param_desc,
            result_compact,
            result_full,
        }
    }
}

// --- EditFormatter ---

struct EditFormatter;

/// Compute edit summary line from actual changed lines (excluding common prefix/suffix).
fn edit_summary(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = if old.is_empty() { vec![] } else { old.lines().collect() };
    let new_lines: Vec<&str> = if new.is_empty() { vec![] } else { new.lines().collect() };

    // Find common prefix/suffix to count only truly changed lines
    let common_prefix = old_lines.iter().zip(new_lines.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let remaining_old = old_lines.len() - common_prefix;
    let remaining_new = new_lines.len() - common_prefix;
    let common_suffix = old_lines[common_prefix..].iter().rev()
        .zip(new_lines[common_prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(remaining_old)
        .min(remaining_new);

    let removed = old_lines.len() - common_prefix - common_suffix;
    let added = new_lines.len() - common_prefix - common_suffix;

    match (removed, added) {
        (0, n) => format!("Added {} line{}", n, if n == 1 { "" } else { "s" }),
        (n, 0) => format!("Removed {} line{}", n, if n == 1 { "" } else { "s" }),
        (o, n) if o == n => format!("Edited {} line{}", o, if o == 1 { "" } else { "s" }),
        (o, n) => format!(
            "Added {} line{}, removed {} line{}",
            n,
            if n == 1 { "" } else { "s" },
            o,
            if o == 1 { "" } else { "s" }
        ),
    }
}

/// Parse occurrence count from "Replaced N occurrences in ..." output.
fn parse_replace_count(output: &str) -> Option<usize> {
    let first_line = output.lines().next()?;
    let rest = first_line.strip_prefix("Replaced ")?;
    rest.split_whitespace().next()?.parse().ok()
}

/// Format numbered diff from edit tool context snippet.
/// Snippet lines: "lineno:-content" (removed), "lineno:+content" (added),
/// "lineno:  content" (context).
/// The snippet is self-contained — no need to inject old_string.
fn format_numbered_diff(output: &str) -> Vec<String> {
    let snippet = match output.split_once("\n---\n") {
        Some((_, ctx)) => ctx,
        None => return Vec::new(),
    };

    struct DiffLine {
        lineno: usize,
        marker: char, // ' ', '-', '+'
        content: String,
    }

    let mut diff_lines: Vec<DiffLine> = Vec::new();

    for line in snippet.lines() {
        let (num_str, rest) = match line.split_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        let lineno: usize = match num_str.trim().parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        if let Some(content) = rest.strip_prefix('-') {
            diff_lines.push(DiffLine { lineno, marker: '-', content: content.to_string() });
        } else if let Some(content) = rest.strip_prefix('+') {
            diff_lines.push(DiffLine { lineno, marker: '+', content: content.to_string() });
        } else if let Some(content) = rest.strip_prefix("  ") {
            diff_lines.push(DiffLine { lineno, marker: ' ', content: content.to_string() });
        }
    }

    if diff_lines.is_empty() {
        return Vec::new();
    }

    // Determine line number width for alignment
    let max_num = diff_lines.iter().map(|l| l.lineno).max().unwrap_or(0);
    let w = max_num.to_string().len().max(4);

    diff_lines
        .iter()
        .map(|l| match l.marker {
            '-' => format!("\x1b[31m{:>w$} -{}\x1b[0m", l.lineno, l.content),
            '+' => format!("\x1b[32m{:>w$} +{}\x1b[0m", l.lineno, l.content),
            _ => format!("\x1b[2m{:>w$}  {}\x1b[0m", l.lineno, l.content),
        })
        .collect()
}

impl ToolFormatter for EditFormatter {
    fn format(&self, input: &FormatInput) -> FormatOutput {
        let param_desc = input
            .params
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let icon = status_icon(&input.output, &input.is_error);
        let (result_compact, result_full) = match &input.output {
            None => (vec![], vec![]),
            Some(_) => {
                if input.is_error == Some(true) {
                    let text = input.output.as_deref().unwrap_or("");
                    let old = input.params.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                    let mut full = all_lines(text);
                    if !old.is_empty() {
                        full.push(String::new());
                        full.push("old_string:".to_string());
                        for line in old.lines() {
                            full.push(format!("  {}", line));
                        }
                    }
                    let compact = head_lines(text, 5);
                    (compact, full)
                } else {
                    let output = input.output.as_deref().unwrap_or("");
                    let old = input.params.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                    let new = input.params.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                    let replace_all = input
                        .params
                        .get("replace_all")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    let summary = edit_summary(old, new);
                    let mut diff = format_numbered_diff(output);

                    // For replace_all with multiple occurrences, append note
                    if replace_all {
                        if let Some(count) = parse_replace_count(output) {
                            if count > 1 {
                                diff.push(format!(
                                    "\x1b[2m... and {} more place{}\x1b[0m",
                                    count - 1,
                                    if count == 2 { "" } else { "s" }
                                ));
                            }
                        }
                    }

                    let mut full = vec![summary.clone()];
                    full.extend(diff.iter().cloned());

                    let mut compact = vec![summary];
                    compact.extend(diff.into_iter().take(50));

                    (compact, full)
                }
            }
        };
        FormatOutput {
            status_icon: icon,
            param_desc,
            result_compact,
            result_full,
        }
    }
}

// --- Lookup ---

pub fn get_formatter(name: &str) -> &'static dyn ToolFormatter {
    match name {
        "read" => &ReadFormatter,
        "edit" | "write" => &EditFormatter,
        _ => &DefaultFormatter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_input(
        tool_name: &str,
        status_template: &str,
        params: serde_json::Value,
        output: Option<&str>,
        is_error: Option<bool>,
    ) -> FormatInput {
        FormatInput {
            tool_name: tool_name.to_string(),
            display_name: tool_name.to_string(),
            status_template: status_template.to_string(),
            params,
            output: output.map(|s| s.to_string()),
            is_error,
        }
    }

    #[test]
    fn default_formatter_before_execution() {
        let input = make_input(
            "command_query",
            "pattern={pattern}",
            json!({"pattern": "git"}),
            None,
            None,
        );
        let out = DefaultFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Running);
        assert_eq!(out.param_desc, "pattern=git");
        assert!(out.result_compact.is_empty());
        assert!(out.result_full.is_empty());
    }

    #[test]
    fn default_formatter_after_success() {
        let input = make_input(
            "command_query",
            "pattern={pattern}",
            json!({"pattern": "git"}),
            Some("line1\nline2\nline3"),
            Some(false),
        );
        let out = DefaultFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Success);
        assert_eq!(out.result_compact, vec!["line1", "line2", "line3"]);
        assert_eq!(out.result_full, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn default_formatter_after_error() {
        let input = make_input(
            "command_query",
            "pattern={pattern}",
            json!({"pattern": "git"}),
            Some("error: something failed"),
            Some(true),
        );
        let out = DefaultFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Error);
        assert!(!out.result_compact.is_empty());
    }

    #[test]
    fn default_formatter_truncates_compact_to_5_lines() {
        let lines: Vec<&str> = (0..20).map(|_| "x").collect();
        let text = lines.join("\n");
        let input = make_input(
            "command_query",
            "",
            json!({}),
            Some(&text),
            Some(false),
        );
        let out = DefaultFormatter.format(&input);
        assert_eq!(out.result_compact.len(), 5);
        assert_eq!(out.result_full.len(), 20);
    }

    #[test]
    fn default_formatter_truncates_full_over_50_lines() {
        let lines: Vec<String> = (1..=80).map(|i| format!("line{}", i)).collect();
        let text = lines.join("\n");
        let input = make_input(
            "command_query",
            "",
            json!({}),
            Some(&text),
            Some(false),
        );
        let out = DefaultFormatter.format(&input);
        // 20 head + 1 separator + 20 tail = 41
        assert_eq!(out.result_full.len(), 41);
        assert_eq!(out.result_full[0], "line1");
        assert_eq!(out.result_full[19], "line20");
        assert!(out.result_full[20].contains("40 lines omitted"));
        assert_eq!(out.result_full[21], "line61");
        assert_eq!(out.result_full[40], "line80");
    }

    #[test]
    fn default_formatter_escapes_newlines_in_param_desc() {
        let input = make_input(
            "command_query",
            "content={content}",
            json!({"content": "line1\nline2\r\nline3"}),
            None,
            None,
        );
        let out = DefaultFormatter.format(&input);
        assert_eq!(out.param_desc, "content=line1\\nline2\\r\\nline3");
    }

    #[test]
    fn read_formatter_compact_shows_line_count() {
        let input = make_input(
            "read",
            "",
            json!({"file_path": "/tmp/test.txt"}),
            Some("     1\taaa\n     2\tbbb\n     3\tccc"),
            Some(false),
        );
        let out = ReadFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Success);
        assert_eq!(out.param_desc, "/tmp/test.txt");
        assert_eq!(out.result_compact, vec!["Read 3 lines"]);
        // N<=10: full shows numbered lines
        assert_eq!(out.result_full, vec!["     1\taaa", "     2\tbbb", "     3\tccc"]);
    }

    #[test]
    fn read_formatter_full_many_lines_shows_summary() {
        let lines: Vec<String> = (1..=15)
            .map(|i| format!("{:>6}\tline{}", i, i))
            .collect();
        let text = lines.join("\n");
        let input = make_input(
            "read",
            "",
            json!({"file_path": "/tmp/test.txt"}),
            Some(&text),
            Some(false),
        );
        let out = ReadFormatter.format(&input);
        assert_eq!(out.result_compact, vec!["Read 15 lines"]);
        // N>10: full also shows summary
        assert_eq!(out.result_full, vec!["Read 15 lines"]);
    }

    #[test]
    fn edit_formatter_summary_only_no_snippet() {
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/test.txt", "old_string": "hello", "new_string": "goodbye"}),
            Some("Edited /tmp/test.txt"),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Success);
        assert_eq!(out.param_desc, "/tmp/test.txt");
        // No snippet → summary only
        assert_eq!(out.result_compact, vec!["Edited 1 line"]);
        assert_eq!(out.result_full, vec!["Edited 1 line"]);
    }

    #[test]
    fn edit_formatter_numbered_diff() {
        // New snippet format: "lineno:-old" and "lineno:+new"
        let output = "Edited /tmp/test.txt\n---\n1:  line1\n2:  line2\n3:  line3\n4:-hello\n4:+goodbye\n5:  line5\n6:  line6\n7:  line7";
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/test.txt", "old_string": "hello", "new_string": "goodbye"}),
            Some(output),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.result_full[0], "Edited 1 line");
        // full: summary + 3 ctx + 1 old + 1 new + 3 ctx = 9
        assert_eq!(out.result_full.len(), 9);
        // Context (dim)
        assert!(out.result_full[1].contains("line1") && out.result_full[1].contains("\x1b[2m"));
        // Old line (red)
        assert!(out.result_full[4].contains("-hello") && out.result_full[4].contains("\x1b[31m"));
        // New line (green)
        assert!(out.result_full[5].contains("+goodbye") && out.result_full[5].contains("\x1b[32m"));
        // Context after
        assert!(out.result_full[6].contains("line5"));
    }

    #[test]
    fn edit_formatter_multiline_numbered() {
        let output = "Edited /tmp/t.txt\n---\n9:  before\n10:-old_a\n11:-old_b\n12:-old_c\n10:+new_a\n11:+new_b\n12:  after";
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/t.txt", "old_string": "old_a\nold_b\nold_c", "new_string": "new_a\nnew_b"}),
            Some(output),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.result_full[0], "Added 2 lines, removed 3 lines");
        // full: summary(1) + ctx(1) + 3 old + 2 new + ctx(1) = 8
        assert_eq!(out.result_full.len(), 8);
        assert!(out.result_full[1].contains("before"), "ctx before");
        assert!(out.result_full[2].contains("-old_a"), "old_a: {}", out.result_full[2]);
        assert!(out.result_full[4].contains("-old_c"), "old_c");
        assert!(out.result_full[5].contains("+new_a"), "new_a");
        assert!(out.result_full[6].contains("+new_b"), "new_b");
        assert!(out.result_full[7].contains("after"), "ctx after");
    }

    #[test]
    fn edit_formatter_replace_all_multiple() {
        let output = "Replaced 3 occurrences in /tmp/t.txt\n---\n5:-foo line\n5:+bar line\n6:  after";
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/t.txt", "old_string": "foo", "new_string": "bar", "replace_all": true}),
            Some(output),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.result_full[0], "Edited 1 line");
        // Last line: "... and 2 more places"
        let last = out.result_full.last().unwrap();
        assert!(last.contains("2 more places"), "got: {}", last);
    }

    #[test]
    fn edit_formatter_compact_limited_to_50() {
        // Build a snippet with many old(-) and new(+) lines
        let mut snippet_lines = Vec::new();
        for i in 1..=60 {
            snippet_lines.push(format!("{}:-old{}", i, i));
        }
        for i in 1..=60 {
            snippet_lines.push(format!("{}:+line{}", i, i));
        }
        let output = format!("Edited /tmp/t.txt\n---\n{}", snippet_lines.join("\n"));
        let old_lines: Vec<String> = (1..=60).map(|i| format!("old{}", i)).collect();
        let new_lines: Vec<String> = (1..=60).map(|i| format!("line{}", i)).collect();
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/t.txt", "old_string": old_lines.join("\n"), "new_string": new_lines.join("\n")}),
            Some(&output),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        // compact: summary + up to 50 diff lines = 51 max
        assert!(out.result_compact.len() <= 51);
        // full: summary + all diff lines (60 old + 60 new = 120)
        assert!(out.result_full.len() > 51);
    }

    #[test]
    fn edit_formatter_deletion() {
        let output = "Edited /tmp/t.txt\n---\n1:  before1\n2:  before2\n3:-del_a\n4:-del_b\n5:-del_c\n3:  after1\n4:  after2";
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/t.txt", "old_string": "del_a\ndel_b\ndel_c", "new_string": ""}),
            Some(output),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.result_full[0], "Removed 3 lines");
        // full: summary(1) + ctx(2) + 3 old(-) + ctx(2) = 8
        assert_eq!(out.result_full.len(), 8, "full: {:?}", out.result_full);
        assert!(out.result_full[1].contains("before1"));
        assert!(out.result_full[2].contains("before2"));
        assert!(out.result_full[3].contains("-del_a"));
        assert!(out.result_full[5].contains("-del_c"));
        assert!(out.result_full[6].contains("after1"));
        assert!(out.result_full[7].contains("after2"));
    }

    #[test]
    fn edit_formatter_error_shows_message_and_old_string() {
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/test.txt", "old_string": "fn foo() {\n    bar()\n}"}),
            Some("Error: old_string not found in /tmp/test.txt"),
            Some(true),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Error);
        assert_eq!(out.result_compact, vec!["Error: old_string not found in /tmp/test.txt"]);
        assert_eq!(out.result_full[0], "Error: old_string not found in /tmp/test.txt");
        assert_eq!(out.result_full[2], "old_string:");
        assert_eq!(out.result_full[3], "  fn foo() {");
    }

    #[test]
    fn edit_formatter_error_no_old_string() {
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/test.txt"}),
            Some("permission denied"),
            Some(true),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.status_icon, StatusIcon::Error);
        assert_eq!(out.result_full, vec!["permission denied"]);
    }

    #[test]
    fn edit_formatter_substring_replace_all() {
        // Simulates: replace "or" → "OR" in a file where "or" is a substring
        // The snippet should show full lines, not just "or"/"OR"
        let output = "Replaced 5 occurrences in /tmp/t.txt\n---\n1:-the world or nothing\n1:+the wORld OR nothing\n2:  unchanged line\n3:  another line";
        let input = make_input(
            "edit",
            "",
            json!({"file_path": "/tmp/t.txt", "old_string": "or", "new_string": "OR", "replace_all": true}),
            Some(output),
            Some(false),
        );
        let out = EditFormatter.format(&input);
        assert_eq!(out.result_full[0], "Edited 1 line");
        // Old full line (red)
        assert!(out.result_full[1].contains("-the world or nothing"));
        // New full line (green)
        assert!(out.result_full[2].contains("+the wORld OR nothing"));
        // "... and 4 more places"
        let last = out.result_full.last().unwrap();
        assert!(last.contains("4 more places"), "got: {}", last);
    }

    #[test]
    fn edit_summary_counts_only_changed_lines() {
        // When old/new strings share many common lines, summary should reflect actual changes
        assert_eq!(edit_summary("line1\nline2\nold\nline4", "line1\nline2\nnew\nline4"), "Edited 1 line");
        assert_eq!(edit_summary("a\nb\nc", "a\nX\nY\nc"), "Added 2 lines, removed 1 line");
        assert_eq!(edit_summary("hello", "goodbye"), "Edited 1 line");
        assert_eq!(edit_summary("a\nb\nc", ""), "Removed 3 lines");
        assert_eq!(edit_summary("", "a\nb"), "Added 2 lines");
    }
}
