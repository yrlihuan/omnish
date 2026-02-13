use crate::prompt_detector::PromptDetector;
use omnish_store::command::CommandRecord;

const SUMMARY_HEAD_LINES: usize = 5;
const SUMMARY_TAIL_LINES: usize = 5;

struct PendingCommand {
    seq: u32,
    started_at: u64,
    stream_offset: u64,
    input_buf: Vec<u8>,
    output_lines: Vec<String>,
}

pub struct CommandTracker {
    session_id: String,
    cwd: Option<String>,
    detector: PromptDetector,
    pending: Option<PendingCommand>,
    next_seq: u32,
    seen_first_prompt: bool,
}

impl CommandTracker {
    pub fn new(session_id: String, cwd: Option<String>) -> Self {
        Self {
            session_id,
            cwd,
            detector: PromptDetector::new(),
            pending: None,
            next_seq: 0,
            seen_first_prompt: false,
        }
    }

    pub fn tracking(&self) -> bool {
        self.seen_first_prompt
    }

    pub fn feed_input(&mut self, data: &[u8], _timestamp_ms: u64) {
        if let Some(ref mut pending) = self.pending {
            pending.input_buf.extend_from_slice(data);
        }
    }

    pub fn feed_output(
        &mut self,
        data: &[u8],
        timestamp_ms: u64,
        stream_pos: u64,
    ) -> Vec<CommandRecord> {
        // Append output lines to pending command before prompt detection
        if let Some(ref mut pending) = self.pending {
            let text = String::from_utf8_lossy(data);
            for line in text.split('\n') {
                let trimmed = line.trim_end_matches('\r');
                if !trimmed.is_empty() {
                    pending.output_lines.push(trimmed.to_string());
                }
            }
        }

        let events = self.detector.feed(data);
        let mut completed = Vec::new();

        for _event in events {
            if !self.seen_first_prompt {
                // First prompt: start tracking, create initial pending command
                self.seen_first_prompt = true;
                self.pending = Some(PendingCommand {
                    seq: self.next_seq,
                    started_at: timestamp_ms,
                    stream_offset: stream_pos,
                    input_buf: Vec::new(),
                    output_lines: Vec::new(),
                });
                self.next_seq += 1;
            } else {
                // Subsequent prompt: finalize pending command, start new one
                if let Some(pending) = self.pending.take() {
                    let command_line = extract_command_line(&pending.input_buf);
                    let output_summary = make_summary(&pending.output_lines);
                    let stream_length = stream_pos - pending.stream_offset;

                    completed.push(CommandRecord {
                        command_id: format!("{}:{}", self.session_id, pending.seq),
                        session_id: self.session_id.clone(),
                        command_line,
                        cwd: self.cwd.clone(),
                        started_at: pending.started_at,
                        ended_at: Some(timestamp_ms),
                        output_summary,
                        stream_offset: pending.stream_offset,
                        stream_length,
                    });
                }

                self.pending = Some(PendingCommand {
                    seq: self.next_seq,
                    started_at: timestamp_ms,
                    stream_offset: stream_pos,
                    input_buf: Vec::new(),
                    output_lines: Vec::new(),
                });
                self.next_seq += 1;
            }
        }

        completed
    }
}

fn extract_command_line(input: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(input);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Take first line only
    let first_line = trimmed.lines().next().unwrap_or("");
    let first_line = first_line.trim();
    if first_line.is_empty() {
        None
    } else {
        Some(first_line.to_string())
    }
}

fn make_summary(lines: &[String]) -> String {
    let total = lines.len();
    if total <= SUMMARY_HEAD_LINES + SUMMARY_TAIL_LINES {
        lines.join("\n")
    } else {
        let head = &lines[..SUMMARY_HEAD_LINES];
        let tail = &lines[total - SUMMARY_TAIL_LINES..];
        let omitted = total - SUMMARY_HEAD_LINES - SUMMARY_TAIL_LINES;
        format!(
            "{}\n... ({} lines omitted) ...\n{}",
            head.join("\n"),
            omitted,
            tail.join("\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tracker() -> CommandTracker {
        CommandTracker::new("sess1".into(), None)
    }

    #[test]
    fn test_first_prompt_starts_tracking() {
        let mut tracker = make_tracker();
        let cmds = tracker.feed_output(b"user@host:~$ ", 1000, 0);
        assert!(cmds.is_empty(), "first prompt should not produce a command");
        assert!(tracker.tracking(), "should be tracking after first prompt");
    }

    #[test]
    fn test_simple_command_recorded() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"user@host:~$ ", 1000, 0);
        tracker.feed_input(b"ls -la\r\n", 1001);
        let cmds = tracker.feed_output(
            b"total 0\r\nfile.txt\r\nuser@host:~$ ",
            1002,
            100,
        );
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].command_line.as_deref(), Some("ls -la"));
        assert_eq!(cmds[0].started_at, 1000);
        assert_eq!(cmds[0].ended_at, Some(1002));
    }

    #[test]
    fn test_output_summary_head_tail() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"user@host:~$ ", 1000, 0);
        tracker.feed_input(b"long-cmd\r\n", 1001);

        let mut output = String::new();
        for i in 0..20 {
            output.push_str(&format!("line {}\r\n", i));
        }
        output.push_str("user@host:~$ ");

        let cmds = tracker.feed_output(output.as_bytes(), 1002, 100);
        assert_eq!(cmds.len(), 1);
        let summary = &cmds[0].output_summary;
        assert!(summary.contains("line 0"), "summary should contain head");
        assert!(summary.contains("line 19"), "summary should contain tail");
    }

    #[test]
    fn test_command_id_sequential() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"$ ", 1000, 0);

        tracker.feed_input(b"cmd1\r\n", 1001);
        let cmds1 = tracker.feed_output(b"out1\r\n$ ", 1002, 50);
        assert_eq!(cmds1[0].command_id, "sess1:0");

        tracker.feed_input(b"cmd2\r\n", 1003);
        let cmds2 = tracker.feed_output(b"out2\r\n$ ", 1004, 100);
        assert_eq!(cmds2[0].command_id, "sess1:1");
    }

    #[test]
    fn test_no_command_without_prompt() {
        let mut tracker = make_tracker();
        let cmds = tracker.feed_output(b"some random output\r\n", 1000, 0);
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_cwd_from_session() {
        let mut tracker = CommandTracker::new(
            "sess1".into(),
            Some("/home/user/project".into()),
        );
        tracker.feed_output(b"$ ", 1000, 0);
        tracker.feed_input(b"make\r\n", 1001);
        let cmds = tracker.feed_output(b"done\r\n$ ", 1002, 100);
        assert_eq!(cmds[0].cwd.as_deref(), Some("/home/user/project"));
    }

    #[test]
    fn test_stream_offsets_recorded() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"$ ", 1000, 50);
        tracker.feed_input(b"ls\r\n", 1001);
        let cmds = tracker.feed_output(b"out\r\n$ ", 1002, 200);
        assert_eq!(cmds[0].stream_offset, 50);
        assert_eq!(cmds[0].stream_length, 200 - 50);
    }
}
