use crate::osc133_detector::{Osc133Event, Osc133EventKind};
use crate::prompt_detector::{strip_ansi, PromptDetector};
use omnish_store::command::CommandRecord;

const SUMMARY_HEAD_LINES: usize = 5;
const SUMMARY_TAIL_LINES: usize = 5;

struct PendingCommand {
    seq: u32,
    started_at: u64,
    stream_offset: u64,
    input_buf: Vec<u8>,
    output_lines: Vec<String>,
    /// True once we've seen \r or \n in the input (user pressed Enter).
    /// Output before this point is shell echo and should be excluded from the summary.
    entered: bool,
    /// Command line text from OSC 133;B payload (shell's $BASH_COMMAND).
    osc_command_line: Option<String>,
    /// Current working directory from OSC 133;B payload.
    osc_cwd: Option<String>,
}

pub struct CommandTracker {
    session_id: String,
    cwd: Option<String>,
    detector: PromptDetector,
    pending: Option<PendingCommand>,
    next_seq: u32,
    seen_first_prompt: bool,
    osc133_mode: bool,
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
            osc133_mode: false,
        }
    }

    pub fn tracking(&self) -> bool {
        self.seen_first_prompt
    }

    pub fn feed_input(&mut self, data: &[u8], _timestamp_ms: u64) {
        if let Some(ref mut pending) = self.pending {
            pending.input_buf.extend_from_slice(data);
            if data.contains(&b'\r') || data.contains(&b'\n') {
                pending.entered = true;
            }
        }
    }

    fn finalize_command(
        &self,
        pending: PendingCommand,
        timestamp_ms: u64,
        stream_pos: u64,
        exit_code: Option<i32>,
    ) -> CommandRecord {
        let command_line = pending
            .osc_command_line
            .or_else(|| extract_command_line(&pending.input_buf));
        // Use runtime cwd if available, otherwise fall back to session cwd
        let cwd = pending.osc_cwd.or_else(|| self.cwd.clone());
        let output_summary = make_summary(&pending.output_lines);
        let stream_length = stream_pos - pending.stream_offset;
        CommandRecord {
            command_id: format!("{}:{}", self.session_id, pending.seq),
            session_id: self.session_id.clone(),
            command_line,
            cwd,
            started_at: pending.started_at,
            ended_at: Some(timestamp_ms),
            output_summary,
            stream_offset: pending.stream_offset,
            stream_length,
            exit_code,
        }
    }

    pub fn feed_output(
        &mut self,
        data: &[u8],
        timestamp_ms: u64,
        stream_pos: u64,
    ) -> Vec<CommandRecord> {
        // Append output lines to pending command before prompt detection.
        // Only collect after Enter has been pressed (to exclude shell echo of typed chars).
        // Strip ANSI escape sequences so the summary is human-readable.
        if let Some(ref mut pending) = self.pending {
            if pending.entered {
                let stripped = strip_ansi(data);
                let text = String::from_utf8_lossy(&stripped);
                for line in text.split('\n') {
                    let trimmed = line.trim_end_matches('\r');
                    if !trimmed.is_empty() {
                        pending.output_lines.push(trimmed.to_string());
                    }
                }
            }
        }

        // In osc133 mode, command boundaries come from OSC events, not regex.
        if self.osc133_mode {
            return Vec::new();
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
                    entered: false,
                    osc_command_line: None,
                    osc_cwd: None,
                });
                self.next_seq += 1;
            } else {
                // Subsequent prompt: finalize pending command, start new one
                if let Some(pending) = self.pending.take() {
                    completed.push(self.finalize_command(pending, timestamp_ms, stream_pos, None));
                }

                self.pending = Some(PendingCommand {
                    seq: self.next_seq,
                    started_at: timestamp_ms,
                    stream_offset: stream_pos,
                    input_buf: Vec::new(),
                    output_lines: Vec::new(),
                    entered: false,
                    osc_command_line: None,
                    osc_cwd: None,
                });
                self.next_seq += 1;
            }
        }

        completed
    }

    pub fn feed_osc133(
        &mut self,
        event: Osc133Event,
        timestamp_ms: u64,
        stream_pos: u64,
    ) -> Vec<CommandRecord> {
        self.osc133_mode = true;
        let mut completed = Vec::new();

        match event.kind {
            Osc133EventKind::PromptStart => {
                // Finalize previous command if pending and entered
                if let Some(pending) = self.pending.take() {
                    if pending.entered {
                        completed.push(self.finalize_command(pending, timestamp_ms, stream_pos, None));
                    }
                }
                self.seen_first_prompt = true;
                self.pending = Some(PendingCommand {
                    seq: self.next_seq,
                    started_at: timestamp_ms,
                    stream_offset: stream_pos,
                    input_buf: Vec::new(),
                    output_lines: Vec::new(),
                    entered: false,
                    osc_command_line: None,
                    osc_cwd: None,
                });
                self.next_seq += 1;
            }
            Osc133EventKind::CommandStart { command, cwd } => {
                if let Some(ref mut pending) = self.pending {
                    pending.entered = true;
                    pending.osc_command_line = command;
                    pending.osc_cwd = cwd;
                }
            }
            Osc133EventKind::OutputStart => {
                // No special action — output collection happens via feed_output_raw
            }
            Osc133EventKind::CommandEnd { exit_code } => {
                if let Some(pending) = self.pending.take() {
                    completed.push(self.finalize_command(pending, timestamp_ms, stream_pos, Some(exit_code)));
                }
            }
        }

        completed
    }

    /// Feed raw output bytes for summary collection only (no prompt detection).
    /// Use this in osc133 mode where command boundaries come from OSC events.
    pub fn feed_output_raw(&mut self, data: &[u8], _timestamp_ms: u64, _stream_pos: u64) {
        if let Some(ref mut pending) = self.pending {
            if pending.entered {
                let stripped = strip_ansi(data);
                let text = String::from_utf8_lossy(&stripped);
                for line in text.split('\n') {
                    let trimmed = line.trim_end_matches('\r');
                    if !trimmed.is_empty() {
                        pending.output_lines.push(trimmed.to_string());
                    }
                }
            }
        }
    }
}

fn extract_command_line(input: &[u8]) -> Option<String> {
    // Replay editing: process backspace (0x7f, 0x08) and Ctrl-U (0x15) on raw
    // bytes before the first \r/\n to reconstruct the actual command line.
    // ESC sequences (e.g. arrow keys: ESC [ A) are skipped entirely.
    let mut line_bytes: Vec<u8> = Vec::new();
    let mut esc_state: u8 = 0; // 0=normal, 1=saw ESC, 2=in CSI params
    for &b in input {
        if b == b'\r' || b == b'\n' {
            break;
        }
        match esc_state {
            1 => {
                // After ESC: if '[' start CSI, otherwise single-char escape — skip both
                if b == b'[' {
                    esc_state = 2;
                } else {
                    esc_state = 0; // ESC + one char (e.g. ESC O A) — done
                }
                continue;
            }
            2 => {
                // Inside CSI: consume parameter bytes (0x20..=0x3f) and
                // terminate on a letter (0x40..=0x7e)
                if (0x40..=0x7e).contains(&b) {
                    esc_state = 0; // final byte — sequence complete
                }
                continue;
            }
            _ => {}
        }
        if b == 0x1b {
            esc_state = 1;
            continue;
        }
        if b == 0x7f || b == 0x08 {
            line_bytes.pop();
        } else if b == 0x15 {
            line_bytes.clear();
        } else if b >= 0x20 {
            line_bytes.push(b);
        }
    }
    let text = String::from_utf8_lossy(&line_bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
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

    // -----------------------------------------------------------------------
    // Bug reproduction tests — based on real terminal data from omnish-commands
    // -----------------------------------------------------------------------

    /// Bug 1: extract_command_line includes interactive keystrokes after \r.
    /// Real data: user types "top\r" then presses "m11q" inside top.
    /// Rust's lines() does NOT split on bare \r, so command_line becomes
    /// "top\rm11q" instead of just "top".
    #[test]
    fn test_bug_command_line_includes_interactive_keystrokes() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"$ ", 1000, 0);

        // User types "top" + Enter, then interactive keys inside top
        tracker.feed_input(b"top\r", 1001);
        tracker.feed_input(b"m11q", 1002); // top interactive: toggle memory, press 1 twice, quit

        // top output + next prompt
        let cmds = tracker.feed_output(b"top - 14:50:20\r\nTasks: 300\r\n$ ", 1003, 100);
        assert_eq!(cmds.len(), 1);
        assert_eq!(
            cmds[0].command_line.as_deref(),
            Some("top"),
            "command_line should be 'top', not include interactive keystrokes after \\r"
        );
    }

    /// Bug 2: output_summary contains shell echo of individual input characters.
    /// Real terminal: when user types "ls", shell echoes "l" then "s" as separate
    /// output chunks BEFORE the actual command output. These single-char echo lines
    /// pollute the summary.
    #[test]
    fn test_bug_output_summary_includes_input_echo() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"$ ", 1000, 0);

        // Simulate character-by-character echo (real PTY behavior)
        tracker.feed_input(b"l", 1001);
        tracker.feed_output(b"l", 1001, 10);   // shell echoes 'l'
        tracker.feed_input(b"s", 1002);
        tracker.feed_output(b"s", 1002, 20);   // shell echoes 's'
        tracker.feed_input(b"\r", 1003);
        tracker.feed_output(b"\r\n", 1003, 30); // echo of Enter

        // Actual command output + next prompt
        let cmds = tracker.feed_output(
            b"\x1b[?2004l\r\nCargo.lock  Cargo.toml  src/\r\n\x1b[?2004h$ ",
            1004, 50,
        );
        assert_eq!(cmds.len(), 1);
        let summary = &cmds[0].output_summary;
        assert!(
            !summary.starts_with("l\ns"),
            "summary should NOT start with echoed input chars, got: {:?}",
            &summary[..summary.len().min(20)]
        );
        assert!(
            summary.contains("Cargo.lock"),
            "summary should contain actual command output"
        );
    }

    /// Bug 3: output_summary contains raw ANSI escape sequences.
    /// Real data shows \x1b[?2004l, \x1b[01;34m, etc. in the summary.
    /// The summary sent to LLM catalog should be human-readable.
    #[test]
    fn test_bug_output_summary_contains_ansi() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"$ ", 1000, 0);
        tracker.feed_input(b"ls\r\n", 1001);

        // Output with ANSI color codes (typical ls --color output)
        let cmds = tracker.feed_output(
            b"\x1b[?2004l\r\n\x1b[0m\x1b[01;34mconfig\x1b[0m  \x1b[01;34mcrates\x1b[0m  README.md\r\n\x1b[?2004h$ ",
            1002, 100,
        );
        assert_eq!(cmds.len(), 1);
        let summary = &cmds[0].output_summary;
        assert!(
            !summary.contains("\x1b["),
            "summary should NOT contain raw ANSI escapes, got: {:?}",
            summary
        );
        assert!(
            summary.contains("config"),
            "summary should contain the actual text"
        );
        assert!(
            summary.contains("README.md"),
            "summary should contain the actual text"
        );
    }

    /// Combined bug: realistic "ls" session with all three issues.
    /// Mirrors the actual b7bf1aac:0 data from production.
    #[test]
    fn test_bug_realistic_ls_session() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"\x1b[01;32mhuan@fortress\x1b[00m:\x1b[01;34m~/project\x1b[00m$ ", 1000, 0);

        // Character-by-character input with echo
        tracker.feed_input(b"l", 1001);
        tracker.feed_output(b"l", 1001, 50);
        tracker.feed_input(b"s", 1002);
        tracker.feed_output(b"s", 1002, 60);
        tracker.feed_input(b"\r", 1003);
        let cmds = tracker.feed_output(
            b"\r\n\x1b[?2004l\r\nCargo.lock  Cargo.toml  \x1b[0m\x1b[01;34mconfig\x1b[0m  \x1b[01;34mcrates\x1b[0m\r\n\x1b[?2004h\x1b[01;32mhuan@fortress\x1b[00m:\x1b[01;34m~/project\x1b[00m$ ",
            1004, 70,
        );
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].command_line.as_deref(), Some("ls"));

        let summary = &cmds[0].output_summary;
        // Should NOT start with echoed "l" and "s"
        assert!(!summary.starts_with("l\n"), "summary should not start with echoed chars");
        // Should NOT contain ANSI
        assert!(!summary.contains("\x1b["), "summary should not contain ANSI escapes");
        // Should contain actual file listing
        assert!(summary.contains("Cargo.lock"), "summary should contain ls output");
    }

    /// Regression: when user accepts a ghost-text completion (e.g. types "git s"
    /// then Tab injects "tatus"), the injected suffix must be fed to the tracker
    /// so command_line records "git status", not "git s".
    #[test]
    fn test_completion_suffix_included_in_command_line() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"$ ", 1000, 0);

        // User types "git s"
        tracker.feed_input(b"git s", 1001);
        // Tab completion injects "tatus" (written to PTY by client)
        tracker.feed_input(b"tatus", 1002);
        // User presses Enter
        tracker.feed_input(b"\r", 1003);

        let cmds = tracker.feed_output(b"On branch master\r\n$ ", 1004, 100);
        assert_eq!(cmds.len(), 1);
        assert_eq!(
            cmds[0].command_line.as_deref(),
            Some("git status"),
            "command_line should include the completion suffix"
        );
    }

    /// Regression: typing "vim", backspacing all 3 chars, then typing "ls"
    /// was recorded as "vim ls" because backspace (0x7f) wasn't handled.
    #[test]
    fn test_backspace_edits_command_line() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"$ ", 1000, 0);

        // User types "vim", backspaces 3 times, types "ls", Enter
        tracker.feed_input(b"vim\x7f\x7f\x7fls\r", 1001);

        let cmds = tracker.feed_output(b"file.txt\r\n$ ", 1002, 100);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].command_line.as_deref(), Some("ls"));
    }

    #[test]
    fn test_ctrl_u_clears_line() {
        let mut tracker = make_tracker();
        tracker.feed_output(b"$ ", 1000, 0);

        // User types "wrong-cmd", Ctrl-U, "ls", Enter
        tracker.feed_input(b"wrong-cmd\x15ls\r", 1001);

        let cmds = tracker.feed_output(b"file.txt\r\n$ ", 1002, 100);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].command_line.as_deref(), Some("ls"));
    }

    // --- OSC 133 mode tests ---

    #[test]
    fn test_osc133_command_line_from_preexec() {
        use crate::osc133_detector::*;
        let mut tracker = make_tracker();

        tracker.feed_osc133(
            Osc133Event { kind: Osc133EventKind::PromptStart, start: 0, end: 8 },
            1000, 0,
        );
        // B payload carries the command from $BASH_COMMAND
        tracker.feed_osc133(
            Osc133Event {
                kind: Osc133EventKind::CommandStart { command: Some("echo hello".into()), cwd: None },
                start: 0, end: 20,
            },
            1001, 50,
        );
        // User input has arrow-key garbage (simulating history navigation)
        tracker.feed_input(b"\x1b[A\r", 1001);
        tracker.feed_osc133(
            Osc133Event { kind: Osc133EventKind::OutputStart, start: 0, end: 8 },
            1002, 60,
        );
        tracker.feed_output_raw(b"hello\r\n", 1002, 70);

        let cmds = tracker.feed_osc133(
            Osc133Event { kind: Osc133EventKind::CommandEnd { exit_code: 0 }, start: 0, end: 10 },
            1003, 100,
        );
        assert_eq!(cmds.len(), 1);
        assert_eq!(
            cmds[0].command_line.as_deref(),
            Some("echo hello"),
            "osc_command_line from B payload should be preferred over extract_command_line"
        );
    }

    #[test]
    fn test_esc_skipped_in_extract() {
        // Arrow up sends ESC [ A — extract_command_line should skip the whole sequence
        let result = extract_command_line(b"\x1b[A\r");
        assert_eq!(result, None, "ESC sequences should not produce command text");

        // ESC [ A followed by real input
        let result2 = extract_command_line(b"\x1b[Als\r");
        assert_eq!(result2, Some("ls".into()), "real input after ESC sequence should be kept");
    }

    #[test]
    fn test_osc133_simple_command() {
        use crate::osc133_detector::*;
        let mut tracker = make_tracker();

        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::PromptStart, start: 0, end: 8 }, 1000, 0);
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::CommandStart { command: None, cwd: None }, start: 0, end: 8 }, 1001, 50);
        tracker.feed_input(b"ls\r", 1001);
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::OutputStart, start: 0, end: 8 }, 1002, 60);
        tracker.feed_output_raw(b"file.txt\r\n", 1002, 70);

        let cmds = tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::CommandEnd { exit_code: 0 }, start: 0, end: 10 }, 1003, 100);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].command_line.as_deref(), Some("ls"));
        assert_eq!(cmds[0].exit_code, Some(0));
        assert_eq!(cmds[0].started_at, 1000);
        assert_eq!(cmds[0].ended_at, Some(1003));
    }

    #[test]
    fn test_osc133_nonzero_exit() {
        use crate::osc133_detector::*;
        let mut tracker = make_tracker();

        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::PromptStart, start: 0, end: 8 }, 1000, 0);
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::CommandStart { command: None, cwd: None }, start: 0, end: 8 }, 1001, 50);
        tracker.feed_input(b"false\r", 1001);
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::OutputStart, start: 0, end: 8 }, 1002, 60);
        let cmds = tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::CommandEnd { exit_code: 1 }, start: 0, end: 10 }, 1003, 100);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].exit_code, Some(1));
    }

    #[test]
    fn test_osc133_suppresses_regex_detection() {
        use crate::osc133_detector::*;
        let mut tracker = make_tracker();

        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::PromptStart, start: 0, end: 8 }, 1000, 0);
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::CommandStart { command: None, cwd: None }, start: 0, end: 8 }, 1001, 50);
        tracker.feed_input(b"echo $\r", 1001);
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::OutputStart, start: 0, end: 8 }, 1002, 60);

        // This output contains a prompt-like pattern
        let regex_cmds = tracker.feed_output(b"user@host:~$ \r\n", 1002, 70);
        assert!(regex_cmds.is_empty(), "regex should not fire in osc133 mode");

        let cmds = tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::CommandEnd { exit_code: 0 }, start: 0, end: 10 }, 1003, 100);
        assert_eq!(cmds.len(), 1);
    }

    #[test]
    fn test_osc133_multiple_commands() {
        use crate::osc133_detector::*;
        let mut tracker = make_tracker();

        // First command
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::PromptStart, start: 0, end: 8 }, 1000, 0);
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::CommandStart { command: None, cwd: None }, start: 0, end: 8 }, 1001, 50);
        tracker.feed_input(b"ls\r", 1001);
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::OutputStart, start: 0, end: 8 }, 1002, 60);
        let cmds1 = tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::CommandEnd { exit_code: 0 }, start: 0, end: 10 }, 1003, 100);
        assert_eq!(cmds1.len(), 1);
        assert_eq!(cmds1[0].command_id, "sess1:0");

        // Second command
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::PromptStart, start: 0, end: 8 }, 1004, 100);
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::CommandStart { command: None, cwd: None }, start: 0, end: 8 }, 1005, 150);
        tracker.feed_input(b"pwd\r", 1005);
        tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::OutputStart, start: 0, end: 8 }, 1006, 160);
        let cmds2 = tracker.feed_osc133(Osc133Event { kind: Osc133EventKind::CommandEnd { exit_code: 0 }, start: 0, end: 10 }, 1007, 200);
        assert_eq!(cmds2.len(), 1);
        assert_eq!(cmds2[0].command_id, "sess1:1");
    }

    #[test]
    fn test_cwd_from_osc133_overrides_session_cwd() {
        use crate::osc133_detector::*;
        let mut tracker = CommandTracker::new(
            "sess1".into(),
            Some("/initial/cwd".into()), // Session cwd
        );

        tracker.feed_osc133(
            Osc133Event { kind: Osc133EventKind::PromptStart, start: 0, end: 8 },
            1000, 0,
        );

        tracker.feed_osc133(
            Osc133Event {
                kind: Osc133EventKind::CommandStart {
                    command: Some("ls".into()),
                    cwd: Some("/runtime/cwd".into())
                },
                start: 0, end: 20,
            },
            1001, 50,
        );

        tracker.feed_input(b"\r", 1001);

        let cmds = tracker.feed_osc133(
            Osc133Event { kind: Osc133EventKind::CommandEnd { exit_code: 0 }, start: 0, end: 10 },
            1003, 100,
        );

        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].cwd.as_deref(), Some("/runtime/cwd")); // Should use runtime cwd
    }
}
