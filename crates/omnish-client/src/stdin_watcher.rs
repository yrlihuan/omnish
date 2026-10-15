//! Background stdin watcher used while the chat waits on the daemon: Ctrl-C
//! interrupts, Ctrl-O (when enabled) asks to browse scroll history, and any
//! other byte typed meanwhile is discarded.
//!
//! The watcher must be the only reader of stdin while it runs. It used to
//! poll stdin with a 100ms timeout and check an mpsc stop flag in between,
//! and the input loop started reading as soon as the stop was sent. When a
//! key arrived right at that moment, both saw stdin readable; the input loop
//! took the bytes and the watcher's `read` then blocked on the empty blocking
//! fd. It stayed blocked past the stop until the user's next key, and threw
//! that key away (issue #696: "hello world" arrived as "ello world").
//!
//! Now the watcher polls stdin together with a stop pipe and no timeout, so a
//! stop wakes it at once without touching stdin, and `stop()` waits for the
//! thread to exit before returning. Callers read stdin only after `stop()`,
//! so the watcher only reads when poll reports data that nobody else can take
//! and never blocks in `read`.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};

/// Why the watcher returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdinSignal {
    /// Ctrl-C: the user wants to interrupt.
    CtrlC,
    /// Ctrl-O (only when `watch_browse`): the user wants to browse scroll
    /// history. The watcher has stopped reading so the caller can run the
    /// blocking browser.
    Browse,
    /// Told to stop, dropped, or stdin hit EOF / an error.
    Stopped,
}

/// A running watcher. Call [`StdinWatcher::stop`] before reading stdin again.
/// Dropping it also ends the thread (the closed pipe wakes it), but without
/// waiting for it.
pub struct StdinWatcher {
    stop_w: Option<OwnedFd>,
    /// Kept open so writing the stop byte never hits a pipe without readers
    /// (EPIPE / SIGPIPE) after the thread has returned and closed its copy.
    _stop_r: Option<OwnedFd>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl StdinWatcher {
    /// Watch the process stdin on a blocking thread and pass the outcome to
    /// `on_signal` when it returns.
    pub fn spawn(watch_browse: bool, on_signal: impl FnOnce(StdinSignal) + Send + 'static) -> Self {
        Self::spawn_on(std::io::stdin().as_raw_fd(), watch_browse, on_signal)
    }

    fn spawn_on(
        fd: RawFd,
        watch_browse: bool,
        on_signal: impl FnOnce(StdinSignal) + Send + 'static,
    ) -> Self {
        let pipe = (|| -> std::io::Result<_> {
            let (r, w) = nix::unistd::pipe()?;
            let thread_r = r.try_clone()?;
            Ok((r, thread_r, w))
        })();
        let (stop_r, thread_r, stop_w) = match pipe {
            Ok(p) => p,
            Err(e) => {
                // No pipe (fd exhaustion): do not watch at all rather than
                // risk an unstoppable reader. Ctrl-C is then unavailable for
                // this wait only.
                crate::event_log::push(format!("stdin_watcher: pipe failed: {e}"));
                return Self { stop_w: None, _stop_r: None, handle: None };
            }
        };
        let handle = tokio::task::spawn_blocking(move || {
            let signal = watch(fd, thread_r.as_raw_fd(), watch_browse);
            drop(thread_r);
            on_signal(signal);
        });
        Self { stop_w: Some(stop_w), _stop_r: Some(stop_r), handle: Some(handle) }
    }

    /// Stop the watcher and wait until its thread has exited, so stdin has
    /// no other reader when this returns. Returns at once if the watcher
    /// already returned by itself (Ctrl-C / Ctrl-O / EOF).
    pub async fn stop(mut self) {
        if let Some(w) = self.stop_w.take() {
            let _ = nix::unistd::write(&w, b"x");
        }
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
    }
}

/// The watch loop: block in poll on `fd` and the stop pipe; a stop always
/// wins over pending input.
fn watch(fd: RawFd, stop_fd: RawFd, watch_browse: bool) -> StdinSignal {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    crate::event_log::push(format!("stdin_watcher[{}]: enter", id));

    let mut byte = [0u8; 1];
    let mut eaten: u64 = 0;
    let (signal, reason) = loop {
        let mut fds = [
            libc::pollfd { fd: stop_fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd, events: libc::POLLIN, revents: 0 },
        ];
        let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ret < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break (StdinSignal::Stopped, "poll-err");
        }
        // Written byte, or the write end closed (watcher dropped).
        if fds[0].revents != 0 {
            break (StdinSignal::Stopped, "stop");
        }
        if fds[1].revents & libc::POLLIN == 0 {
            if fds[1].revents != 0 {
                break (StdinSignal::Stopped, "stdin-hup");
            }
            continue;
        }
        match nix::unistd::read(fd, &mut byte) {
            Ok(1) if byte[0] == 0x03 => break (StdinSignal::CtrlC, "ctrl-c"),
            Ok(1) if watch_browse && byte[0] == 0x0f => break (StdinSignal::Browse, "ctrl-o"),
            Ok(1) => eaten += 1,
            _ => break (StdinSignal::Stopped, "read-err"),
        }
    };
    crate::event_log::push(format!(
        "stdin_watcher[{}]: exit reason={} ate={}",
        id, reason, eaten
    ));
    signal
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A pipe standing in for stdin: (read end for the watcher, write end).
    fn fake_stdin() -> (OwnedFd, OwnedFd) {
        nix::unistd::pipe().unwrap()
    }

    fn readable(fd: &OwnedFd) -> bool {
        let mut pfd = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        unsafe { libc::poll(&mut pfd, 1, 0) > 0 }
    }

    fn spawn(fd: &OwnedFd, browse: bool) -> (StdinWatcher, mpsc::Receiver<StdinSignal>) {
        let (tx, rx) = mpsc::channel();
        let w = StdinWatcher::spawn_on(fd.as_raw_fd(), browse, move |s| {
            let _ = tx.send(s);
        });
        (w, rx)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_returns_promptly_and_leaves_later_input_unread() {
        let (r, w) = fake_stdin();
        let (watcher, rx) = spawn(&r, true);
        tokio::time::sleep(Duration::from_millis(50)).await; // blocked in poll
        let started = std::time::Instant::now();
        watcher.stop().await;
        assert!(started.elapsed() < Duration::from_millis(500), "stop waited {:?}", started.elapsed());
        assert_eq!(rx.try_recv(), Ok(StdinSignal::Stopped), "thread finished before stop() returned");
        // The next key belongs to whoever reads stdin next, not the watcher.
        nix::unistd::write(&w, b"h").unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(readable(&r), "the byte typed after stop is still there");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ctrl_c_and_ctrl_o_are_reported() {
        let (r, w) = fake_stdin();
        let (watcher, rx) = spawn(&r, true);
        nix::unistd::write(&w, b"ab\x03").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(StdinSignal::CtrlC));
        watcher.stop().await; // already returned: no wait, no extra read
        assert!(!readable(&r), "bytes before Ctrl-C were discarded");

        let (watcher, rx) = spawn(&r, true);
        nix::unistd::write(&w, b"\x0f").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(StdinSignal::Browse));
        watcher.stop().await;

        // Without watch_browse, Ctrl-O is just discarded input.
        let (watcher, rx) = spawn(&r, false);
        nix::unistd::write(&w, b"\x0f").unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rx.try_recv().is_err());
        watcher.stop().await;
        assert_eq!(rx.try_recv(), Ok(StdinSignal::Stopped));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_watcher_ends_it() {
        let (r, _w) = fake_stdin();
        let (watcher, rx) = spawn(&r, true);
        drop(watcher);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(StdinSignal::Stopped));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stdin_eof_ends_it() {
        let (r, w) = fake_stdin();
        let (watcher, rx) = spawn(&r, true);
        drop(w);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(StdinSignal::Stopped));
        watcher.stop().await;
    }
}
