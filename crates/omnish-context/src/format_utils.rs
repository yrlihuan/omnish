use std::collections::HashMap;

/// Format millisecond timestamp as relative time string.
/// Rules: <60s -> "Ns ago", <60m -> "Nm ago", <24h -> "Nh ago", >=24h -> "Nd ago"
/// If now_ms <= timestamp_ms -> "just now"
pub fn format_relative_time(timestamp_ms: u64, now_ms: u64) -> String {
    if now_ms <= timestamp_ms {
        return "just now".to_string();
    }
    let diff_ms = now_ms - timestamp_ms;
    let seconds = diff_ms / 1000;
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;

    if days >= 1 {
        format!("{}d ago", days)
    } else if hours >= 1 {
        format!("{}h ago", hours)
    } else if minutes >= 1 {
        format!("{}m ago", minutes)
    } else {
        format!("{}s ago", seconds)
    }
}

/// Generate a term name for index 0..N.
/// 0 → "term A", 25 → "term Z", 26 → "term AA", 27 → "term AB", ...,
/// 701 → "term ZZ", 702 → "term AAA", etc.  Works for any index.
fn term_name(index: usize) -> String {
    // Convert index to bijective base-26: A=0..Z=25, AA=26..AZ=51, BA=52..
    let mut n = index;
    let mut letters = Vec::new();
    loop {
        letters.push((b'A' + (n % 26) as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    letters.reverse();
    format!("term {}", letters.into_iter().collect::<String>())
}

/// Build the final label map from term_letters + hostname lookup.
fn build_labels(
    term_letters: &HashMap<String, String>,
    commands: &[super::CommandContext],
) -> HashMap<String, String> {
    let mut hostnames: HashMap<String, String> = HashMap::new();
    for cmd in commands {
        if !hostnames.contains_key(&cmd.session_id) {
            if let Some(ref h) = cmd.hostname {
                hostnames.insert(cmd.session_id.clone(), h.clone());
            }
        }
    }

    let mut labels = HashMap::new();
    for (sid, term) in term_letters {
        let label = match hostnames.get(sid) {
            Some(hostname) => format!("{} ({})", hostname, term),
            None => term.clone(),
        };
        labels.insert(sid.clone(), label);
    }
    labels
}

/// Assign session labels using hostname as primary identifier.
/// Format: "hostname (term A)", or "term A" if no hostname available.
/// current_session_id always gets "term A", others get "term B", "term C", etc.
pub fn assign_term_labels(
    commands: &[super::CommandContext],
    current_session_id: &str,
) -> HashMap<String, String> {
    let mut term_letters: HashMap<String, String> = HashMap::new();
    term_letters.insert(current_session_id.to_string(), term_name(0));

    let mut index = 1usize;
    for cmd in commands {
        if !term_letters.contains_key(&cmd.session_id) {
            term_letters.insert(cmd.session_id.clone(), term_name(index));
            index += 1;
        }
    }

    build_labels(&term_letters, commands)
}

/// Assign session labels by chronological order of first appearance.
/// Unlike `assign_term_labels`, this does NOT prioritize the current session —
/// labels are stable across requests regardless of which terminal is "current".
/// Format: "hostname (term A)", or "term A" if no hostname available.
pub fn assign_stable_term_labels(
    commands: &[super::CommandContext],
) -> HashMap<String, String> {
    let mut term_letters: HashMap<String, String> = HashMap::new();
    let mut index = 0usize;

    for cmd in commands {
        if !term_letters.contains_key(&cmd.session_id) {
            term_letters.insert(cmd.session_id.clone(), term_name(index));
            index += 1;
        }
    }

    build_labels(&term_letters, commands)
}

/// Truncate each line to at most `max_width` characters.
/// Lines that exceed the limit are cut and appended with "..." (the total may be max_width + 3).
pub fn truncate_line_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return text.to_string();
    }
    let mut result = String::new();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        if line.chars().count() > max_width {
            result.extend(line.chars().take(max_width));
            result.push_str("...");
        } else {
            result.push_str(line);
        }
    }
    result
}

/// Truncate output lines. If over max_lines, keep head + "..." + tail.
/// Also limits total characters to max_chars if provided.
pub fn truncate_lines(text: &str, max_lines: usize, head: usize, tail: usize, max_chars: Option<usize>) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .collect();

    let total = lines.len();
    if total <= max_lines {
        // Even if within line limit, still check character limit
        if let Some(limit) = max_chars {
            let content = lines.join("\n");
            if content.chars().count() > limit {
                return truncate_by_chars(&content, limit);
            }
        }
        lines.join("\n")
    } else {
        let head_part = &lines[..head];
        let tail_part = &lines[total - tail..];
        let omitted = total - head - tail;
        format!(
            "{}\n... ({} lines omitted) ...\n{}",
            head_part.join("\n"),
            omitted,
            tail_part.join("\n")
        )
    }
}

/// Truncate text to at most max_chars characters.
/// Keeps head + "..." + tail where head and tail are roughly equal.
fn truncate_by_chars(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }

    // Reserve 5 chars for "..."
    let available = max_chars.saturating_sub(5);
    let head_len = available / 2;
    let tail_len = available - head_len;

    let head: String = chars[..head_len].iter().collect();
    let tail: String = chars[chars.len() - tail_len..].iter().collect();

    format!("{}...{}", head, tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommandContext;

    #[test]
    fn test_relative_time_seconds() {
        assert_eq!(format_relative_time(59000, 60000), "1s ago");
        assert_eq!(format_relative_time(10000, 10000), "just now");
    }

    #[test]
    fn test_relative_time_minutes() {
        assert_eq!(format_relative_time(0, 120000), "2m ago");
        assert_eq!(format_relative_time(0, 3599000), "59m ago");
    }

    #[test]
    fn test_relative_time_hours() {
        assert_eq!(format_relative_time(0, 3600000), "1h ago");
        assert_eq!(format_relative_time(0, 86399000), "23h ago");
    }

    #[test]
    fn test_relative_time_days() {
        assert_eq!(format_relative_time(0, 86400000), "1d ago");
    }

    #[test]
    fn test_relative_time_future() {
        assert_eq!(format_relative_time(100000, 50000), "just now");
    }

    #[test]
    fn test_assign_labels_current_first() {
        let commands = vec![
            CommandContext {
                session_id: "my-sess".into(),
                hostname: Some("host-a".into()),
                command_line: Some("ls".into()),
                cwd: None,
                started_at: 1000,
                ended_at: Some(1050),
                output: String::new(),
                exit_code: None,
            },
            CommandContext {
                session_id: "other-sess".into(),
                hostname: Some("host-b".into()),
                command_line: Some("pwd".into()),
                cwd: None,
                started_at: 2000,
                ended_at: Some(2050),
                output: String::new(),
                exit_code: None,
            },
        ];
        let labels = assign_term_labels(&commands, "my-sess");
        assert_eq!(labels.get("my-sess").unwrap(), "host-a (term A)");
        assert_eq!(labels.get("other-sess").unwrap(), "host-b (term B)");
    }

    #[test]
    fn test_assign_labels_single_session() {
        let commands = vec![CommandContext {
            session_id: "only".into(),
            hostname: Some("myhost".into()),
            command_line: Some("ls".into()),
            cwd: None,
            started_at: 1000,
            ended_at: Some(1050),
            output: String::new(),
            exit_code: None,
        }];
        let labels = assign_term_labels(&commands, "only");
        assert_eq!(labels.len(), 1);
        assert_eq!(labels.get("only").unwrap(), "myhost (term A)");
    }

    #[test]
    fn test_assign_labels_no_hostname_fallback() {
        let commands = vec![CommandContext {
            session_id: "only".into(),
            hostname: None,
            command_line: Some("ls".into()),
            cwd: None,
            started_at: 1000,
            ended_at: Some(1050),
            output: String::new(),
            exit_code: None,
        }];
        let labels = assign_term_labels(&commands, "only");
        assert_eq!(labels.get("only").unwrap(), "term A");
    }

    #[test]
    fn test_truncate_lines_short() {
        let text = "line 1\nline 2\nline 3\n";
        let result = truncate_lines(text, 20, 10, 10, None);
        assert_eq!(result, "line 1\nline 2\nline 3");
    }

    #[test]
    fn test_truncate_lines_long() {
        let mut text = String::new();
        for i in 0..30 {
            text.push_str(&format!("line {}\n", i));
        }
        let result = truncate_lines(&text, 20, 10, 10, None);
        assert!(result.contains("line 0"));
        assert!(result.contains("line 9"));
        assert!(result.contains("... (10 lines omitted) ..."));
        assert!(result.contains("line 20"));
        assert!(result.contains("line 29"));
        assert!(!result.contains("\nline 10\n"));
    }

    #[test]
    fn test_truncate_lines_by_chars() {
        // Test character limit - text with 600 chars should be truncated to 500
        let text = "x".repeat(600);
        let result = truncate_lines(&text, 1000, 500, 500, Some(500));
        assert!(result.len() <= 503); // 500 chars + "..."
    }

    #[test]
    fn test_truncate_by_chars_head_tail() {
        // Test that truncation keeps head and tail
        let text = "abcdefghijklmnopqrstuvwxyz";
        let result = truncate_by_chars(&text, 10);
        assert!(result.starts_with("ab"));
        assert!(result.ends_with("yz"));
        assert!(result.contains("..."));
    }

    #[test]
    fn test_truncate_line_width_short_lines_unchanged() {
        let text = "short\nlines\nhere";
        assert_eq!(truncate_line_width(text, 512), text);
    }

    #[test]
    fn test_truncate_line_width_long_line_truncated() {
        let long = "x".repeat(600);
        let text = format!("ok\n{}\nend", long);
        let result = truncate_line_width(&text, 512);
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines[0], "ok");
        assert_eq!(lines[1].len(), 515); // 512 chars + "..."
        assert!(lines[1].ends_with("..."));
        assert_eq!(lines[2], "end");
    }

    #[test]
    fn test_truncate_line_width_zero_is_noop() {
        let text = "x".repeat(1000);
        assert_eq!(truncate_line_width(&text, 0), text);
    }

    #[test]
    fn test_term_name_single_letter() {
        assert_eq!(term_name(0), "term A");
        assert_eq!(term_name(1), "term B");
        assert_eq!(term_name(25), "term Z");
    }

    #[test]
    fn test_term_name_double_letter() {
        assert_eq!(term_name(26), "term AA");
        assert_eq!(term_name(27), "term AB");
        assert_eq!(term_name(51), "term AZ");
        assert_eq!(term_name(52), "term BA");
        assert_eq!(term_name(701), "term ZZ");
    }

    #[test]
    fn test_term_name_triple_letter() {
        assert_eq!(term_name(702), "term AAA");
        assert_eq!(term_name(703), "term AAB");
        assert_eq!(term_name(728), "term ABA");
    }
}
