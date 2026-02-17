// crates/omnish-client/src/main.rs
mod command;
mod display;
mod interceptor;
mod probe;

use anyhow::Result;
use interceptor::{InputInterceptor, InterceptAction, TimeGapGuard};
use omnish_protocol::message::*;
use omnish_pty::proxy::PtyProxy;
use omnish_pty::raw_mode::RawModeGuard;
use omnish_transport::traits::{Connection, Transport};
use omnish_transport::unix::UnixTransport;
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use uuid::Uuid;

fn timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn get_socket_path() -> String {
    std::env::var("OMNISH_SOCKET").unwrap_or_else(|_| {
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            format!("{}/omnish.sock", dir)
        } else {
            "/tmp/omnish.sock".to_string()
        }
    })
}

fn get_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    let session_id = Uuid::new_v4().to_string()[..8].to_string();
    let parent_session_id = std::env::var("OMNISH_SESSION_ID").ok();
    let shell = get_shell();

    // Spawn PTY with shell, passing our session_id so nested omnish can detect parent
    let mut child_env = HashMap::new();
    child_env.insert("OMNISH_SESSION_ID".to_string(), session_id.clone());
    let proxy = PtyProxy::spawn_with_env(&shell, &[], child_env)?;

    // Connect to daemon (graceful degradation)
    let daemon_conn = connect_daemon(&session_id, parent_session_id, proxy.child_pid() as u32).await;

    // Enter raw mode
    let _raw_guard = RawModeGuard::enter(std::io::stdin().as_raw_fd())?;

    // Sync initial window size
    if let Some((rows, cols)) = get_terminal_size() {
        proxy.set_window_size(rows, cols).ok();
    }

    // Install SIGWINCH handler
    let master_fd = proxy.master_raw_fd();
    setup_sigwinch(master_fd);

    // Main I/O loop using poll
    let mut input_buf = [0u8; 4096];
    let mut output_buf = [0u8; 4096];
    let guard = TimeGapGuard::new(std::time::Duration::from_secs(1));
    let mut interceptor = InputInterceptor::new(":", Box::new(guard));
    let mut alt_screen_detector = AltScreenDetector::new();
    let mut col_tracker = CursorColTracker::new();
    let mut dismiss_col: u16 = 0;
    let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string());
    let mut command_tracker = omnish_tracker::command_tracker::CommandTracker::new(
        session_id.clone(), cwd,
    );

    loop {
        let mut fds = [
            libc::pollfd {
                fd: 0, // stdin
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: master_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];

        let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, 100) };
        if ret < 0 {
            continue;
        }

        // Stdin -> PTY master
        if fds[0].revents & libc::POLLIN != 0 {
            let n = nix::unistd::read(0, &mut input_buf)?;
            if n == 0 {
                break;
            }

            // Feed bytes to interceptor one by one
            for i in 0..n {
                let byte = input_buf[i];
                match interceptor.feed_byte(byte) {
                    InterceptAction::Buffering(buf) => {
                        if buf == b":" {
                            // Save cursor column before drawing omnish UI
                            dismiss_col = col_tracker.col;
                            let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
                            let prompt = display::render_prompt(cols);
                            nix::unistd::write(std::io::stdout(), prompt.as_bytes()).ok();
                        } else if buf.len() > 1 && buf.starts_with(b":") {
                            // Echo the user's input after the prompt
                            let user_input = &buf[1..]; // Skip ":"
                            let echo = display::render_input_echo(user_input);
                            nix::unistd::write(std::io::stdout(), echo.as_bytes()).ok();
                        }
                    }
                    InterceptAction::Backspace(buf) => {
                        if buf.is_empty() {
                            // Backspaced past the prefix — clear omnish UI, restore cursor column
                            let dismiss = display::render_dismiss();
                            let restore = format!("\x1b[{}G", dismiss_col + 1);
                            nix::unistd::write(std::io::stdout(), dismiss.as_bytes()).ok();
                            nix::unistd::write(std::io::stdout(), restore.as_bytes()).ok();
                        } else if buf.starts_with(b":") {
                            if buf.len() == 1 {
                                // Only prefix char left — redraw ❯ with no input text
                                let echo = display::render_input_echo(b"");
                                nix::unistd::write(std::io::stdout(), echo.as_bytes()).ok();
                            } else {
                                // Show the user's input after the prompt
                                let user_input = &buf[1..]; // Skip ":"
                                let echo = display::render_input_echo(user_input);
                                nix::unistd::write(std::io::stdout(), echo.as_bytes()).ok();
                            }
                        }
                    }
                    InterceptAction::Forward(bytes) => {
                        // Forward these bytes to PTY
                        proxy.write_all(&bytes)?;

                        // Feed input to command tracker
                        command_tracker.feed_input(&bytes, timestamp_ms());

                        // Report to daemon async
                        if let Some(ref conn) = daemon_conn {
                            let msg = Message::IoData(IoData {
                                session_id: session_id.clone(),
                                direction: IoDirection::Input,
                                timestamp_ms: timestamp_ms(),
                                data: bytes,
                            });
                            let _ = conn.send(&msg).await;
                        }
                    }
                    InterceptAction::Cancel => {
                        // ESC pressed — clear omnish UI, restore cursor column
                        let dismiss = display::render_dismiss();
                        let restore = format!("\x1b[{}G", dismiss_col + 1); // CHA is 1-indexed
                        nix::unistd::write(std::io::stdout(), dismiss.as_bytes()).ok();
                        nix::unistd::write(std::io::stdout(), restore.as_bytes()).ok();
                    }
                    InterceptAction::Chat(msg) => {
                        match command::dispatch(&msg) {
                            command::ChatAction::Command { result, redirect } => {
                                handle_command_result(&result, redirect.as_deref(), &proxy);
                            }
                            command::ChatAction::DaemonDebug { query, redirect } => {
                                if let Some(ref conn) = daemon_conn {
                                    send_daemon_query(&query, &session_id, conn, &proxy, redirect.as_deref(), false).await;
                                } else {
                                    let err = display::render_error("Daemon not connected");
                                    nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                                    proxy.write_all(b"\r").ok();
                                }
                            }
                            command::ChatAction::LlmQuery(query) => {
                                if let Some(ref conn) = daemon_conn {
                                    send_daemon_query(&query, &session_id, conn, &proxy, None, true).await;
                                } else {
                                    let err = display::render_error("Daemon not connected");
                                    nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                                    proxy.write_all(b"\r").ok();
                                }
                            }
                        }
                    }
                    InterceptAction::Pending => {
                        // ESC sequence in progress — no UI update needed
                    }
                }
            }

            // After processing all bytes from this read(), check for bare ESC
            if let Some(action) = interceptor.finish_batch() {
                if matches!(action, InterceptAction::Cancel) {
                    let dismiss = display::render_dismiss();
                    let restore = format!("\x1b[{}G", dismiss_col + 1);
                    nix::unistd::write(std::io::stdout(), dismiss.as_bytes()).ok();
                    nix::unistd::write(std::io::stdout(), restore.as_bytes()).ok();
                }
            }
        }

        // PTY master -> stdout
        if fds[1].revents & libc::POLLIN != 0 {
            match proxy.read(&mut output_buf) {
                Ok(0) => break,
                Ok(n) => {
                    nix::unistd::write(std::io::stdout(), &output_buf[..n])?;

                    // Track cursor column for dismiss restore
                    col_tracker.feed(&output_buf[..n]);

                    // Detect alternate screen transitions (vim, less, etc.)
                    if let Some(active) = alt_screen_detector.feed(&output_buf[..n]) {
                        interceptor.set_suppressed(active);
                    }

                    // Notify interceptor of output (resets chat state)
                    interceptor.note_output(&output_buf[..n]);

                    // Send IoData to daemon first (so stream is written before CommandComplete)
                    if let Some(ref conn) = daemon_conn {
                        let msg = Message::IoData(IoData {
                            session_id: session_id.clone(),
                            direction: IoDirection::Output,
                            timestamp_ms: timestamp_ms(),
                            data: output_buf[..n].to_vec(),
                        });
                        let _ = conn.send(&msg).await;
                    }

                    // Feed output to command tracker (after IoData sent)
                    let completed = command_tracker.feed_output(&output_buf[..n], timestamp_ms(), 0);
                    for record in &completed {
                        if let Some(ref conn) = daemon_conn {
                            let msg = Message::CommandComplete(omnish_protocol::message::CommandComplete {
                                session_id: session_id.clone(),
                                record: record.clone(),
                            });
                            let _ = conn.send(&msg).await;
                        }
                    }
                }
                Err(_) => break,
            }
        }

        // Check if PTY hung up
        if fds[1].revents & libc::POLLHUP != 0 {
            break;
        }
    }

    // Send session end
    if let Some(ref conn) = daemon_conn {
        let msg = Message::SessionEnd(SessionEnd {
            session_id: session_id.clone(),
            timestamp_ms: timestamp_ms(),
            exit_code: None,
        });
        let _ = conn.send(&msg).await;
    }

    // Drop raw mode guard BEFORE process::exit, since exit() skips destructors
    drop(_raw_guard);

    let exit_code = proxy.wait().unwrap_or(1);
    std::process::exit(exit_code);
}

async fn connect_daemon(
    session_id: &str,
    parent_session_id: Option<String>,
    child_pid: u32,
) -> Option<Box<dyn Connection>> {
    let socket_path = get_socket_path();
    let transport = UnixTransport;
    match transport.connect(&socket_path).await {
        Ok(conn) => {
            let attrs = probe::default_session_probes(child_pid).collect_all();
            let msg = Message::SessionStart(SessionStart {
                session_id: session_id.to_string(),
                parent_session_id,
                timestamp_ms: timestamp_ms(),
                attrs,
            });
            if conn.send(&msg).await.is_ok() {
                eprintln!("\x1b[32m[omnish]\x1b[0m Connected to daemon (session: {})", &session_id[..8]);
                Some(conn)
            } else {
                eprintln!("\x1b[33m[omnish]\x1b[0m Connected but failed to register session");
                None
            }
        }
        Err(e) => {
            eprintln!("\x1b[33m[omnish]\x1b[0m Daemon not available ({}), running in passthrough mode", e);
            eprintln!("\x1b[33m[omnish]\x1b[0m Socket: {}", socket_path);
            eprintln!("\x1b[33m[omnish]\x1b[0m To start daemon: omnish-daemon or cargo run -p omnish-daemon");
            None
        }
    }
}

fn get_terminal_size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 {
        Some((ws.ws_row, ws.ws_col))
    } else {
        None
    }
}

fn setup_sigwinch(master_fd: i32) {
    unsafe {
        MASTER_FD = master_fd;
        libc::signal(libc::SIGWINCH, sigwinch_handler as *const () as libc::sighandler_t);
    }
}

static mut MASTER_FD: i32 = -1;

/// Tracks cursor column from PTY output bytes.
///
/// Follows `\r` (reset to 0), printable ASCII, multi-byte UTF-8 characters,
/// and skips ANSI escape sequences (CSI, OSC) so they don't inflate the count.
/// CJK / fullwidth characters are counted as 2 columns using `unicode-width`.
/// Used to save/restore cursor column when dismissing the omnish UI.
struct CursorColTracker {
    col: u16,
    state: ColTrackState,
    /// Buffer for accumulating a multi-byte UTF-8 character.
    utf8_buf: [u8; 4],
    /// Number of bytes collected so far for the current UTF-8 character.
    utf8_len: u8,
    /// Expected total bytes for the current UTF-8 character.
    utf8_need: u8,
}

#[derive(Clone, Copy)]
enum ColTrackState {
    Normal,
    Esc,
    Csi,
    Osc,
}

impl CursorColTracker {
    fn new() -> Self {
        Self {
            col: 0,
            state: ColTrackState::Normal,
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
        }
    }

    fn feed(&mut self, data: &[u8]) {
        use unicode_width::UnicodeWidthChar;

        for &byte in data {
            // If we're accumulating a multi-byte UTF-8 character, collect continuation bytes.
            if self.utf8_need > 0 {
                if byte & 0xC0 == 0x80 {
                    // Continuation byte
                    self.utf8_buf[self.utf8_len as usize] = byte;
                    self.utf8_len += 1;
                    if self.utf8_len == self.utf8_need {
                        // Complete character — decode and measure width
                        if let Ok(s) = std::str::from_utf8(&self.utf8_buf[..self.utf8_len as usize]) {
                            if let Some(ch) = s.chars().next() {
                                self.col += ch.width().unwrap_or(0) as u16;
                            }
                        }
                        self.utf8_need = 0;
                        self.utf8_len = 0;
                    }
                } else {
                    // Invalid continuation — discard partial and re-process this byte
                    self.utf8_need = 0;
                    self.utf8_len = 0;
                    self.process_normal(byte);
                }
                continue;
            }

            match self.state {
                ColTrackState::Normal => self.process_normal(byte),
                ColTrackState::Esc => {
                    self.state = match byte {
                        b'[' => ColTrackState::Csi,
                        b']' => ColTrackState::Osc,
                        _ => ColTrackState::Normal,
                    };
                }
                ColTrackState::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.state = ColTrackState::Normal;
                    }
                }
                ColTrackState::Osc => {
                    if byte == 0x07 {
                        self.state = ColTrackState::Normal;
                    } else if byte == 0x1b {
                        self.state = ColTrackState::Esc;
                    }
                }
            }
        }
    }

    fn process_normal(&mut self, byte: u8) {
        match byte {
            0x1b => self.state = ColTrackState::Esc,
            b'\r' => self.col = 0,
            b'\n' => {}
            0x08 => self.col = self.col.saturating_sub(1),
            0x20..=0x7e => self.col += 1,
            // UTF-8 start bytes — begin accumulation
            0xc0..=0xdf => {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_need = 2;
            }
            0xe0..=0xef => {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_need = 3;
            }
            0xf0..=0xf7 => {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_need = 4;
            }
            _ => {}
        }
    }
}

extern "C" fn sigwinch_handler(_sig: libc::c_int) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) == 0 {
            libc::ioctl(MASTER_FD, libc::TIOCSWINSZ, &ws);
        }
    }
}

/// Detects alternate screen enter/exit escape sequences in PTY output.
///
/// Full-screen programs (vim, less, htop, etc.) switch to the alternate screen
/// buffer via these CSI sequences:
///   - Enter: \x1b[?1049h or \x1b[?47h
///   - Exit:  \x1b[?1049l or \x1b[?47l
///
/// We scan output bytes with a small state machine to detect these transitions
/// without needing a full VTE parser.
struct AltScreenDetector {
    active: bool,
    /// Partial match buffer for escape sequence detection
    seq_buf: Vec<u8>,
}

impl AltScreenDetector {
    fn new() -> Self {
        Self {
            active: false,
            seq_buf: Vec::with_capacity(16),
        }
    }

    /// Feed output bytes and return Some(true/false) if alternate screen state changed.
    /// Returns None if no state change occurred.
    fn feed(&mut self, data: &[u8]) -> Option<bool> {
        let mut changed = false;

        for &byte in data {
            if byte == 0x1b {
                // Start of a new escape sequence
                self.seq_buf.clear();
                self.seq_buf.push(byte);
                continue;
            }

            if !self.seq_buf.is_empty() {
                self.seq_buf.push(byte);

                // We're looking for patterns like:
                //   \x1b [ ? 1049 h/l
                //   \x1b [ ? 47 h/l
                // Max length we care about: \x1b[?1049h = 9 bytes
                if self.seq_buf.len() > 10 {
                    // Too long, not a sequence we care about
                    self.seq_buf.clear();
                    continue;
                }

                // Check for terminal character (h or l)
                if byte == b'h' || byte == b'l' {
                    let s = &self.seq_buf;
                    let entering = byte == b'h';

                    // Check \x1b[?1049h/l
                    if s == b"\x1b[?1049h" || s == b"\x1b[?1049l"
                        || s == b"\x1b[?47h" || s == b"\x1b[?47l"
                    {
                        if self.active != entering {
                            self.active = entering;
                            changed = true;
                        }
                    }

                    self.seq_buf.clear();
                }

                // If we got a character that can't be part of our target sequences,
                // and it's not a digit, ?, or [, abort
                if byte != b'[' && byte != b'?' && !byte.is_ascii_digit()
                    && byte != b'h' && byte != b'l'
                {
                    self.seq_buf.clear();
                }
            }
        }

        if changed { Some(self.active) } else { None }
    }
}

/// Display a command result or write to file if redirected.
fn handle_command_result(content: &str, redirect: Option<&str>, proxy: &PtyProxy) {
    if let Some(path) = redirect {
        match std::fs::write(path, content) {
            Ok(_) => {
                let msg = display::render_response(&format!("Written to {}", path));
                nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
            }
            Err(e) => {
                let err = display::render_error(&format!("Write failed: {}", e));
                nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
            }
        }
    } else {
        let output = display::render_response(content);
        nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
    }
    proxy.write_all(b"\r").ok();
}

/// Send a query to the daemon and display the result.
///
/// If `redirect` is Some, the response is written to the given file path instead of stdout.
/// If `show_thinking` is true, a thinking spinner is shown while waiting for the response
/// and a separator is appended after.
async fn send_daemon_query(
    query: &str,
    session_id: &str,
    conn: &Box<dyn Connection>,
    proxy: &PtyProxy,
    redirect: Option<&str>,
    show_thinking: bool,
) {
    if show_thinking {
        let status = display::render_thinking();
        nix::unistd::write(std::io::stdout(), status.as_bytes()).ok();
    }

    let request_id = Uuid::new_v4().to_string()[..8].to_string();
    let request = Message::Request(Request {
        request_id: request_id.clone(),
        session_id: session_id.to_string(),
        query: query.to_string(),
        scope: RequestScope::AllSessions,
    });

    if conn.send(&request).await.is_err() {
        let err = display::render_error("Failed to send request");
        nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
        proxy.write_all(b"\r").ok();
        return;
    }

    match conn.recv().await {
        Ok(Message::Response(resp)) if resp.request_id == request_id => {
            if show_thinking {
                std::fs::write("/tmp/omnish_last_response.txt", &resp.content).ok();
            }
            handle_command_result(&resp.content, redirect, proxy);
            if show_thinking {
                let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
                let separator = display::render_separator(cols);
                let sep_line = format!("{}\r\n", separator);
                nix::unistd::write(std::io::stdout(), sep_line.as_bytes()).ok();
            }
        }
        _ => {
            let err = display::render_error("Failed to receive response");
            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
            proxy.write_all(b"\r").ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alt_screen_detect_1049h() {
        let mut d = AltScreenDetector::new();
        assert_eq!(d.feed(b"\x1b[?1049h"), Some(true));
        assert_eq!(d.feed(b"some output"), None);
        assert_eq!(d.feed(b"\x1b[?1049l"), Some(false));
    }

    #[test]
    fn test_alt_screen_detect_47h() {
        let mut d = AltScreenDetector::new();
        assert_eq!(d.feed(b"\x1b[?47h"), Some(true));
        assert_eq!(d.feed(b"\x1b[?47l"), Some(false));
    }

    #[test]
    fn test_alt_screen_no_duplicate_events() {
        let mut d = AltScreenDetector::new();
        assert_eq!(d.feed(b"\x1b[?1049h"), Some(true));
        // Already active, no change
        assert_eq!(d.feed(b"\x1b[?1049h"), None);
        assert_eq!(d.feed(b"\x1b[?1049l"), Some(false));
        // Already inactive, no change
        assert_eq!(d.feed(b"\x1b[?1049l"), None);
    }

    #[test]
    fn test_alt_screen_embedded_in_output() {
        let mut d = AltScreenDetector::new();
        // Sequence embedded in larger output (like vim startup)
        let data = b"some preamble\x1b[?1049hmore stuff";
        assert_eq!(d.feed(data), Some(true));
    }

    #[test]
    fn test_alt_screen_split_across_chunks() {
        let mut d = AltScreenDetector::new();
        // Escape sequence split across two read() calls
        assert_eq!(d.feed(b"\x1b[?104"), None);
        assert_eq!(d.feed(b"9h"), Some(true));
    }

    #[test]
    fn test_alt_screen_ignores_unrelated_sequences() {
        let mut d = AltScreenDetector::new();
        // Other CSI sequences should not trigger
        assert_eq!(d.feed(b"\x1b[?25h"), None); // show cursor
        assert_eq!(d.feed(b"\x1b[?25l"), None); // hide cursor
        assert_eq!(d.feed(b"\x1b[2J"), None);   // clear screen
        assert_eq!(d.active, false);
    }

    #[test]
    fn test_alt_screen_integration_with_interceptor() {
        use interceptor::AlwaysIntercept;

        let mut interceptor = InputInterceptor::new(":", Box::new(AlwaysIntercept));
        let mut detector = AltScreenDetector::new();

        // Normal mode: interceptor should buffer ":"
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));

        // Reset for clean test
        interceptor.note_output(b"reset");

        // vim opens: alternate screen enter
        if let Some(active) = detector.feed(b"\x1b[?1049h") {
            interceptor.set_suppressed(active);
        }

        // Now ":" should forward directly
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Forward(vec![b':']));

        // vim exits: alternate screen leave
        if let Some(active) = detector.feed(b"\x1b[?1049l") {
            interceptor.set_suppressed(active);
        }

        // Back to normal: ":" should intercept again
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
    }

    // --- CursorColTracker tests ---

    #[test]
    fn test_col_tracker_ascii() {
        let mut t = CursorColTracker::new();
        t.feed(b"hello");
        assert_eq!(t.col, 5);

        // \r resets column
        t.feed(b"\rworld");
        assert_eq!(t.col, 5);

        // Backspace
        t.feed(b"\x08");
        assert_eq!(t.col, 4);
    }

    #[test]
    fn test_col_tracker_skips_csi() {
        let mut t = CursorColTracker::new();
        // Color escape sequences should not advance column
        t.feed(b"\x1b[32mgreen\x1b[0m");
        assert_eq!(t.col, 5); // only "green" counted
    }

    #[test]
    fn test_col_tracker_skips_osc() {
        let mut t = CursorColTracker::new();
        // OSC title sequence (invisible) then prompt
        t.feed(b"\x1b]0;my title\x07$ ");
        assert_eq!(t.col, 2); // only "$ " counted
    }

    #[test]
    fn test_col_tracker_typical_prompt() {
        let mut t = CursorColTracker::new();
        // Typical colored prompt: \r\n\x1b[32muser@host\x1b[0m:\x1b[34m~\x1b[0m$
        t.feed(b"\r\n\x1b[32muser@host\x1b[0m:\x1b[34m~\x1b[0m$ ");
        // "user@host" (9) + ":" (1) + "~" (1) + "$ " (2) = 13
        assert_eq!(t.col, 13);
    }

    #[test]
    fn test_col_tracker_cjk_wide_chars() {
        let mut t = CursorColTracker::new();
        // Chinese characters are fullwidth — each occupies 2 columns
        t.feed("你好".as_bytes());
        assert_eq!(t.col, 4); // 2 chars × 2 columns each

        // Mixed: CJK + ASCII
        t = CursorColTracker::new();
        t.feed("用户@主机:~$ ".as_bytes());
        // "用" (2) + "户" (2) + "@" (1) + "主" (2) + "机" (2) + ":" (1) + "~" (1) + "$ " (2) = 13
        assert_eq!(t.col, 13);
    }

    #[test]
    fn test_col_tracker_cjk_with_colors() {
        let mut t = CursorColTracker::new();
        // Colored prompt with CJK characters
        let prompt = format!(
            "\r\n\x1b[32m{}\x1b[0m:\x1b[34m~\x1b[0m$ ",
            "用户@主机"
        );
        t.feed(prompt.as_bytes());
        // "用户" (4) + "@" (1) + "主机" (4) + ":" (1) + "~" (1) + "$ " (2) = 13
        assert_eq!(t.col, 13);
    }

    #[test]
    fn test_col_tracker_emoji() {
        let mut t = CursorColTracker::new();
        // ❯ (U+276F) is narrow — width 1
        t.feed("❯ ".as_bytes());
        assert_eq!(t.col, 2); // ❯ (1) + space (1)

        // 🚀 (U+1F680) is a wide emoji — width 2
        t = CursorColTracker::new();
        t.feed("🚀x".as_bytes());
        assert_eq!(t.col, 3); // 🚀 (2) + x (1)
    }
}
