use std::os::unix::io::AsRawFd;

use super::common;
use super::line_editor::LineEditor;

/// Expected length in bytes of a UTF-8 sequence given its lead byte.
fn utf8_len(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead >> 5 == 0b110 {
        2
    } else if lead >> 4 == 0b1110 {
        3
    } else if lead >> 3 == 0b11110 {
        4
    } else {
        1 // invalid lead byte; consume one to make progress
    }
}

#[cfg(test)]
mod tests {
    use super::utf8_len;

    #[test]
    fn test_utf8_len() {
        assert_eq!(utf8_len(b'a'), 1); // ASCII
        // Multi-byte lead bytes (e.g. Chinese "查" = e6 9f a5).
        assert_eq!(utf8_len(0xe6), 3);
        assert_eq!(utf8_len(0xc3), 2); // 2-byte (e.g. "à")
        assert_eq!(utf8_len(0xf0), 4); // 4-byte (e.g. emoji)
        // Continuation byte alone: consume one to make progress.
        assert_eq!(utf8_len(0x9f), 1);
    }
}

fn redraw(title: &str, editor: &LineEditor) {
    let mut out = String::from("\r\x1b[K");
    out.push_str(title);
    out.push(' ');
    out.push_str(&editor.content());
    common::write_stdout(out.as_bytes());
}

/// Blocking single-line text prompt. Renders `title` then reads a line from
/// stdin, echoing keystrokes. Returns the trimmed input, or `None` on ESC,
/// Ctrl-C, or an empty submission. Assumes the terminal is already in raw mode
/// (as it is inside the chat loop). UTF-8 aware so non-ASCII descriptions work.
pub fn prompt_line(title: &str) -> Option<String> {
    let mut editor = LineEditor::new();
    redraw(title, &editor);

    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut buf = [0u8; 16];
    let mut pending: Vec<u8> = Vec::new();

    loop {
        let n = match nix::unistd::read(stdin_fd, &mut buf) {
            Ok(n) if n > 0 => n,
            _ => {
                common::write_stdout(b"\r\n");
                return None;
            }
        };
        pending.extend_from_slice(&buf[..n]);

        let mut i = 0;
        while i < pending.len() {
            let b = pending[i];
            match b {
                0x03 => {
                    // Ctrl-C -> cancel
                    common::write_stdout(b"\r\n");
                    return None;
                }
                0x1b => {
                    // ESC: distinguish a bare ESC (cancel) from an escape
                    // sequence (arrow keys etc., which we ignore).
                    if i + 1 < pending.len() && (pending[i + 1] == b'[' || pending[i + 1] == b'O') {
                        let mut j = i + 2;
                        while j < pending.len() && !(0x40..=0x7e).contains(&pending[j]) {
                            j += 1;
                        }
                        if j < pending.len() {
                            i = j + 1; // consumed the full sequence
                            continue;
                        }
                        break; // incomplete sequence, wait for more bytes
                    } else if i + 1 < pending.len() {
                        i += 1; // ESC + something else: skip the ESC, ignore
                        continue;
                    } else {
                        // Lone ESC at end of input -> cancel
                        common::write_stdout(b"\r\n");
                        return None;
                    }
                }
                b'\r' | b'\n' => {
                    common::write_stdout(b"\r\n");
                    let content = editor.content();
                    let trimmed = content.trim().to_string();
                    return if trimmed.is_empty() { None } else { Some(trimmed) };
                }
                0x7f | 0x08 => {
                    editor.delete_back();
                    i += 1;
                }
                _ if b < 0x20 => {
                    i += 1; // ignore other control bytes
                }
                _ => {
                    let len = utf8_len(b);
                    if i + len > pending.len() {
                        break; // incomplete UTF-8 sequence, wait for more bytes
                    }
                    if let Ok(s) = std::str::from_utf8(&pending[i..i + len]) {
                        if let Some(ch) = s.chars().next() {
                            editor.insert(ch);
                        }
                    }
                    i += len;
                }
            }
        }
        pending.drain(..i);
        redraw(title, &editor);
    }
}
