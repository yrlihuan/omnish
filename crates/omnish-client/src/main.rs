// crates/omnish-client/src/main.rs
mod commands;
mod display;
mod interceptor;

use anyhow::Result;
use commands::{parse_command, OmnishCommand};
use interceptor::{InputInterceptor, InterceptAction, TimeGapGuard};
use omnish_protocol::message::*;
use omnish_pty::proxy::PtyProxy;
use omnish_pty::raw_mode::RawModeGuard;
use omnish_transport::traits::{Connection, Transport};
use omnish_transport::unix::UnixTransport;
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
    let shell = get_shell();

    // Spawn PTY with shell
    let proxy = PtyProxy::spawn(&shell, &[])?;

    // Connect to daemon (graceful degradation)
    let daemon_conn = connect_daemon(&session_id, &shell, proxy.child_pid() as u32).await;

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
                            // User just typed ":", show the prompt interface
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
                        // If we backspaced back to empty or partial prefix, might need to clear prompt
                        // For simplicity, just redraw if still showing command input
                        if !buf.is_empty() && buf.starts_with(b":") {
                            if buf.len() == 1 {
                                let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
                                let prompt = display::render_prompt(cols);
                                nix::unistd::write(std::io::stdout(), prompt.as_bytes()).ok();
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
                    InterceptAction::Chat(msg) => {
                        // Chat mode message — send to LLM with terminal context
                        if let Some(ref conn) = daemon_conn {
                            handle_omnish_query(&msg, &session_id, conn).await;
                        } else {
                            // No daemon connection, print error
                            let err = display::render_error("Daemon not connected");
                            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                        }
                    }
                }
            }
        }

        // PTY master -> stdout
        if fds[1].revents & libc::POLLIN != 0 {
            match proxy.read(&mut output_buf) {
                Ok(0) => break,
                Ok(n) => {
                    nix::unistd::write(std::io::stdout(), &output_buf[..n])?;

                    // Detect alternate screen transitions (vim, less, etc.)
                    if let Some(active) = alt_screen_detector.feed(&output_buf[..n]) {
                        interceptor.set_suppressed(active);
                    }

                    // Notify interceptor of output (resets chat state)
                    interceptor.note_output(&output_buf[..n]);

                    if let Some(ref conn) = daemon_conn {
                        let msg = Message::IoData(IoData {
                            session_id: session_id.clone(),
                            direction: IoDirection::Output,
                            timestamp_ms: timestamp_ms(),
                            data: output_buf[..n].to_vec(),
                        });
                        let _ = conn.send(&msg).await;
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
    shell: &str,
    pid: u32,
) -> Option<Box<dyn Connection>> {
    let socket_path = get_socket_path();
    let transport = UnixTransport;
    match transport.connect(&socket_path).await {
        Ok(conn) => {
            let tty = std::env::var("TTY").unwrap_or_default();
            let msg = Message::SessionStart(SessionStart {
                session_id: session_id.to_string(),
                shell: shell.to_string(),
                pid,
                tty,
                timestamp_ms: timestamp_ms(),
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

async fn handle_omnish_query(query: &str, session_id: &str, conn: &Box<dyn Connection>) {
    // Show thinking status
    let status = display::render_thinking();
    nix::unistd::write(std::io::stdout(), status.as_bytes()).ok();

    let request_id = Uuid::new_v4().to_string()[..8].to_string();
    let request = Message::Request(Request {
        request_id: request_id.clone(),
        session_id: session_id.to_string(),
        query: query.to_string(),
        scope: RequestScope::CurrentSession,
    });

    // Send request
    if conn.send(&request).await.is_err() {
        let err = display::render_error("Failed to send request");
        nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
        return;
    }

    // Wait for response
    match conn.recv().await {
        Ok(Message::Response(resp)) if resp.request_id == request_id => {
            // Debug: save raw response
            std::fs::write("/tmp/omnish_last_response.txt", &resp.content).ok();

            // Display response
            let output = display::render_response(&resp.content);
            nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();

            // Add separator after response
            let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
            let separator = display::render_separator(cols);
            let sep_line = format!("{}\r\n", separator);
            nix::unistd::write(std::io::stdout(), sep_line.as_bytes()).ok();
        }
        Ok(_) => {
            let err = display::render_error("Unexpected response");
            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
        }
        Err(_) => {
            let err = display::render_error("Failed to receive response");
            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
        }
    }
}

async fn handle_command(cmd_str: &str, session_id: &str, conn: &Box<dyn Connection>) {
    let cmd = parse_command(&format!("::{}", cmd_str), "::");

    match cmd {
        Some(OmnishCommand::Ask { flags, query }) => {
            // Show overlay at top with "thinking..." status
            let status_msg = format!(
                "\x1b[s\x1b[H\x1b[K\x1b[48;5;235m\x1b[36m ::{}\x1b[0m\x1b[48;5;235m \x1b[2m(thinking...)\x1b[0m\x1b[u",
                cmd_str
            );
            nix::unistd::write(std::io::stdout(), status_msg.as_bytes()).ok();

            let scope = if flags.all_sessions {
                RequestScope::AllSessions
            } else {
                RequestScope::CurrentSession
            };

            let request_id = Uuid::new_v4().to_string()[..8].to_string();
            let request = Message::Request(Request {
                request_id: request_id.clone(),
                session_id: session_id.to_string(),
                query: if query.is_empty() {
                    "Why did that fail?".to_string()
                } else {
                    query
                },
                scope,
            });

            // Send request
            if conn.send(&request).await.is_err() {
                // Clear top line and show error
                let err = "\x1b[s\x1b[H\x1b[K\x1b[48;5;235m\x1b[31m [omnish] Failed to send request\x1b[0m\x1b[u";
                nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                nix::unistd::write(std::io::stdout(), b"\x1b[s\x1b[H\x1b[K\x1b[u").ok();
                return;
            }

            // Wait for response
            match conn.recv().await {
                Ok(Message::Response(resp)) if resp.request_id == request_id => {
                    // Debug: save raw response
                    std::fs::write("/tmp/omnish_last_response.txt", &resp.content).ok();

                    // Convert line breaks for raw mode and trim lines
                    let content: String = resp.content
                        .lines()
                        .map(|line| line.trim_end())
                        .collect::<Vec<_>>()
                        .join("\r\n");

                    // Show response in scrollable overlay box at top
                    show_response_overlay(cmd_str, &content);
                }
                Ok(_) => {
                    let err = "\x1b[s\x1b[H\x1b[K\x1b[48;5;235m\x1b[31m [omnish] Unexpected response\x1b[0m\x1b[u";
                    nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    nix::unistd::write(std::io::stdout(), b"\x1b[s\x1b[H\x1b[K\x1b[u").ok();
                }
                Err(_) => {
                    let err = "\x1b[s\x1b[H\x1b[K\x1b[48;5;235m\x1b[31m [omnish] Failed to receive response\x1b[0m\x1b[u";
                    nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    nix::unistd::write(std::io::stdout(), b"\x1b[s\x1b[H\x1b[K\x1b[u").ok();
                }
            }
        }
        Some(OmnishCommand::Unknown(s)) => {
            let msg = format!("\x1b[s\x1b[H\x1b[K\x1b[48;5;235m\x1b[33m [omnish] Unknown command: {}\x1b[0m\x1b[u", s);
            nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            nix::unistd::write(std::io::stdout(), b"\x1b[s\x1b[H\x1b[K\x1b[u").ok();
        }
        _ => {
            let msg = "\x1b[s\x1b[H\x1b[K\x1b[48;5;235m\x1b[33m [omnish] Command not yet implemented\x1b[0m\x1b[u";
            nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            nix::unistd::write(std::io::stdout(), b"\x1b[s\x1b[H\x1b[K\x1b[u").ok();
        }
    }
}

fn show_response_overlay(cmd_str: &str, content: &str) {
    // Get terminal size
    let (rows, _cols) = get_terminal_size().unwrap_or((24, 80));

    // Use top 60% of screen for overlay (max 15 lines)
    let max_lines = std::cmp::min((rows as f32 * 0.6) as usize, 15);
    let lines: Vec<&str> = content.lines().take(max_lines).collect();

    // Build the overlay box
    let mut output = String::new();

    // Save cursor, clear top area
    output.push_str("\x1b[s");

    // Draw header line
    output.push_str("\x1b[H\x1b[K\x1b[48;5;235m\x1b[1;36m ┌─ omnish ─ ");
    output.push_str(cmd_str);
    output.push_str(" ─");
    output.push_str("\x1b[0m\r\n");

    // Draw content lines
    for line in &lines {
        output.push_str("\x1b[K\x1b[48;5;235m\x1b[32m │\x1b[0m\x1b[48;5;235m ");
        output.push_str(line);
        output.push_str("\x1b[0m\r\n");
    }

    // Draw footer
    output.push_str("\x1b[K\x1b[48;5;235m\x1b[2;32m └─ Press any key to close ─\x1b[0m\r\n");

    // Add one more blank line to separate from shell
    output.push_str("\x1b[K\r\n");

    // Restore cursor
    output.push_str("\x1b[u");

    nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();

    // Wait for any key press (we'll detect it in the main loop)
    // The overlay will stay until user types something
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
}
