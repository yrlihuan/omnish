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

/// Assign session labels: current_session_id -> "term A", others -> "term B", "term C", etc.
/// in order of first appearance in commands.
pub fn assign_term_labels(
    commands: &[super::CommandContext],
    current_session_id: &str,
) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    // Always assign current session as "term A"
    labels.insert(current_session_id.to_string(), "term A".to_string());

    let mut next_letter = b'B';
    for cmd in commands {
        if !labels.contains_key(&cmd.session_id) {
            labels.insert(
                cmd.session_id.clone(),
                format!("term {}", next_letter as char),
            );
            next_letter += 1;
        }
    }
    labels
}

/// Truncate output lines. If over max_lines, keep head + "... (N lines omitted) ..." + tail.
pub fn truncate_lines(text: &str, max_lines: usize, head: usize, tail: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .collect();

    let total = lines.len();
    if total <= max_lines {
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
                command_line: Some("ls".into()),
                cwd: None,
                started_at: 1000,
                ended_at: Some(1050),
                output: String::new(),
                exit_code: None,
            },
            CommandContext {
                session_id: "other-sess".into(),
                command_line: Some("pwd".into()),
                cwd: None,
                started_at: 2000,
                ended_at: Some(2050),
                output: String::new(),
                exit_code: None,
            },
        ];
        let labels = assign_term_labels(&commands, "my-sess");
        assert_eq!(labels.get("my-sess").unwrap(), "term A");
        assert_eq!(labels.get("other-sess").unwrap(), "term B");
    }

    #[test]
    fn test_assign_labels_single_session() {
        let commands = vec![CommandContext {
            session_id: "only".into(),
            command_line: Some("ls".into()),
            cwd: None,
            started_at: 1000,
            ended_at: Some(1050),
            output: String::new(),
            exit_code: None,
        }];
        let labels = assign_term_labels(&commands, "only");
        assert_eq!(labels.len(), 1);
        assert_eq!(labels.get("only").unwrap(), "term A");
    }

    #[test]
    fn test_truncate_lines_short() {
        let text = "line 1\nline 2\nline 3\n";
        let result = truncate_lines(text, 20, 10, 10);
        assert_eq!(result, "line 1\nline 2\nline 3");
    }

    #[test]
    fn test_truncate_lines_long() {
        let mut text = String::new();
        for i in 0..30 {
            text.push_str(&format!("line {}\n", i));
        }
        let result = truncate_lines(&text, 20, 10, 10);
        assert!(result.contains("line 0"));
        assert!(result.contains("line 9"));
        assert!(result.contains("... (10 lines omitted) ..."));
        assert!(result.contains("line 20"));
        assert!(result.contains("line 29"));
        assert!(!result.contains("\nline 10\n"));
    }
}
