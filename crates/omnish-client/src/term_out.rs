// crates/omnish-client/src/term_out.rs
//
// Unified stdout writer with a single global lock.
//
// The client runs on a multi-threaded tokio runtime (worker_threads = 4) and
// writes terminal escape sequences to stdout from several places: the render
// path (chat redraw, spinner, prompt, ghost text), the background polling task
// (tmux/screen window title), and the ServerPush dispatcher (pushed thread
// titles). A single `write(2)` is atomic, but a *logical* escape sequence that
// is split across multiple writes can be interrupted by another thread's write.
// If a window-title sequence `\x1bk...\x1b\\` lands inside an in-flight CSI
// sequence, the embedded ESC truncates that CSI and corrupts the terminal.
//
// Routing every stdout write through `write_stdout` here serializes writes on a
// process-global mutex, so two writes can never interleave. Multi-segment
// sequences should be coalesced into one buffer and emitted with a single
// `write_stdout` call so the whole sequence is held as one locked unit.
//
// The lock is held only around the write loop (no user callbacks run while
// held) and `write_stdout` never calls back into itself, so there is no
// reentrancy / deadlock risk.

use nix::errno::Errno;
use std::sync::Mutex;

static STDOUT_LOCK: Mutex<()> = Mutex::new(());

/// Write a complete logical sequence to stdout, ignoring errors.
///
/// This is the funnel every stdout write should go through. Matches the
/// behaviour of the former `.ok()` call sites (write failures are dropped).
pub fn write_stdout(data: &[u8]) {
    let _ = write_stdout_res(data);
}

/// `&str` convenience wrapper around [`write_stdout`].
pub fn write_str(s: &str) {
    write_stdout(s.as_bytes());
}

/// Like [`write_stdout`] but returns the first write error.
///
/// Used by the PTY output path, which treats a broken stdout as fatal and
/// propagates the error to shut the client down.
pub fn write_stdout_res(data: &[u8]) -> nix::Result<()> {
    if data.is_empty() {
        return Ok(());
    }
    // Recover from a poisoned lock: a panic mid-write leaves the terminal in an
    // unknown state anyway, and refusing all further output is worse.
    let _guard = STDOUT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    write_all_with(data, |buf| nix::unistd::write(std::io::stdout(), buf))
}

/// Loop a write function until every byte of `data` is written.
///
/// Factored out so the partial-write / EINTR handling can be unit tested
/// without touching the real stdout fd.
fn write_all_with<F>(mut data: &[u8], mut write_fn: F) -> nix::Result<()>
where
    F: FnMut(&[u8]) -> nix::Result<usize>,
{
    while !data.is_empty() {
        match write_fn(data) {
            Ok(0) => break, // refuses to make progress; avoid spinning
            Ok(n) => data = &data[n.min(data.len())..],
            Err(Errno::EINTR) => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn empty_is_noop() {
        // Must not even attempt a write (and must not panic).
        let called = Cell::new(false);
        let r = write_all_with(b"", |_| {
            called.set(true);
            Ok(0)
        });
        assert!(r.is_ok());
        assert!(!called.get());
    }

    #[test]
    fn writes_all_bytes_in_one_shot() {
        let written = Cell::new(0usize);
        write_all_with(b"hello", |buf| {
            written.set(written.get() + buf.len());
            Ok(buf.len())
        })
        .unwrap();
        assert_eq!(written.get(), 5);
    }

    #[test]
    fn loops_until_partial_writes_complete() {
        // Accept only one byte per call: the loop must keep going until done.
        let total = Cell::new(0usize);
        let collected = std::cell::RefCell::new(Vec::new());
        write_all_with(b"abcd", |buf| {
            collected.borrow_mut().push(buf[0]);
            total.set(total.get() + 1);
            Ok(1)
        })
        .unwrap();
        assert_eq!(total.get(), 4);
        assert_eq!(&*collected.borrow(), b"abcd");
    }

    #[test]
    fn retries_on_eintr_without_losing_bytes() {
        // First call is interrupted, second writes everything.
        let calls = Cell::new(0u32);
        let seen = std::cell::RefCell::new(Vec::new());
        write_all_with(b"xy", |buf| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Err(Errno::EINTR)
            } else {
                seen.borrow_mut().extend_from_slice(buf);
                Ok(buf.len())
            }
        })
        .unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(&*seen.borrow(), b"xy");
    }

    #[test]
    fn propagates_non_eintr_error() {
        let r = write_all_with(b"z", |_| Err(Errno::EPIPE));
        assert_eq!(r, Err(Errno::EPIPE));
    }

    #[test]
    fn stops_on_zero_progress() {
        // Ok(0) means the sink refuses more data; bail instead of spinning.
        let calls = Cell::new(0u32);
        write_all_with(b"data", |_| {
            calls.set(calls.get() + 1);
            Ok(0)
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
    }
}
