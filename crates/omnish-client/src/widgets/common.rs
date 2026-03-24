// crates/omnish-client/src/widgets/common.rs
//
// Shared terminal utilities for picker and menu widgets.

/// Maximum number of items visible in a widget viewport.
pub const MAX_VISIBLE: usize = 10;

/// Get terminal width, fallback to 80.
pub fn terminal_cols() -> u16 {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 && ws.ws_col > 0 { ws.ws_col } else { 80 }
}

/// Separator line spanning `cols` columns (dim horizontal-rule characters).
pub fn render_separator(cols: u16) -> String {
    format!("\r\x1b[2m{}\x1b[0m", "\u{2500}".repeat(cols as usize))
}

/// Write raw bytes to stdout (for terminal escape sequences).
pub fn write_stdout(data: &[u8]) {
    nix::unistd::write(std::io::stdout(), data).ok();
}

/// Parse escape sequence after ESC byte.
/// Uses poll with 50ms timeout to distinguish bare ESC from arrow keys.
pub fn parse_esc_seq(stdin_fd: i32) -> Option<[u8; 2]> {
    let mut pfd = libc::pollfd {
        fd: stdin_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut pfd, 1, 50) };
    if ready <= 0 {
        return None;
    }
    let mut seq = [0u8; 2];
    if nix::unistd::read(stdin_fd, &mut seq[0..1]) != Ok(1) {
        return None;
    }
    if seq[0] == b'[' && nix::unistd::read(stdin_fd, &mut seq[1..2]) == Ok(1) {
        return Some(seq);
    }
    None
}

/// Strip ANSI escape sequences from a string for width measurement.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_esc = false;
    for ch in s.chars() {
        if in_esc {
            if ch.is_ascii_alphabetic() {
                in_esc = false;
            }
        } else if ch == '\x1b' {
            in_esc = true;
        } else {
            out.push(ch);
        }
    }
    out
}
