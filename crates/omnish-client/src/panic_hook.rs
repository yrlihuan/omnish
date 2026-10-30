//! Panic hook: record panics in client.log and, when the main thread panics
//! (which ends the client), restore the terminal and send SessionEnd.
//!
//! Without it a panic message only goes to stderr, i.e. the pane the client
//! runs in, and tmux closes that pane as soon as the process exits, so the
//! cause of a crash was lost.
//!
//! The hook writes to client.log through its own file handle instead of
//! `tracing`, and only `try_lock`s the event log, so a panic raised while
//! one of those locks is held does not deadlock the hook.

use omnish_protocol::message::{Message, SessionEnd};
use omnish_transport::rpc_client::RpcClient;
use std::cell::Cell;
use std::io::Write;
use std::os::fd::RawFd;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

thread_local! {
    /// Set while `catch` runs its closure on this thread.
    static CATCHING: Cell<bool> = const { Cell::new(false) };
}

/// Terminal settings from before raw mode, restored on a fatal panic.
static TERMINAL: OnceLock<(RawFd, libc::termios)> = OnceLock::new();

/// What a fatal panic needs to end the session on the daemon.
struct SessionInfo {
    runtime: tokio::runtime::Handle,
    rpc: RpcClient,
    session_id: String,
}

static SESSION: Mutex<Option<SessionInfo>> = Mutex::new(None);

/// Install the hook. Call once, early in `main`; the default hook still runs
/// afterwards so the message also reaches stderr.
pub fn install() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>").to_string();
        // Panics in tokio tasks and helper threads are caught there and the
        // client keeps running; only a main thread panic ends the process,
        // unless `catch` is about to recover from it.
        let caught = CATCHING.with(|c| c.get());
        let fatal = name == "main" && !caught;
        let backtrace = std::backtrace::Backtrace::force_capture();
        write_report(&format_report(
            &name,
            fatal,
            caught,
            &info.to_string(),
            &backtrace.to_string(),
            &crate::event_log::try_recent(200),
        ));
        if fatal {
            restore_terminal();
            send_session_end();
        }
        // A caught panic is recovered from: printing it would only scribble
        // over the user's terminal.
        if !caught {
            default_hook(info);
        }
    }));
}

/// Run `f`, turning a panic into `Err` so the caller can recover. The hook
/// still writes the report to client.log (marked `caught=true`) but does not
/// end the session or print to the terminal.
pub fn catch<R>(f: impl FnOnce() -> R) -> std::thread::Result<R> {
    let prev = CATCHING.with(|c| c.replace(true));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    CATCHING.with(|c| c.set(prev));
    result
}

/// Remember the terminal settings of `fd`. Call just before entering raw mode.
pub fn remember_terminal(fd: RawFd) {
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut t) } == 0 {
        let _ = TERMINAL.set((fd, t));
    }
}

/// Register the daemon connection so a fatal panic can send SessionEnd.
pub fn set_session(rpc: &RpcClient, session_id: &str) {
    if let Ok(mut s) = SESSION.lock() {
        *s = Some(SessionInfo {
            runtime: tokio::runtime::Handle::current(),
            rpc: rpc.clone(),
            session_id: session_id.to_string(),
        });
    }
}

fn format_report(
    thread: &str,
    fatal: bool,
    caught: bool,
    message: &str,
    backtrace: &str,
    events: &[String],
) -> String {
    let mut out = format!(
        "{} PANIC thread={} fatal={}{} version={}\n{}\nbacktrace:\n{}\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        thread,
        fatal,
        if caught { " caught=true" } else { "" },
        omnish_common::VERSION,
        message,
        backtrace.trim_end(),
    );
    if events.is_empty() {
        out.push_str("recent events: <unavailable>\n");
    } else {
        out.push_str(&format!("recent events ({}):\n", events.len()));
        for e in events {
            out.push_str("  ");
            out.push_str(e);
            out.push('\n');
        }
    }
    out.push_str("END PANIC\n");
    out
}

fn write_report(report: &str) {
    let path = omnish_common::config::omnish_dir().join("client.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(report.as_bytes());
        let _ = f.flush();
    }
}

fn restore_terminal() {
    if let Some((fd, t)) = TERMINAL.get() {
        unsafe { libc::tcsetattr(*fd, libc::TCSANOW, t) };
    }
}

/// Send SessionEnd from a helper thread: the panicking main thread is inside
/// the runtime and cannot block on it, but the worker threads still run the
/// connection's writer task. Bounded so a dead connection cannot hang exit.
fn send_session_end() {
    let Some(info) = SESSION.try_lock().ok().and_then(|mut s| s.take()) else {
        return;
    };
    let handle = std::thread::spawn(move || {
        info.runtime.block_on(async {
            let msg = Message::SessionEnd(SessionEnd {
                session_id: info.session_id.clone(),
                timestamp_ms: crate::timestamp_ms(),
                exit_code: None,
            });
            let _ = tokio::time::timeout(Duration::from_millis(500), info.rpc.send(msg)).await;
            // send() only queues the frame; give the writer time to flush it.
            tokio::time::sleep(Duration::from_millis(200)).await;
        });
    });
    let _ = handle.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_contains_message_backtrace_and_events() {
        let r = format_report(
            "main",
            true,
            false,
            "panicked at src/x.rs:1:2:\nboom",
            "   0: foo\n   1: bar\n",
            &["+    1.000 a".to_string(), "+    2.000 b".to_string()],
        );
        assert!(r.contains(" PANIC thread=main fatal=true version="));
        assert!(r.contains("panicked at src/x.rs:1:2:\nboom\n"));
        assert!(r.contains("backtrace:\n   0: foo\n   1: bar\n"));
        assert!(r.contains("recent events (2):\n  +    1.000 a\n  +    2.000 b\n"));
        assert!(r.ends_with("END PANIC\n"));
    }

    #[test]
    fn report_without_events() {
        let r = format_report("tokio-rt-worker", false, false, "boom", "", &[]);
        assert!(r.contains("thread=tokio-rt-worker fatal=false version="));
        assert!(r.contains("recent events: <unavailable>\n"));
    }

    #[test]
    fn report_marks_caught_panics() {
        let r = format_report("main", false, true, "boom", "", &[]);
        assert!(r.contains("thread=main fatal=false caught=true version="));
    }

    #[test]
    fn catch_returns_err_and_restores_flag() {
        let r = catch(|| -> u32 { panic!("inside catch") });
        assert!(r.is_err());
        assert!(!CATCHING.with(|c| c.get()), "flag cleared after catch");
        assert_eq!(catch(|| 7).unwrap(), 7);
    }
}
